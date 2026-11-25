//! The editor's buffer (`.prd/12-editor.md` §7.1, §7.3; E11, E17, E35): a piece table over the
//! file's own bytes, so a byte not edited is never decoded or rewritten.
//!
//! - **Two buffers, each one identity for the session.** The [`Original`] is the file as read at
//!   F4 (§7.4), held where it was read and shared, never copied or changed. The add buffer holds
//!   every byte typed or pasted, already encoded (ED-enc), in blocks that are appended to and
//!   never reallocated, so a piece never moves. The document is the list of [`Piece`]s in a
//!   `Vec`, found by a scan (E11's first cut). A Save rebases nothing ([`Buffer::mark_saved`]);
//!   only a re-read ([`Buffer::reread`]) brings a new original, generation `k` + 1 (§9.5).
//! - **Lines end at the encoding's LF unit** (08 §7.3), never at a 0x0A byte inside another
//!   unit. Each buffer keeps its line starts as the count of LF units before every 4 KiB, so a
//!   split or a lookup costs a binary search and a scan of at most 4 KiB, and the index costs
//!   1/512 of the file whatever its lines. Line 0 starts at byte 0: the BOM is the caller's (§7.2).
//! - **A CR LF pair is never split** (E35): a boundary of an insert or a delete strictly between
//!   the CR unit and the LF unit moves past the LF; one before the CR stays. Undo and redo replay
//!   what was recorded, unguarded.
//! - **Undo** (§7.3): one stack of steps, each holding its changes as pieces (a 200 MB delete
//!   copies nothing), grouped as Turbo C++'s Group Undo ([`Group`]), with the caller's caret and
//!   block before and after ([`Edit`]); at most [`UNDO_STEPS`], the oldest going first. Undoing
//!   past a Save makes the document unsaved again, redoing back makes it saved.
//! - **Serialising** ([`Buffer::write_to`]) writes the pieces in order, as they are.
//!
//! Every offset and length handed in is a multiple of the encoding's unit (a character start,
//! 08 §7.3): the line index counts LF units at the unit's alignment. Pieces belong to the buffer
//! that made them.

use crate::encoding::Encoding;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::ops::{Deref, Range};
use std::sync::Arc;

/// Undo steps kept (§7.3, E17; 07 §11's bound for hex): past it the oldest go.
pub const UNDO_STEPS: usize = 100_000;

/// The line index's stride, a multiple of every unit, so no LF unit straddles two strides.
const STRIDE: usize = 4096;

/// A new add block's capacity: a longer insert gets a block of its own size.
const BLOCK: usize = 64 * 1024;

/// The LF units wholly in `bytes` (read from offset `at`), at the unit's alignment.
fn count_lf(encoding: Encoding, bytes: &[u8], at: u64) -> u64 {
    if encoding.unit() == 1 {
        return bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    }
    let (mut n, mut from) = (0, 0);
    while let Some(i) = encoding.find_lf(&bytes[from..], at + from as u64) {
        n += 1;
        from += i + encoding.lf().len();
    }
    n
}

/// A buffer's line starts (§7.1): `before[i]` is the count of LF units in its first
/// `i * stride` bytes. Kept for every stride the buffer has completed.
#[derive(Debug)]
struct Lines {
    stride: usize,
    before: Vec<u64>,
}

impl Lines {
    fn new(stride: usize) -> Self {
        Lines {
            stride,
            before: vec![0],
        }
    }

    /// Counts the strides `bytes` (the whole buffer so far) has completed since the last call.
    fn extend(&mut self, bytes: &[u8], encoding: Encoding) {
        let s = self.stride;
        while self.before.len() * s <= bytes.len() {
            let i = self.before.len() - 1;
            let n = count_lf(encoding, &bytes[i * s..(i + 1) * s], (i * s) as u64);
            self.before.push(self.before[i] + n);
        }
    }

    /// The LF units wholly in `bytes[..x]`.
    fn upto(&self, bytes: &[u8], encoding: Encoding, x: usize) -> u64 {
        let i = (x / self.stride).min(self.before.len() - 1);
        let from = i * self.stride;
        self.before[i] + count_lf(encoding, &bytes[from..x], from as u64)
    }

    fn count(&self, bytes: &[u8], encoding: Encoding, range: Range<usize>) -> u64 {
        self.upto(bytes, encoding, range.end) - self.upto(bytes, encoding, range.start)
    }

    /// Where the `n`-th LF unit (from 1) at or after `from` starts.
    fn nth(&self, bytes: &[u8], encoding: Encoding, from: usize, n: u64) -> Option<usize> {
        let target = self.upto(bytes, encoding, from) + n;
        let i = self
            .before
            .partition_point(|&c| c < target)
            .checked_sub(1)?;
        let (mut at, mut left) = (i * self.stride, target - self.before[i]);
        loop {
            let j = encoding.find_lf(&bytes[at..], at as u64)?;
            left -= 1;
            if left == 0 {
                return Some(at + j);
            }
            at += j + encoding.lf().len();
        }
    }
}

/// The file's bytes as read at F4 (§7.4: whole, at most 256 MiB), never changed and never
/// copied: they move in with whatever holds them, and a clone (the journal's worker writing
/// `original`, §9.5) shares them. The line index is built here, on the worker that read them.
#[derive(Clone)]
pub struct Original(Arc<Held>);

struct Held {
    bytes: Box<dyn AsRef<[u8]> + Send + Sync>,
    encoding: Encoding,
    lines: Lines,
}

impl Original {
    pub fn new(bytes: impl AsRef<[u8]> + Send + Sync + 'static, encoding: Encoding) -> Self {
        Self::strided(bytes, encoding, STRIDE)
    }

    fn strided(
        bytes: impl AsRef<[u8]> + Send + Sync + 'static,
        encoding: Encoding,
        stride: usize,
    ) -> Self {
        let mut lines = Lines::new(stride);
        lines.extend(bytes.as_ref(), encoding);
        Original(Arc::new(Held {
            bytes: Box::new(bytes),
            encoding,
            lines,
        }))
    }

    pub fn bytes(&self) -> &[u8] {
        (*self.0.bytes).as_ref()
    }

    pub fn encoding(&self) -> Encoding {
        self.0.encoding
    }

    pub fn len(&self) -> u64 {
        self.bytes().len() as u64
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the two are one original: the same bytes, not a copy.
    pub fn same(&self, other: &Original) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Which buffer a piece is a range of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Run {
    /// The original of re-read generation `k`.
    Original(u32),
    /// An add block, by its index.
    Add(u32),
}

/// A range of the original or of one add block (§7.1). The document, its undo steps and a
/// block kept for the clipboard (E36) are pieces, never bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    run: Run,
    start: u64,
    len: u64,
    /// The LF units wholly inside it.
    lfs: u64,
}

impl Piece {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

fn follows(a: &Piece, b: &Piece) -> bool {
    a.run == b.run && a.start + a.len == b.start
}

fn total(pieces: &[Piece]) -> u64 {
    pieces.iter().map(|p| p.len).sum()
}

/// `next` after `pieces`, joined to the last one where they touch.
fn push(pieces: &mut Vec<Piece>, next: &[Piece]) {
    for &p in next.iter().filter(|p| p.len > 0) {
        match pieces.last_mut() {
            Some(last) if follows(last, &p) => {
                last.len += p.len;
                last.lfs += p.lfs;
            }
            _ => pieces.push(p),
        }
    }
}

/// Where a piece's bytes are: a range of the original of generation `k`, or of the add buffer
/// counted as one run of bytes ([`Buffer::add_chunks`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Original { k: u32, range: Range<u64> },
    Add(Range<u64>),
}

/// Pieces this buffer no longer holds: the original's before a re-read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stale;

/// How a change groups into undo steps (§7.3, Turbo C++'s Group Undo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Typing, Overwrite's too: joins the open step while each change starts where the run's
    /// text ends.
    Typing,
    /// Backspace: joins while each delete ends where the run's last one started.
    Backspace,
    /// Del: joins while each delete starts where the run's last one did.
    Delete,
    /// A step of its own, however many changes it makes: a paste, a block operation.
    Single,
}

/// One add block: appended to within the capacity it was made with, never reallocated.
struct Block {
    bytes: Vec<u8>,
    lines: Lines,
}

/// The add buffer: its blocks, and where each starts in the add buffer as one run of bytes.
struct Add {
    blocks: Vec<Block>,
    starts: Vec<u64>,
    len: u64,
    block: usize,
    stride: usize,
}

impl Add {
    /// Appends `bytes` in one block (a new one when the last cannot hold them, or holds a
    /// partial unit): their piece.
    fn append(&mut self, bytes: &[u8], encoding: Encoding) -> Piece {
        let fits = self.blocks.last().is_some_and(|b| {
            b.bytes.capacity() - b.bytes.len() >= bytes.len()
                && b.bytes.len().is_multiple_of(encoding.unit())
        });
        if !fits {
            self.starts.push(self.len);
            self.blocks.push(Block {
                bytes: Vec::with_capacity(self.block.max(bytes.len())),
                lines: Lines::new(self.stride),
            });
        }
        let index = self.blocks.len() - 1;
        let b = &mut self.blocks[index];
        let start = b.bytes.len();
        b.bytes.extend_from_slice(bytes);
        b.lines.extend(&b.bytes, encoding);
        self.len += bytes.len() as u64;
        Piece {
            run: Run::Add(index as u32),
            start: start as u64,
            len: bytes.len() as u64,
            lfs: b.lines.count(&b.bytes, encoding, start..b.bytes.len()),
        }
    }
}

/// One change: at `at`, `removed` taken out, then `inserted` put in.
#[derive(Debug)]
struct Change {
    at: u64,
    removed: Vec<Piece>,
    inserted: Vec<Piece>,
}

impl Change {
    /// Takes in `next` (made right after it) when it starts where this one's text ends.
    fn join_after(&mut self, next: &Change) -> bool {
        if next.at != self.at + total(&self.inserted) {
            return false;
        }
        push(&mut self.removed, &next.removed);
        push(&mut self.inserted, &next.inserted);
        true
    }

    /// Takes in `next` (made right after it) when what it removes ends where this one starts.
    fn join_before(&mut self, next: &Change) -> bool {
        if next.at + total(&next.removed) != self.at {
            return false;
        }
        self.at = next.at;
        for (mine, theirs) in [
            (&mut self.removed, &next.removed),
            (&mut self.inserted, &next.inserted),
        ] {
            let mut joined = theirs.clone();
            push(&mut joined, mine);
            *mine = joined;
        }
        true
    }
}

/// One undo step: its changes in order, and the caller's state before and after it.
struct Step<S> {
    id: u64,
    group: Group,
    changes: Vec<Change>,
    before: S,
    after: S,
}

/// The document: the original, the add buffer, the pieces and the undo steps. `S` is the
/// caller's state an undo or a redo gives back (the caret and the block).
pub struct Buffer<S> {
    original: Original,
    /// The original's re-read generation (§9.5's `k`).
    k: u32,
    add: Add,
    pieces: Vec<Piece>,
    len: u64,
    lfs: u64,
    undo: VecDeque<Step<S>>,
    redo: Vec<Step<S>>,
    /// The top step takes the next change of its run; a Save, an undo, a redo and
    /// [`Buffer::seal`] end the run.
    open: bool,
    /// Step ids, so the saved state is known by identity, not by its bytes: the next one, the
    /// state below the oldest step kept (0, the file as read, until a step goes), the saved one.
    next: u64,
    floor: u64,
    saved: u64,
}

impl<S> Buffer<S> {
    pub fn new(original: Original) -> Self {
        Self::with_block(original, BLOCK)
    }

    fn with_block(original: Original, block: usize) -> Self {
        let stride = original.0.lines.stride;
        let mut buffer = Buffer {
            original,
            k: 0,
            add: Add {
                blocks: Vec::new(),
                starts: Vec::new(),
                len: 0,
                block,
                stride,
            },
            pieces: Vec::new(),
            len: 0,
            lfs: 0,
            undo: VecDeque::new(),
            redo: Vec::new(),
            open: false,
            next: 1,
            floor: 0,
            saved: 0,
        };
        buffer.start_over();
        buffer
    }

    /// The document as the original alone.
    fn start_over(&mut self) {
        let o = &self.original;
        self.len = o.len();
        self.lfs = o.0.lines.upto(o.bytes(), o.encoding(), o.bytes().len());
        self.pieces.clear();
        if self.len > 0 {
            self.pieces.push(Piece {
                run: Run::Original(self.k),
                start: 0,
                len: self.len,
                lfs: self.lfs,
            });
        }
    }

    /// A re-read (§7.3: Reload, E30's re-read of a clean editor): the document is `original`
    /// (of the same encoding), generation `k` + 1, saved, with no undo steps. The add buffer
    /// stays: a block kept for the clipboard keeps its typed pieces, not the old original's.
    pub fn reread(&mut self, original: Original) {
        debug_assert_eq!(original.encoding(), self.encoding());
        self.original = original;
        self.k += 1;
        self.undo.clear();
        self.redo.clear();
        self.open = false;
        self.floor = self.next;
        self.next += 1;
        self.saved = self.floor;
        self.start_over();
    }

    pub fn original(&self) -> &Original {
        &self.original
    }

    pub fn encoding(&self) -> Encoding {
        self.original.encoding()
    }

    /// The original's re-read generation `k`: 0 for the file as read at F4.
    pub fn generation(&self) -> u32 {
        self.k
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    pub fn span(&self, piece: &Piece) -> Span {
        let range = piece.start..piece.start + piece.len;
        match piece.run {
            Run::Original(k) => Span::Original { k, range },
            Run::Add(i) => {
                let s = self.add.starts.get(i as usize).copied().unwrap_or(0);
                Span::Add(s + range.start..s + range.end)
            }
        }
    }

    /// The bytes appended to the add buffer this session.
    pub fn add_len(&self) -> u64 {
        self.add.len
    }

    /// `range` of the add buffer counted as one run of bytes, block by block.
    pub fn add_chunks(&self, range: Range<u64>) -> impl Iterator<Item = &[u8]> + '_ {
        let blocks = self.add.blocks.iter().zip(&self.add.starts);
        blocks.filter_map(move |(b, &s)| {
            let from = range.start.max(s);
            let to = range.end.min(s + b.bytes.len() as u64);
            (from < to).then(|| &b.bytes[(from - s) as usize..(to - s) as usize])
        })
    }

    fn run(&self, run: Run) -> (&[u8], &Lines) {
        match run {
            Run::Original(_) => (self.original.bytes(), &self.original.0.lines),
            Run::Add(i) => {
                let b = &self.add.blocks[i as usize];
                (&b.bytes, &b.lines)
            }
        }
    }

    fn bytes_of(&self, p: &Piece) -> &[u8] {
        &self.run(p.run).0[p.start as usize..(p.start + p.len) as usize]
    }

    /// Whether `p` is a range of a buffer this one holds now.
    fn holds(&self, p: &Piece) -> bool {
        let size = match p.run {
            Run::Original(k) if k == self.k => self.original.len(),
            Run::Original(_) => return false,
            Run::Add(i) => match self.add.blocks.get(i as usize) {
                Some(b) => b.bytes.len() as u64,
                None => return false,
            },
        };
        p.start.checked_add(p.len).is_some_and(|end| end <= size)
    }

    /// The piece holding byte `at` (the count of pieces at the end) and where it starts.
    fn find(&self, at: u64) -> (usize, u64) {
        let mut off = 0;
        for (i, p) in self.pieces.iter().enumerate() {
            if at < off + p.len {
                return (i, off);
            }
            off += p.len;
        }
        (self.pieces.len(), off)
    }

    /// The bytes of `range`, piece by piece.
    pub fn chunks(&self, range: Range<u64>) -> impl Iterator<Item = &[u8]> + '_ {
        let start = range.start;
        let end = range.end.min(self.len).max(start);
        let (first, mut off) = self.find(start);
        self.pieces[first..].iter().map_while(move |p| {
            let at = off;
            off += p.len;
            (at < end).then(|| {
                let from = start.max(at) - at;
                &self.bytes_of(p)[from as usize..(end.min(off) - at) as usize]
            })
        })
    }

    /// Appends the bytes of `range` to `out`.
    pub fn read(&self, range: Range<u64>, out: &mut Vec<u8>) {
        for c in self.chunks(range) {
            out.extend_from_slice(c);
        }
    }

    /// Serialises the document for Save (§7.1): the pieces in order, each written as it is.
    /// Returns the bytes written.
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<u64> {
        for p in &self.pieces {
            w.write_all(self.bytes_of(p))?;
        }
        Ok(self.len)
    }

    /// The pieces of `range`, for a block's copy or move (E36): no byte is copied.
    pub fn pieces_in(&self, range: Range<u64>) -> Vec<Piece> {
        let end = range.end.min(self.len);
        let (first, mut off) = self.find(range.start);
        let mut out = Vec::new();
        for p in &self.pieces[first..] {
            if off >= end {
                break;
            }
            let (a, b) = (range.start.max(off), end.min(off + p.len));
            if a < b {
                out.push(self.cut(p, a - off..b - off));
            }
            off += p.len;
        }
        out
    }

    /// The part `range` (counted within it) of a piece.
    fn cut(&self, p: &Piece, range: Range<u64>) -> Piece {
        if range == (0..p.len) {
            return *p;
        }
        let (bytes, lines) = self.run(p.run);
        let (s, e) = (p.start + range.start, p.start + range.end);
        Piece {
            start: s,
            len: e - s,
            lfs: lines.count(bytes, self.encoding(), s as usize..e as usize),
            ..*p
        }
    }

    /// The lines: one more than the LF units.
    pub fn lines(&self) -> u64 {
        self.lfs + 1
    }

    /// Where line `line` (from 0) starts: after the `line`-th LF unit. `None` past the last.
    pub fn line_start(&self, line: u64) -> Option<u64> {
        if line == 0 {
            return Some(0);
        }
        let (mut off, mut seen) = (0, 0);
        for p in &self.pieces {
            if seen + p.lfs >= line {
                let (bytes, lines) = self.run(p.run);
                let e = self.encoding();
                let lf = lines.nth(bytes, e, p.start as usize, line - seen)? as u64;
                return Some(off + lf - p.start + e.lf().len() as u64);
            }
            (off, seen) = (off + p.len, seen + p.lfs);
        }
        None
    }

    /// The line byte `at` is on: the LF units wholly before it.
    pub fn line_of(&self, at: u64) -> u64 {
        let (mut off, mut seen) = (0, 0);
        for p in &self.pieces {
            if at < off + p.len {
                let (bytes, lines) = self.run(p.run);
                let s = p.start as usize;
                return seen + lines.count(bytes, self.encoding(), s..s + (at - off) as usize);
            }
            (off, seen) = (off + p.len, seen + p.lfs);
        }
        seen
    }

    /// `at`, or past the LF when it falls strictly between a CR unit and an LF unit (E35).
    fn whole(&self, at: u64) -> u64 {
        let e = self.encoding();
        let u = e.unit();
        if at < u as u64 || at + u as u64 > self.len || !at.is_multiple_of(u as u64) {
            return at;
        }
        let (mut pair, mut n) = ([0u8; 8], 0);
        for c in self.chunks(at - u as u64..at + u as u64) {
            pair[n..n + c.len()].copy_from_slice(c);
            n += c.len();
        }
        if pair[..u] == *e.cr() && pair[u..2 * u] == *e.lf() {
            at + u as u64
        } else {
            at
        }
    }

    /// Makes `at` a piece boundary: the index of the first piece from it.
    fn split(&mut self, at: u64) -> usize {
        let (i, off) = self.find(at);
        if i == self.pieces.len() || off == at {
            return i;
        }
        let p = self.pieces[i];
        let left = self.cut(&p, 0..at - off);
        self.pieces[i] = left;
        let right = Piece {
            start: p.start + left.len,
            len: p.len - left.len,
            lfs: p.lfs - left.lfs,
            ..p
        };
        self.pieces.insert(i + 1, right);
        i + 1
    }

    /// Joins the pieces on either side of boundary `j` where they touch.
    fn merge(&mut self, j: usize) {
        if j == 0 || j >= self.pieces.len() || !follows(&self.pieces[j - 1], &self.pieces[j]) {
            return;
        }
        let b = self.pieces.remove(j);
        let a = &mut self.pieces[j - 1];
        a.len += b.len;
        a.lfs += b.lfs;
    }

    /// Puts `pieces` in at `at`, as recorded: no guard.
    fn put(&mut self, at: u64, pieces: &[Piece]) {
        let mut joined = Vec::with_capacity(pieces.len());
        push(&mut joined, pieces);
        self.len += total(&joined);
        self.lfs += joined.iter().map(|p| p.lfs).sum::<u64>();
        let (i, n) = (self.split(at), joined.len());
        self.pieces.splice(i..i, joined);
        self.merge(i + n);
        self.merge(i);
    }

    /// Takes `range` out, as recorded: no guard. Its pieces.
    fn take(&mut self, range: Range<u64>) -> Vec<Piece> {
        if range.start >= range.end {
            return Vec::new();
        }
        let i = self.split(range.start);
        let j = self.split(range.end);
        let gone: Vec<Piece> = self.pieces.drain(i..j).collect();
        self.len -= total(&gone);
        self.lfs -= gone.iter().map(|p| p.lfs).sum::<u64>();
        self.merge(i);
        gone
    }

    /// The undo steps held.
    pub fn steps(&self) -> usize {
        self.undo.len()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn state(&self) -> u64 {
        self.undo.back().map_or(self.floor, |s| s.id)
    }

    /// Whether the document is the version last saved, or the file as read when none was.
    pub fn is_saved(&self) -> bool {
        self.state() == self.saved
    }

    /// Ends the open run: the next change starts a step of its own.
    pub fn seal(&mut self) {
        self.open = false;
    }

    /// A Save landed (§9.3): the undo stack gets a mark here, and the run ends. Nothing is
    /// rebased: undo and redo cross the mark on the same buffers.
    pub fn mark_saved(&mut self) {
        self.open = false;
        self.saved = self.state();
    }
}

impl<S: Clone> Buffer<S> {
    /// Starts an edit whose changes are one undo step of `group`, or join the open step of
    /// its run; `before` is the state an undo of that step gives back.
    pub fn edit(&mut self, group: Group, before: S) -> Edit<'_, S> {
        Edit {
            buffer: self,
            group,
            before,
            touched: false,
        }
    }

    /// Undoes the last step; the state before it.
    pub fn undo(&mut self) -> Option<S> {
        self.open = false;
        let step = self.undo.pop_back()?;
        for c in step.changes.iter().rev() {
            self.take(c.at..c.at + total(&c.inserted));
            self.put(c.at, &c.removed);
        }
        let before = step.before.clone();
        self.redo.push(step);
        Some(before)
    }

    /// Redoes the last step undone; the state after it.
    pub fn redo(&mut self) -> Option<S> {
        self.open = false;
        let step = self.redo.pop()?;
        for c in &step.changes {
            self.take(c.at..c.at + total(&c.removed));
            self.put(c.at, &c.inserted);
        }
        let after = step.after.clone();
        self.undo.push_back(step);
        Some(after)
    }
}

/// One edit of the document: its changes, guarded by E35, recorded as one undo step or joined
/// to the open step of its run. Reads go through it to the buffer. [`Edit::end`] gives the state
/// after it, which a redo gives back; without it a redo gives the state before.
pub struct Edit<'a, S> {
    buffer: &'a mut Buffer<S>,
    group: Group,
    before: S,
    /// A change was recorded: this edit's step is the top one.
    touched: bool,
}

impl<S> Deref for Edit<'_, S> {
    type Target = Buffer<S>;

    fn deref(&self) -> &Buffer<S> {
        self.buffer
    }
}

impl<S: Clone> Edit<'_, S> {
    /// Inserts `bytes` (encoded, whole units) at `at`; the range they landed at.
    pub fn insert(&mut self, at: u64, bytes: &[u8]) -> Range<u64> {
        let b = &mut *self.buffer;
        let at = b.whole(at.min(b.len));
        if bytes.is_empty() {
            return at..at;
        }
        let piece = b.add.append(bytes, b.original.encoding());
        b.put(at, &[piece]);
        self.record(Change {
            at,
            removed: Vec::new(),
            inserted: vec![piece],
        });
        at..at + piece.len
    }

    /// Inserts pieces this buffer made ([`Buffer::pieces_in`]) at `at`, their bytes unchanged
    /// (E36); the range they landed at. [`Stale`] when one is no longer held, inserting nothing.
    pub fn insert_pieces(&mut self, at: u64, pieces: &[Piece]) -> Result<Range<u64>, Stale> {
        let b = &mut *self.buffer;
        if !pieces.iter().all(|p| b.holds(p)) {
            return Err(Stale);
        }
        let at = b.whole(at.min(b.len));
        let mut inserted = Vec::new();
        push(&mut inserted, pieces);
        if inserted.is_empty() {
            return Ok(at..at);
        }
        let n = total(&inserted);
        b.put(at, &inserted);
        self.record(Change {
            at,
            removed: Vec::new(),
            inserted,
        });
        Ok(at..at + n)
    }

    /// Deletes `range`; the range deleted (empty when nothing was).
    pub fn delete(&mut self, range: Range<u64>) -> Range<u64> {
        let b = &mut *self.buffer;
        let start = b.whole(range.start.min(b.len));
        let end = b.whole(range.end.min(b.len));
        if start >= end {
            return start..start;
        }
        let removed = b.take(start..end);
        self.record(Change {
            at: start,
            removed,
            inserted: Vec::new(),
        });
        start..end
    }

    /// Ends the edit: `after` is the state a redo of its step gives back.
    pub fn end(self, after: S) {
        if self.touched {
            if let Some(step) = self.buffer.undo.back_mut() {
                step.after = after;
            }
        }
    }

    fn record(&mut self, change: Change) {
        let b = &mut *self.buffer;
        let group = self.group;
        let top = b.undo.back_mut();
        // A later change of this edit joins its step's last change where they touch; the
        // first joins the open run of its group.
        let joined = if self.touched {
            top.and_then(|s| s.changes.last_mut())
                .is_some_and(|c| c.join_after(&change) || c.join_before(&change))
        } else {
            b.open
                && top
                    .filter(|s| s.group == group)
                    .and_then(|s| s.changes.last_mut())
                    .is_some_and(|c| match group {
                        Group::Typing | Group::Delete => c.join_after(&change),
                        Group::Backspace => c.join_before(&change),
                        Group::Single => false,
                    })
        };
        let first = !self.touched;
        self.touched = true;
        if joined {
            return;
        }
        if !first {
            if let Some(step) = b.undo.back_mut() {
                step.changes.push(change);
                return;
            }
        }
        b.redo.clear();
        b.undo.push_back(Step {
            id: b.next,
            group,
            changes: vec![change],
            before: self.before.clone(),
            after: self.before.clone(),
        });
        b.next += 1;
        b.open = true;
        if b.undo.len() > UNDO_STEPS {
            if let Some(gone) = b.undo.pop_front() {
                b.floor = gone.id;
            }
        }
    }
}

#[cfg(test)]
mod tests;
