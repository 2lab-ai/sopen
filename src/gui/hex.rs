//! The hex window (spec §6a): any file as rows of 16 bytes — offset, hex, ASCII — read through a
//! small page cache, so only the rows on screen are ever read and a file of any size scrolls
//! the same. Editing overwrites bytes only (the size never changes); Undo/Redo; Save (backup
//! first, conflict-checked overwrite) and Save copy (a new sibling), as in the image window.
//!
//! [`HexDoc`] is the edit model (the bytes as opened plus a sparse map of changed bytes) and
//! [`HexFile`] the write side; both are window-free, so the tests and the `SOPEN_AUTOSAVE` hook
//! run the exact code the window's buttons run.
//!
//! As the file browser's preview (02 §4.2, [`super::Window::set_preview`]) the window shows its
//! first rows without toolbar, status row or cursor, takes no input and never takes the keyboard.

mod file;

pub use file::{Base, Fetching, HexFile, Rebind, Repaint, Saved};

use super::chrome::{self, HEX_KEYS};
use super::image::CANNOT_REPLACE;
use super::{Hooks, Outcome, Status};
use crate::stream::{Bytes, Mark, ReadError, Reader, Source, Waiting};
use anyhow::{anyhow, bail, Result};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, Vec2};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::task::Poll;
// What the tests beside this file name through `super::*`, as before the write side moved.
#[cfg(test)]
#[allow(unused_imports)]
use {
    super::Origin, crate::remote::WriteOutcome, anyhow::Context as _, file::CHUNK,
    std::time::SystemTime,
};

/// Bytes per row.
pub const ROW: u64 = 16;
/// The page cache: pages of this many bytes, at most [`PAGES`] of them held.
const PAGE: u64 = 4096;
const PAGES: usize = 64;
/// Undo history bound (one step per typed digit or character).
const UNDO_STEPS: usize = 100_000;
/// The window's size when it opens as a hex window.
pub const WINDOW: [f32; 2] = [760.0, 560.0];
/// The window's title in its frame, as PC Shell names its hex view.
const TITLE: &str = "File Edit";

/// An offset as typed: `0x…` (hex) or decimal digits.
pub fn parse_offset(text: &str) -> Result<u64> {
    let t = text.trim();
    let bad = || anyhow!("'{t}' is not an offset (0x… for hex, or decimal)");
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) => {
            u64::from_str_radix(hex, 16).map_err(|_| bad())
        }
        Some(_) => Err(bad()),
        None if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => {
            t.parse().map_err(|_| bad())
        }
        None => Err(bad()),
    }
}

/// Bytes as hex digits, two per byte (`deadBEEF`).
pub fn parse_bytes(text: &str) -> Result<Vec<u8>> {
    let t = text.trim();
    if t.is_empty() || t.len() % 2 == 1 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("'{t}' is not bytes in hex (two digits per byte)");
    }
    (0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16).map_err(|_| anyhow!("'{t}' is not hex")))
        .collect()
}

/// `1234567` → `1,234,567`.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Hex digits the offset column needs for a file of `size` bytes (at least 4).
pub fn offset_digits(size: u64) -> usize {
    let last = size.saturating_sub(1);
    let bits = 64 - last.leading_zeros() as usize;
    bits.div_ceil(4).max(4)
}

/// Decimal digits the offset column needs for a file of `size` bytes (at least 4).
pub fn decimal_digits(size: u64) -> usize {
    size.saturating_sub(1).to_string().len().max(4)
}

/// A row's offset as PC Shell's File Edit prints it: decimal, then hex in parentheses —
/// `0016(0010)` — each as wide as the file's last offset needs (at least four digits), so the
/// column keeps its width down the file.
pub fn offset_text(at: u64, size: u64) -> String {
    let (dec, hex) = (decimal_digits(size), offset_digits(size));
    format!("{at:0dec$}({at:0hex$X})")
}

/// One row's 16 (or fewer, at the end) bytes as the hex column shows them: two digits each,
/// one space between, two between the 8th and the 9th.
pub fn hex_cells(bytes: &[Option<u8>]) -> String {
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push_str(if i == 8 { "  " } else { " " });
        }
        match b {
            Some(b) => out.push_str(&format!("{b:02X}")),
            None => out.push_str("??"),
        }
    }
    out
}

/// The ASCII column's character for a byte: its code page 437 glyph, as PC Shell shows the
/// column — printable ASCII as itself, ☺ ♥ ▲ … for the control bytes, Ç é ░ ═ … for the upper
/// half, a blank for 0x00 — and `?` for a byte that could not be read (`??` beside it).
pub fn ascii_char(b: Option<u8>) -> char {
    match b {
        Some(b) => chrome::cp437(b),
        None => '?',
    }
}

/// Where the hex column draws byte `i` of a row, in characters from the column's start.
fn hex_col(i: u64) -> u64 {
    3 * i + u64::from(i >= 8)
}

/// Whether a row of `per` bytes — the offset, the hex column, the ASCII column — fits in
/// `width` points at `cw` points a character.
fn row_fits(width: f32, cw: f32, offset_chars: usize, per: u64) -> bool {
    let chars = offset_chars as u64 + 1 + per * 3 + 2 + per;
    cw * chars as f32 <= width
}

// ---------------------------------------------------------------------------------------------
// The edit model.

/// What the hex window reads its bytes through (06 §7.1): a stream's [`Reader`] at the screen,
/// or a local file in place.
pub trait Feed: Bytes + Send {
    /// What it has been waiting for, and for how long. A file read in place never waits.
    fn waiting(&self) -> Option<Waiting> {
        None
    }

    /// After a reload: it reads the version open now (06 §5).
    fn renew(&mut self) {}

    /// A local file read in place: every read is answered at once, by the file itself.
    fn in_place(&self) -> bool {
        false
    }
}

impl Feed for Reader {
    fn waiting(&self) -> Option<Waiting> {
        Reader::waiting(self)
    }

    fn renew(&mut self) {
        Reader::renew(self)
    }
}

/// A local file read in place, through its handle.
pub struct InPlace(pub Arc<File>);

impl InPlace {
    /// Fills `buf` from `off` until it is full or the file ends.
    fn fill(
        &self,
        off: u64,
        buf: &mut [u8],
        cancel: Option<&AtomicBool>,
    ) -> crate::stream::Result<usize> {
        let mut got = 0;
        while got < buf.len() {
            if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                return Err(ReadError::Cancelled);
            }
            match self.0.read_at(&mut buf[got..], off + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ReadError::Failed(e.to_string())),
            }
        }
        Ok(got)
    }
}

impl Bytes for InPlace {
    fn len(&self) -> u64 {
        self.0.metadata().map_or(0, |m| m.len())
    }

    fn try_read(&mut self, off: u64, len: usize) -> Poll<crate::stream::Result<bytes::Bytes>> {
        let mut buf = vec![0; len];
        Poll::Ready(self.fill(off, &mut buf, None).map(|n| {
            buf.truncate(n);
            buf.into()
        }))
    }

    fn read_at(
        &mut self,
        off: u64,
        buf: &mut [u8],
        cancel: &AtomicBool,
    ) -> crate::stream::Result<usize> {
        self.fill(off, buf, Some(cancel))
    }
}

impl Feed for InPlace {
    fn in_place(&self) -> bool {
        true
    }
}

/// What a place in the grid shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Byte(u8),
    /// Its bytes are on their way (06 §7.1): `··`, in the skin's dim colour.
    Pending,
    /// It cannot be read: `??`.
    Unreadable,
}

impl Cell {
    pub fn byte(self) -> Option<u8> {
        match self {
            Cell::Byte(b) => Some(b),
            Cell::Pending | Cell::Unreadable => None,
        }
    }
}

/// What a frame does about the pages of its screen that are not here (a stream's only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ask {
    /// Reads them: the fetch is queued, and they show as pending until it arrives.
    Read,
    /// Asks for nothing and shows them as pending: a place the scroll bar's thumb passes.
    Wait,
    /// Asks for nothing and shows them as unreadable: a failed stream, until the next input
    /// retries.
    Failed,
}

/// Reads a file through a small page cache, at most [`PAGES`] pages held (the least recently
/// used one goes first). A local file is read in place, a page when a byte of it is asked for.
/// A stream is read once a frame, for the pages of the screen that are not here
/// ([`Pages::view`]): a page that has not arrived is pending, never empty (06 §7.1).
struct Pages {
    feed: Box<dyn Feed>,
    in_place: bool,
    size: u64,
    /// Page number → (bytes, last use): only pages that were read.
    held: HashMap<u64, (Vec<u8>, u64)>,
    tick: u64,
    /// A local file: the first read error, for the status line.
    error: Option<String>,
    /// A local file: pages whose read failed since the last frame (not tried again within it).
    bad: HashSet<u64>,
    /// A stream: the pages not held cannot be read (its last read failed), rather than being
    /// on their way.
    failed: bool,
    /// A stream: where the pages of the last view that are not here begin, while they are on
    /// their way.
    pending_at: Option<u64>,
    /// A stream: a read found the version open now to be another one, and the reader was moved
    /// to it (06 §5); taken by the window.
    renewed: bool,
    /// Pages read so far (each read is one page).
    reads: u64,
    /// Reads asked of the feed so far.
    asks: u64,
}

impl Pages {
    fn page_len(&self, page: u64) -> usize {
        (self.size - page * PAGE).min(PAGE) as usize
    }

    /// Holds `bytes` as `page`; the least recently used pages outside `spare` go first.
    fn keep(&mut self, page: u64, bytes: Vec<u8>, spare: (u64, u64)) {
        self.tick += 1;
        while self.held.len() >= PAGES {
            let oldest = self
                .held
                .iter()
                .filter(|(k, _)| **k < spare.0 || **k > spare.1)
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| *k);
            match oldest {
                Some(old) => self.held.remove(&old),
                None => break,
            };
        }
        self.reads += 1;
        self.held.insert(page, (bytes, self.tick));
    }

    /// A local file's page, read now.
    fn load(&mut self, page: u64) {
        let (start, len) = (page * PAGE, self.page_len(page));
        self.asks += 1;
        let why = match self.feed.try_read(start, len) {
            Poll::Ready(Ok(got)) if got.len() == len => {
                return self.keep(page, got.to_vec(), (page, page))
            }
            Poll::Ready(Ok(got)) => format!("the file ends at 0x{:X}", start + got.len() as u64),
            Poll::Ready(Err(e)) => e.to_string(),
            Poll::Pending => "not there yet".to_string(),
        };
        self.reads += 1;
        self.error
            .get_or_insert_with(|| format!("cannot read at 0x{start:X}: {why}"));
        self.bad.insert(page);
    }

    fn cell(&mut self, at: u64) -> Cell {
        if at >= self.size {
            return Cell::Unreadable;
        }
        let page = at / PAGE;
        if !self.held.contains_key(&page) {
            if !self.in_place {
                return if self.failed {
                    Cell::Unreadable
                } else {
                    Cell::Pending
                };
            }
            if !self.bad.contains(&page) {
                self.load(page);
            }
        }
        self.tick += 1;
        let tick = self.tick;
        match self.held.get_mut(&page) {
            Some((buf, used)) => {
                *used = tick;
                buf.get((at - page * PAGE) as usize)
                    .map_or(Cell::Unreadable, |b| Cell::Byte(*b))
            }
            None => Cell::Unreadable,
        }
    }

    /// One frame's read (06 §7.1: one reader at the screen): the screen shows `len` bytes at
    /// `off`. A stream is asked once, for the span of the screen's pages that are not here —
    /// so while it waits the reader sees the same read again, and scrolling down reads on from
    /// the one before (06 §4). A local file is read where a byte is asked for; this only lets a
    /// page that failed be tried again.
    fn view(&mut self, off: u64, len: u64, ask: Ask) {
        self.bad.clear();
        self.pending_at = None;
        if self.in_place || len == 0 || off >= self.size {
            return;
        }
        let (first, last) = (off / PAGE, (off + len - 1).min(self.size - 1) / PAGE);
        let mut missing = (first..=last).filter(|p| !self.held.contains_key(p));
        let span = match (missing.next(), missing.next_back()) {
            (Some(a), b) => Some((a, b.unwrap_or(a))),
            (None, _) => None,
        };
        let Some((a, b)) = span else {
            // Every page of the screen is here: what was asked for another place is no longer
            // waited for (06 §4: the latest place wins).
            self.withdraw(off);
            return;
        };
        match ask {
            Ask::Read => {}
            Ask::Wait => {
                self.failed = false;
                self.pending_at = Some((a * PAGE).max(off));
                // The thumb moved on: the place it left is not fetched.
                self.withdraw(off);
                return;
            }
            Ask::Failed => return self.failed = true,
        }
        let start = a * PAGE;
        let len = ((b + 1) * PAGE).min(self.size) - start;
        self.asks += 1;
        match self.feed.try_read(start, len as usize) {
            Poll::Pending => {
                self.failed = false;
                self.pending_at = Some(start.max(off));
            }
            Poll::Ready(Ok(got)) if got.len() as u64 == len => {
                for page in a..=b {
                    let from = ((page - a) * PAGE) as usize;
                    let to = from + self.page_len(page);
                    self.keep(page, got[from..to].to_vec(), (first, last));
                }
                self.failed = false;
            }
            // The version open now is another one (06 §5): nothing held is of it, and its
            // size may be another. The next frame reads it.
            Poll::Ready(Err(ReadError::Reloaded)) => self.renew(off),
            // What is shown stays (06 §5); only what is not here cannot be read.
            Poll::Ready(_) => self.failed = true,
        }
    }

    /// The stream's wait for another place ends, unsent: a read of no length there is the
    /// reader's move away (nothing is fetched for it).
    fn withdraw(&mut self, off: u64) {
        if self.feed.waiting().is_some() {
            let _ = self.feed.try_read(off, 0);
        }
    }

    /// The reader reads the version open now: nothing held is of it. The rows at `off` are on
    /// their way until the next view has read them.
    fn renew(&mut self, off: u64) {
        self.feed.renew();
        self.held.clear();
        self.size = self.feed.len();
        self.renewed = true;
        self.failed = false;
        self.pending_at = (self.size > 0).then(|| off.min(self.size - 1));
    }

    /// A page read now, however long it takes: the windowless Save's `SOPEN_STATE` patches.
    fn fetch(&mut self, page: u64) -> Result<()> {
        if self.held.contains_key(&page) {
            return Ok(());
        }
        let mut buf = vec![0; self.page_len(page)];
        self.asks += 1;
        let never = AtomicBool::new(false);
        let got = self
            .feed
            .read_at(page * PAGE, &mut buf, &never)
            .map_err(|e| anyhow!("cannot read at 0x{:X}: {e}", page * PAGE))?;
        if got != buf.len() {
            bail!("cannot read at 0x{:X}: the file ends there", page * PAGE);
        }
        self.keep(page, buf, (page, page));
        Ok(())
    }

    fn resident(&self) -> usize {
        self.held.values().map(|(b, _)| b.capacity()).sum()
    }
}

/// Which column the cursor types into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Hex,
    Ascii,
}

/// The cursor: a byte, the column it types into, and (in the hex column) whether the next
/// digit is the byte's low nibble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub at: u64,
    pub pane: Pane,
    pub low: bool,
}

/// One change, as the bytes were shown: undone by putting `before` back, redone by `after`.
#[derive(Debug, Clone, Copy)]
struct Step {
    at: u64,
    before: u8,
    after: u8,
    /// Where the cursor was when the change was made (undo puts it back there).
    cursor: Cursor,
}

/// The bytes as opened (never written: read through [`Pages`]) and the edits made to them: a
/// sparse map offset → byte holding the byte wanted at every offset set — absolute values, as
/// shown, never said relative to the file under them. An entry may equal the file's byte (an
/// edit undone, a byte typed back); whether a byte is changed is a comparison with the byte the
/// file had there, which the edit keeps (06 §7.1): read when the edit was typed, so nothing
/// about the edits is ever read from the file again — over a stream that would block a frame,
/// or count a byte not here as changed.
pub struct HexDoc {
    pages: Pages,
    patches: BTreeMap<u64, u8>,
    /// The byte the file as opened has at every offset `patches`, `saved`, `copied` or an undo
    /// or redo step names (after a rebase: the file read from then on).
    replaced: HashMap<u64, u8>,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    /// Undo steps kept (the oldest goes first).
    undo_cap: usize,
    /// The byte whose high nibble was just typed (its step is the last one): the low nibble
    /// typed next joins that step, so a byte typed as two digits undoes as one.
    open_step: Option<u64>,
    /// Bumped by every change (a repaint key; what is saved is compared by content).
    rev: u64,
    /// The bytes the file holds beyond the bytes read (what the last Save in it wrote), and
    /// the ones the last Save copy holds — absolute, like `patches`.
    saved: BTreeMap<u64, u8>,
    copied: Option<BTreeMap<u64, u8>>,
    /// `changed_count` at `rev` (it compares every entry with the file).
    counted: Option<(u64, usize)>,
    /// Bumped by everything that can change `dirty` / `unsaved` (edits, a Save or a Save copy
    /// landing, a rebase); their answers are kept for the generation they were given at.
    generation: u64,
    answers: Option<(u64, bool, bool)>,
}

/// Whether the bytes shown with `a` and with `b` over the file differ anywhere. Walks both
/// maps; where one has an entry and the other does not, the file's byte there is the one the
/// edit kept (`replaced`) — the file is not read. An offset no edit was made at has no kept
/// byte and counts as different.
fn differ(replaced: &HashMap<u64, u8>, a: &BTreeMap<u64, u8>, b: &BTreeMap<u64, u8>) -> bool {
    for (&at, &x) in a {
        match b.get(&at) {
            Some(&y) if x != y => return true,
            Some(_) => {}
            None if replaced.get(&at) != Some(&x) => return true,
            None => {}
        }
    }
    b.iter()
        .any(|(&at, &y)| !a.contains_key(&at) && replaced.get(&at) != Some(&y))
}

fn pages(feed: Box<dyn Feed>, size: u64) -> Pages {
    Pages {
        in_place: feed.in_place(),
        feed,
        size,
        held: HashMap::new(),
        tick: 0,
        error: None,
        bad: HashSet::new(),
        failed: false,
        pending_at: None,
        renewed: false,
        reads: 0,
        asks: 0,
    }
}

impl HexDoc {
    /// The document over `file`, whose first `size` bytes are shown.
    pub fn new(file: Arc<File>, size: u64) -> Self {
        Self::over(Box::new(InPlace(file)), size)
    }

    /// The document over what `file` is read through: its stream, or the local file in place.
    pub fn of(file: &HexFile) -> Self {
        Self::over(file.feed(), file.size())
    }

    /// The document over `feed`, whose first `size` bytes are shown.
    pub fn over(feed: Box<dyn Feed>, size: u64) -> Self {
        Self {
            pages: pages(feed, size),
            patches: BTreeMap::new(),
            replaced: HashMap::new(),
            undo: VecDeque::new(),
            redo: Vec::new(),
            undo_cap: UNDO_STEPS,
            open_step: None,
            rev: 0,
            saved: BTreeMap::new(),
            copied: None,
            counted: None,
            generation: 0,
            answers: None,
        }
    }

    /// A smaller undo bound (tests reach it without a hundred thousand edits).
    #[cfg(test)]
    pub(crate) fn set_undo_cap(&mut self, cap: usize) {
        self.undo_cap = cap;
    }

    /// Undo steps held now.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn size(&self) -> u64 {
        self.pages.size
    }

    /// Rows in the grid.
    pub fn rows(&self) -> u64 {
        self.size().div_ceil(ROW)
    }

    /// The byte at `at` as edited (`None` past the end, where the file cannot be read, or where
    /// its bytes are not here yet).
    pub fn byte(&mut self, at: u64) -> Option<u8> {
        self.cell(at).byte()
    }

    /// What the grid shows at `at`.
    pub fn cell(&mut self, at: u64) -> Cell {
        if at >= self.size() {
            return Cell::Unreadable;
        }
        match self.patches.get(&at) {
            Some(&b) => Cell::Byte(b),
            None => self.pages.cell(at),
        }
    }

    /// The byte the file holds, where it is here.
    pub fn original(&mut self, at: u64) -> Option<u8> {
        self.pages.cell(at).byte()
    }

    /// Whether the byte at `at` differs from the file's: from the byte the edit replaced.
    pub fn changed(&mut self, at: u64) -> bool {
        match self.patches.get(&at) {
            Some(b) => self.replaced.get(&at) != Some(b),
            None => false,
        }
    }

    /// How many bytes differ from the file.
    pub fn changed_count(&mut self) -> usize {
        if let Some((rev, n)) = self.counted {
            if rev == self.rev {
                return n;
            }
        }
        let n = self
            .patches
            .iter()
            .filter(|(at, b)| self.replaced.get(at) != Some(b))
            .count();
        self.counted = Some((self.rev, n));
        n
    }

    /// The bytes wanted at every offset set (absolute; some may equal the file's).
    pub fn patches(&self) -> &BTreeMap<u64, u8> {
        &self.patches
    }

    /// The first read error, if any.
    pub fn read_error(&self) -> Option<&str> {
        self.pages.error.as_deref()
    }

    /// Bytes the page cache holds now.
    pub fn resident(&self) -> usize {
        self.pages.resident()
    }

    /// Pages read from the file so far.
    pub fn reads(&self) -> u64 {
        self.pages.reads
    }

    /// Reads asked of the feed so far.
    pub fn asks(&self) -> u64 {
        self.pages.asks
    }

    /// One frame's read: the screen shows `len` bytes at `off` ([`Pages::view`]).
    pub(super) fn view(&mut self, off: u64, len: u64, ask: Ask) {
        self.pages.view(off, len, ask);
    }

    /// Where the bytes of the last view that are on their way begin, if any are.
    pub fn pending_at(&self) -> Option<u64> {
        self.pages.pending_at
    }

    /// What the stream has been waiting for, and for how long.
    pub fn waiting(&self) -> Option<Waiting> {
        self.pages.feed.waiting()
    }

    /// The pages held are dropped and the next view reads them again, from the version of the
    /// stream open now (after a reload: one of sopen's own writes, or Cmd/Ctrl+R; 06 §5).
    pub(super) fn refresh(&mut self) {
        let shown = self.pages.held.keys().min().copied().unwrap_or(0);
        self.pages.renew(shown * PAGE);
    }

    /// Whether a read found the stream reloaded since this was last asked: the reader reads
    /// the version open now, and nothing held is of the one before.
    pub(super) fn take_renewed(&mut self) -> bool {
        std::mem::take(&mut self.pages.renewed)
    }

    /// Every edit, undo and redo step goes: they were made over a version of the file that is
    /// no longer the one read (a reload).
    pub(super) fn reset(&mut self) {
        self.patches.clear();
        self.replaced.clear();
        self.undo.clear();
        self.redo.clear();
        self.saved.clear();
        self.copied = None;
        self.open_step = None;
        self.bump();
    }

    /// Reads the page of `at` now, however long it takes (no window: `SOPEN_AUTOSAVE`).
    fn fetch(&mut self, at: u64) -> Result<()> {
        self.pages.fetch(at / PAGE)
    }

    /// Overwrites the byte at `at` with `value`, one undo step (`cursor` = where the cursor is,
    /// for the undo). Writing the byte already shown is no step. `false` past the end or where
    /// the file cannot be read.
    pub fn set(&mut self, at: u64, value: u8, cursor: Cursor) -> bool {
        self.change(at, value, cursor).is_some()
    }

    /// [`HexDoc::set`], saying whether it made an undo step (`Some(false)`: the byte already
    /// showed `value`; `None`: past the end or unreadable).
    fn change(&mut self, at: u64, value: u8, cursor: Cursor) -> Option<bool> {
        self.open_step = None;
        let before = self.byte(at)?;
        if before == value {
            return Some(false);
        }
        // The first edit here: `before` is the file's byte, kept from now on.
        if !self.patches.contains_key(&at) {
            self.replaced.insert(at, before);
        }
        self.patches.insert(at, value);
        self.undo.push_back(Step {
            at,
            before,
            after: value,
            cursor,
        });
        while self.undo.len() > self.undo_cap.max(1) {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.bump();
        Some(true)
    }

    /// The low nibble typed right after the high one: into the high nibble's step when it
    /// made one, else a step of its own.
    fn set_low(&mut self, at: u64, value: u8, cursor: Cursor) -> bool {
        let open =
            self.open_step.take() == Some(at) && self.undo.back().is_some_and(|s| s.at == at);
        if !open {
            return self.set(at, value, cursor);
        }
        let step = self.undo.back_mut().expect("checked above");
        step.after = value;
        if step.before == value {
            self.undo.pop_back();
        }
        self.patches.insert(at, value);
        self.bump();
        true
    }

    /// `c` typed at `cursor`: in the hex column a hex digit overwrites the high nibble (the
    /// cursor stays for the low one), then the low nibble (the cursor moves on) — one undo step
    /// for the two; in the ASCII column a printable ASCII character overwrites the byte and the
    /// cursor moves on. Returns the cursor after it, or `None` when `c` does nothing there.
    pub fn type_char(&mut self, cursor: Cursor, c: char) -> Option<Cursor> {
        let last = self.size().checked_sub(1)?;
        let at = cursor.at.min(last);
        let current = self.byte(at)?;
        let next = Cursor {
            at: (at + 1).min(last),
            low: false,
            ..cursor
        };
        match cursor.pane {
            Pane::Hex => {
                let digit = c.to_digit(16)? as u8;
                if cursor.low {
                    self.set_low(at, (current & 0xf0) | digit, cursor)
                        .then_some(next)
                } else {
                    let made_step = self.change(at, (digit << 4) | (current & 0x0f), cursor)?;
                    if made_step {
                        self.open_step = Some(at);
                    }
                    Some(Cursor {
                        at,
                        low: true,
                        ..cursor
                    })
                }
            }
            Pane::Ascii => {
                if !(' '..='~').contains(&c) {
                    return None;
                }
                self.set(at, c as u8, cursor).then_some(next)
            }
        }
    }

    /// Undoes the last change; returns where the cursor goes.
    pub fn undo(&mut self) -> Option<Cursor> {
        self.open_step = None;
        let step = self.undo.pop_back()?;
        self.patches.insert(step.at, step.before);
        self.redo.push(step);
        self.bump();
        Some(Cursor {
            at: step.at,
            low: false,
            ..step.cursor
        })
    }

    /// Redoes the last undone change; returns where the cursor goes.
    pub fn redo(&mut self) -> Option<Cursor> {
        self.open_step = None;
        let step = self.redo.pop()?;
        self.patches.insert(step.at, step.after);
        self.undo.push_back(step);
        self.bump();
        Some(Cursor {
            at: step.at,
            low: false,
            ..step.cursor
        })
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Something shown changed.
    fn bump(&mut self) {
        self.rev += 1;
        self.generation += 1;
    }

    /// `dirty` and `unsaved`, worked out once per generation.
    fn answers(&mut self) -> (bool, bool) {
        if let Some((generation, dirty, unsaved)) = self.answers {
            if generation == self.generation {
                return (dirty, unsaved);
            }
        }
        let dirty = differ(&self.replaced, &self.patches, &self.saved);
        let unsaved = dirty
            && match &self.copied {
                Some(copied) => differ(&self.replaced, &self.patches, copied),
                None => true,
            };
        self.answers = Some((self.generation, dirty, unsaved));
        (dirty, unsaved)
    }

    /// The bytes differ from what the file holds now (what Save would write) — by content: an
    /// edit undone, or a byte typed back to its value, is no change.
    pub fn dirty(&mut self) -> bool {
        self.answers().0
    }

    /// The bytes are written nowhere — neither the file nor the last Save copy (the close
    /// prompt).
    pub fn unsaved(&mut self) -> bool {
        self.answers().1
    }

    /// A Save of `changes` landed: the file holds them now.
    pub fn mark_saved(&mut self, changes: BTreeMap<u64, u8>) {
        self.saved = changes;
        self.generation += 1;
    }

    /// A Save copy of `changes` landed.
    pub fn mark_copied(&mut self, changes: BTreeMap<u64, u8>) {
        self.copied = Some(changes);
        self.generation += 1;
    }

    /// The file was saved in place and is read through `file` from now on — it holds what was
    /// saved (checked by its hash before this). The edits, and every undo and redo step, are
    /// absolute bytes: they stay as they are. An entry is dropped only where it equals the new
    /// file's byte, read from `file` and kept as the byte an edit there replaces; nothing is
    /// read from the old one. What the last Save copy holds is forgotten, not translated: it
    /// was remembered against the old file (an offset left out meant the old byte), so it no
    /// longer says what the copy holds — closing asks unless the bytes equal the saved file.
    /// All or nothing: a byte `file` cannot give is an error and nothing is changed.
    pub fn rebase(&mut self, file: Arc<File>) -> Result<()> {
        let mut new = pages(Box::new(InPlace(file)), self.pages.size);
        // The new file's byte at every offset looked at: what an edit there replaces from now.
        let mut there_now = HashMap::new();
        let mut keep = |m: &BTreeMap<u64, u8>| -> Result<BTreeMap<u64, u8>> {
            let mut out = BTreeMap::new();
            for (&at, &b) in m {
                let there = new.cell(at).byte().ok_or_else(|| {
                    anyhow!(
                        "the saved file cannot be read at 0x{at:X} ({})",
                        new.error
                            .clone()
                            .unwrap_or_else(|| "no byte there".to_string())
                    )
                })?;
                there_now.insert(at, there);
                if there != b {
                    out.insert(at, b);
                }
            }
            Ok(out)
        };
        let patches = keep(&self.patches)?;
        let saved = keep(&self.saved)?;
        // An offset not looked at holds what it held: the save wrote only the edits.
        self.replaced.extend(there_now);
        new.reads += self.pages.reads;
        self.pages = new;
        self.patches = patches;
        self.saved = saved;
        self.copied = None;
        self.bump();
        Ok(())
    }
}

/// `SOPEN_STATE` for the hex window: the patches (each byte one undo step), then the cursor.
/// A patch waits for its bytes (06 §7.1): from the first byte that is not here yet, the rest
/// are returned in order, for the window to land as their pages arrive.
fn apply_state(doc: &mut HexDoc, hooks: &Hooks) -> Result<(Cursor, VecDeque<(u64, u8)>)> {
    let state = &hooks.state;
    let size = doc.size();
    let mut cursor = Cursor::default();
    let mut waiting = VecDeque::new();
    for (at, bytes) in &state.patches {
        let end = at.checked_add(bytes.len() as u64).unwrap_or(u64::MAX);
        if end > size {
            bail!(
                "SOPEN_STATE patch at 0x{at:X} runs past the end of the file ({} bytes)",
                grouped(size)
            );
        }
        for (i, &b) in bytes.iter().enumerate() {
            let at = at + i as u64;
            if !waiting.is_empty() || doc.cell(at) == Cell::Pending {
                waiting.push_back((at, b));
            } else if !doc.set(at, b, cursor) {
                bail!("SOPEN_STATE patch: cannot read the byte at 0x{at:X}");
            }
        }
    }
    if let Some(at) = state.at {
        if at >= size {
            bail!(
                "SOPEN_STATE at=0x{at:X} is past the end of the file ({} bytes)",
                grouped(size)
            );
        }
        cursor.at = at;
    }
    Ok((cursor, waiting))
}

/// Opens the hex window, or the windowless `SOPEN_AUTOSAVE` run. `notices` (where earlier
/// copies' unsaved edits went) are shown whole in the window's rows.
pub fn open(file: HexFile, hooks: &Hooks, notices: Vec<String>) -> Result<Outcome> {
    let mut doc = HexDoc::of(&file);
    let (cursor, waiting) = apply_state(&mut doc, hooks)?;
    if hooks.autosave {
        // No window, so no frame to wait in: each patch's page is read now.
        for (at, b) in waiting {
            doc.fetch(at)?;
            if !doc.set(at, b, Cursor::default()) {
                bail!("SOPEN_STATE patch: cannot read the byte at 0x{at:X}");
            }
        }
        return autosave(file, &mut doc, hooks);
    }
    let title = file.origin.display();
    if !file.can_replace() {
        eprintln!("sopen: {CANNOT_REPLACE}");
    }
    let outcome: Arc<Mutex<Outcome>> = Arc::default();
    let leftover: Arc<Mutex<Option<Job>>> = Arc::default();
    let mut window = HexWindow::new(
        file,
        doc,
        cursor,
        notices,
        outcome.clone(),
        leftover.clone(),
    );
    window.state_patches = waiting;
    window.ask_close = hooks.state.ask_close;
    window.shoot_loading = hooks.state.loading;
    let run = super::run_window(&title, WINDOW, window, hooks);
    finish(&leftover, &outcome);
    run?;
    let outcome = outcome.lock().unwrap().clone();
    Ok(outcome)
}

/// The hex window for a file the binary dialog opened (the same window switches to it).
pub(super) fn embedded(
    file: HexFile,
    hooks: &Hooks,
    notices: Vec<String>,
    outcome: Arc<Mutex<Outcome>>,
    leftover: Arc<Mutex<Option<Job>>>,
) -> Result<HexWindow> {
    let mut doc = HexDoc::of(&file);
    let (cursor, waiting) = apply_state(&mut doc, hooks)?;
    let mut window = HexWindow::new(file, doc, cursor, notices, outcome, leftover);
    window.state_patches = waiting;
    window.ask_close = hooks.state.ask_close;
    window.shoot_loading = hooks.state.loading;
    Ok(window)
}

/// The hex window inside another window (the file browser): the window [`open`] shows, without
/// the receipt hooks.
pub fn embed(file: HexFile, notices: Vec<String>) -> super::Embedded {
    let outcome: Arc<Mutex<Outcome>> = Arc::default();
    let leftover: Arc<Mutex<Option<Job>>> = Arc::default();
    let doc = HexDoc::of(&file);
    let window = HexWindow::new(
        file,
        doc,
        Cursor::default(),
        notices,
        outcome.clone(),
        leftover.clone(),
    );
    super::Embedded {
        window: Box::new(window),
        settle: Box::new(move |wait| settle(&leftover, &outcome, wait)),
    }
}

/// A save the window left running when it went away, as [`finish`] waits for it: with `wait`
/// waited for, else taken only once it has finished; printed and counted either way. The
/// window's outcome once none runs.
fn settle(leftover: &Mutex<Option<Job>>, outcome: &Mutex<Outcome>, wait: bool) -> Option<Outcome> {
    if wait {
        finish(leftover, outcome);
    } else {
        let mut slot = leftover.lock().unwrap();
        if let Some(job) = slot.as_ref() {
            let result = match job.rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(anyhow!("the save stopped without a result"))
                }
            };
            *slot = None;
            let _ = report(&result, &mut outcome.lock().unwrap());
        }
    }
    Some(outcome.lock().unwrap().clone())
}

/// Never exit with a write in flight without its outcome on the terminal: a save the window
/// handed over when it closed is waited for here.
pub(super) fn finish(leftover: &Mutex<Option<Job>>, outcome: &Mutex<Outcome>) {
    if let Some(job) = leftover.lock().unwrap().take() {
        eprintln!("sopen: the window closed during a save; waiting for it to finish");
        let result = job
            .rx
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the save stopped without a result")));
        let _ = report(&result, &mut outcome.lock().unwrap());
    }
}

fn autosave(mut file: HexFile, doc: &mut HexDoc, hooks: &Hooks) -> Result<Outcome> {
    let mut outcome = Outcome::default();
    if !doc.dirty() {
        println!("no changes; {} untouched", file.origin.display());
        return Ok(outcome);
    }
    if let Some(delay) = hooks.save_delay {
        eprintln!("waiting {} ms before saving", delay.as_millis());
        std::thread::sleep(delay);
    }
    match file.save(doc.patches())? {
        Saved::Written { remote, notes, .. } => {
            outcome.saved = true;
            let n = doc.changed_count();
            println!("saved {remote} ({} changed)", plural_bytes(n as u64));
            for note in notes {
                eprintln!("sopen: {note}");
            }
            if let Some(Rebind::Lost(why)) = file.take_rebind() {
                eprintln!("sopen: {why}");
            }
        }
        Saved::Refused { message, .. } => {
            outcome.conflict = true;
            outcome.refused = Some(message);
        }
    }
    Ok(outcome)
}

fn plural_bytes(n: u64) -> String {
    if n == 1 {
        "1 byte".to_string()
    } else {
        format!("{} bytes", grouped(n))
    }
}

// ---------------------------------------------------------------------------------------------
// The window.

/// Prints a finished save to the terminal, records it in `outcome` and returns the window's
/// one-line status and what must be shown whole (the disclosure rows). Also used for a save
/// still in flight when the window went away.
fn report(result: &Result<JobDone>, outcome: &mut Outcome) -> (Status, Vec<String>) {
    match result {
        Ok(JobDone::Saved(Saved::Written {
            remote,
            backup,
            notes,
        })) => {
            outcome.saved = true;
            outcome.refused = None;
            println!("saved {remote}");
            for note in notes {
                eprintln!("sopen: {note}");
            }
            let status = Status::info(format!(
                "saved {remote} · original backed up at {}",
                backup.display()
            ));
            (status, notes.clone())
        }
        Ok(JobDone::Saved(Saved::Refused { message, .. })) => {
            outcome.conflict = true;
            outcome.refused = Some(message.clone());
            eprintln!("sopen: {message}");
            (
                Status::error("not saved: the file changed"),
                vec![message.clone()],
            )
        }
        Ok(JobDone::Copy(path)) => {
            println!("saved a copy at {path}");
            (Status::info(format!("saved a copy at {path}")), Vec::new())
        }
        // The whole fetch before a streamed file's Save was stopped (Esc, or closing).
        Err(e) if stopped(e) => (Status::info(STOPPED), Vec::new()),
        Err(e) => {
            eprintln!("sopen: {e:#}");
            (Status::error("save failed"), vec![format!("{e:#}")])
        }
    }
}

/// A stopped fetch wrote nothing, anywhere but the cache.
const STOPPED: &str = "stopped: nothing was written";

/// Whether a save ended because its whole fetch was stopped.
fn stopped(e: &anyhow::Error) -> bool {
    e.downcast_ref::<crate::remote::Cancelled>().is_some()
}

/// A reload is on its way: nothing is edited or saved until it has landed.
const RELOADING: &str = "reloading…";
/// How long the scroll bar's thumb rests on a place before it is fetched, in seconds (06 §7.1).
const THUMB_REST: f64 = 0.1;

/// Closing was asked for while a save runs.
const CLOSES_AFTER_SAVE: &str = "the window closes when the save is done";

enum JobKind {
    Save { close_after: bool },
    SaveCopy { close_after: bool },
}

pub(super) struct Job {
    kind: JobKind,
    /// The changes the save writes (what the file holds if it lands).
    snapshot: BTreeMap<u64, u8>,
    rx: Receiver<Result<JobDone>>,
}

enum JobDone {
    Saved(Saved),
    /// The copy's printed path.
    Copy(String),
}

/// Where the grid was last laid out: the rows' rectangle and the character metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    rows: Rect,
    row_h: f32,
    cw: f32,
    hex_x: f32,
    ascii_x: f32,
}

pub(super) struct HexWindow {
    /// `host:/abs`.
    title: String,
    /// False on a server without `posix-rename@openssh.com`: Save is off, Save copy works.
    can_replace: bool,
    doc: HexDoc,
    file: Arc<Mutex<HexFile>>,
    /// The interim Save's whole fetch (06 §7.1), read without the file's lock: its worker
    /// holds that for as long as it runs.
    fetching: Arc<Fetching>,
    cursor: Cursor,
    /// The first row on screen.
    top_row: u64,
    /// Rows that fit on screen (the last pass's).
    visible: u64,
    /// The cursor moved by a key, a click or an edit: scroll it into view on the next pass.
    follow: bool,
    /// Wheel points not yet a whole row (trackpads scroll in points).
    wheel_rest: f32,
    /// The Go to field's text.
    goto: String,
    status: Status,
    job: Option<Job>,
    outcome: Arc<Mutex<Outcome>>,
    prompt: bool,
    close_ok: bool,
    /// Close was asked for while a save ran: close when it is done.
    close_when_done: bool,
    /// Where a save still in flight goes if the window is dropped anyway; its opener waits.
    leftover: Arc<Mutex<Option<Job>>>,
    painted: bool,
    /// The grid as last laid out, and how many rows the last pass painted.
    layout: Option<Layout>,
    rows_painted: u64,
    /// Why the window is read-only (the file saved in place changed right after, or cannot be
    /// read back), and where the edits are kept — the line shown whole in the rows.
    read_only: Option<String>,
    /// The reason alone, for the header of a list of the edits.
    read_only_why: String,
    /// Where the list of the edits was written, once it was.
    retained: Option<PathBuf>,
    /// Where "Write the list there" writes the list of edits.
    pub(super) keep_to: String,
    /// Run before each link that writing the list makes (tests: something put in the private
    /// folder, so its removal fails and is said).
    #[cfg(test)]
    pub(super) keep_link: Option<KeepLink>,
    /// Lines from open that must be read whole and stay (see [`super::disclosures`]).
    opened: Vec<String>,
    /// What the last write reported that must be read whole, and the next such report, held
    /// while the button is down.
    notices: Vec<String>,
    pending_notices: Option<Vec<String>>,
    /// `SOPEN_STATE` `close=ask`: closing is asked for on the first pass.
    ask_close: bool,
    /// `SOPEN_STATE` `loading=1` (06 §11): the window is ready to be shot at its first frame
    /// with rows, those still loading as they are.
    pub(super) shoot_loading: bool,
    /// The stream the file is viewed through (06 §7.1); `None` for a file read in place.
    stream: Option<Source>,
    /// What the stream wakes: this window, from its first pass on.
    repaint: Repaint,
    /// `SOPEN_STATE` patches whose bytes have not arrived, in order.
    state_patches: VecDeque<(u64, u8)>,
    /// A byte typed over while its page was on its way: the status row says so until it is
    /// here or the cursor moves on.
    blocked: Option<u64>,
    /// Cmd/Ctrl+R was pressed and the reload has not landed: nothing is edited or saved. Its
    /// worker waits for the stream (an open and an `fstat` on its connection), not a frame.
    reloading: Option<Receiver<std::result::Result<(), String>>>,
    /// The first row the scroll bar's thumb last put on top, and since when (the window's
    /// clock): a place is fetched once the thumb has rested there (06 §7.1).
    thumb: Option<(u64, f64)>,
    /// The stats line's `first` was marked (06 §11): a frame drew bytes from the stream.
    marked_first: bool,
    /// The screen (its first offset) whose rows are on their way, marked as an `input` (or the
    /// open's own first rows): the frame that shows that screen marks `ready` (or `first`).
    awaiting: Option<u64>,
    /// A preview in the file browser (02 §4.2): its first rows in a single-lined frame, no
    /// toolbar, status row, cursor or disclosures, no input of its own, and it never takes the
    /// keyboard.
    preview: bool,
    /// Saves started so far (tests: a key starts exactly one).
    #[cfg(test)]
    pub(super) save_starts: usize,
}

impl Drop for HexWindow {
    fn drop(&mut self) {
        // A whole fetch still running has written nothing: it is stopped, not waited for.
        if self.fetching.active.load(Ordering::SeqCst) {
            self.fetching.cancel.store(true, Ordering::SeqCst);
        }
        if let Some(job) = self.job.take() {
            *self.leftover.lock().unwrap() = Some(job);
        }
    }
}

/// A key a viewer handles itself (its F-keys) pressed with no modifier at all, consumed: 04 §3's
/// exact matching. egui's `consume_key(Modifiers::NONE, …)` also takes it with Shift or Alt held
/// (it ignores those), so Shift+F5 would save. The image and text windows use it too.
pub(super) fn bare_key(ctx: &egui::Context, key: Key) -> bool {
    ctx.input_mut(|i| {
        let before = i.events.len();
        i.events.retain(|e| {
            !matches!(
                e,
                egui::Event::Key { key: k, pressed: true, modifiers, .. }
                    if *k == key && modifiers.is_none()
            )
        });
        i.events.len() != before
    })
}

fn grid_id() -> egui::Id {
    egui::Id::unique("sopen-hex-grid")
}

fn goto_id() -> egui::Id {
    egui::Id::unique("sopen-hex-goto")
}

/// A stand-in run before each link of writing the list (tests).
#[cfg(test)]
type KeepLink = Box<dyn FnMut(super::binary::LinkStep, &Path) -> std::io::Result<()> + Send>;

/// The read-only window's "Write the list there" path field.
fn keep_id() -> egui::Id {
    egui::Id::unique("sopen-hex-keep-to")
}

impl HexWindow {
    fn new(
        file: HexFile,
        doc: HexDoc,
        cursor: Cursor,
        notices: Vec<String>,
        outcome: Arc<Mutex<Outcome>>,
        leftover: Arc<Mutex<Option<Job>>>,
    ) -> Self {
        let can_replace = file.can_replace();
        let (stream, repaint) = (file.stream(), file.repaint());
        let opened = notices
            .into_iter()
            .chain((!can_replace).then(|| CANNOT_REPLACE.to_string()))
            .collect();
        Self {
            title: file.origin.display(),
            can_replace,
            doc,
            fetching: file.fetching.clone(),
            file: Arc::new(Mutex::new(file)),
            cursor,
            top_row: 0,
            visible: 1,
            follow: true,
            wheel_rest: 0.0,
            goto: String::new(),
            status: Status::default(),
            job: None,
            outcome,
            prompt: false,
            close_ok: false,
            close_when_done: false,
            leftover,
            painted: false,
            layout: None,
            rows_painted: 0,
            read_only: None,
            read_only_why: String::new(),
            retained: None,
            keep_to: String::new(),
            #[cfg(test)]
            keep_link: None,
            opened,
            notices: Vec::new(),
            pending_notices: None,
            ask_close: false,
            shoot_loading: false,
            stream,
            repaint,
            state_patches: VecDeque::new(),
            blocked: None,
            reloading: None,
            thumb: None,
            marked_first: false,
            awaiting: None,
            preview: false,
            #[cfg(test)]
            save_starts: 0,
        }
    }

    fn last(&self) -> Option<u64> {
        self.doc.size().checked_sub(1)
    }

    /// Puts the cursor on `at` (clamped to the file) in `pane`, at the byte's high nibble.
    fn place(&mut self, at: u64, pane: Pane) {
        let Some(last) = self.last() else {
            return;
        };
        self.cursor = Cursor {
            at: at.min(last),
            pane,
            low: false,
        };
        self.follow = true;
    }

    /// Moves the cursor by `delta` bytes, staying in the file.
    fn step(&mut self, delta: i64) {
        self.place(
            self.cursor.at.saturating_add_signed(delta),
            self.cursor.pane,
        );
    }

    /// Cmd/Ctrl+R (06 §5): the stream re-opens the path and the window shows that version at
    /// the same offset. Not with edits written nowhere: they were made over the version shown.
    fn reload(&mut self, ctx: &egui::Context) {
        let Some(source) = self.stream.clone() else {
            return;
        };
        if self.job.is_some() {
            return self.status = Status::info("a save is running");
        }
        if self.reloading.is_some() {
            return;
        }
        if self.doc.unsaved() {
            return self.status = Status::error(
                "not reloaded: your edits are saved nowhere, and a reload drops them — save a \
                 copy or undo them first",
            );
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(source.reload().map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        self.reloading = Some(rx);
        self.status = Status::info(RELOADING);
    }

    /// A reload that has ended: the version open now is shown at the same offset (within its
    /// size). Edits and steps over the one before are gone — none was unsaved — and so is the
    /// copy a Save fetched of it.
    fn poll_reload(&mut self) {
        let Some(rx) = &self.reloading else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("its worker ended".to_string()),
        };
        self.reloading = None;
        match result {
            Ok(()) => {
                self.doc.refresh();
                self.doc.take_renewed();
                self.doc.reset();
                self.file
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .forget_fetched();
                self.cursor.at = self.cursor.at.min(self.doc.size().saturating_sub(1));
                self.cursor.low = false;
                self.status = Status::info("reloaded");
            }
            Err(e) => self.status = Status::error(format!("not reloaded: {e}")),
        }
    }

    /// The frame's one read of the stream (06 §7.1), for the `len` bytes at `off` the screen
    /// shows; `on_bar` while the button is down on the scroll bar. Then what arrived is used:
    /// `SOPEN_STATE` patches land, a reload that landed is taken, and the stats line's events
    /// are marked.
    fn read_screen(&mut self, ctx: &egui::Context, off: u64, len: u64, on_bar: bool) {
        let Some(source) = self.stream.clone() else {
            return self.doc.view(off, len, Ask::Read);
        };
        self.repaint.bind(ctx);
        self.poll_reload();
        let state = source.state();
        let failed = matches!(state, crate::stream::State::Failed(_));
        let ask = if on_bar {
            // A place the thumb passes is not fetched: only where it has rested 100 ms, or
            // where it is let go (the next frame, off the bar).
            let now = ctx.input(|i| i.time);
            let since = match self.thumb {
                Some((row, since)) if row == self.top_row => since,
                _ => now,
            };
            self.thumb = Some((self.top_row, since));
            let rested = now - since;
            if rested >= THUMB_REST {
                Ask::Read
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(THUMB_REST - rested));
                Ask::Wait
            }
        } else {
            self.thumb = None;
            // A failed stream is retried by the next key, click or scroll (06 §5), not by a
            // frame the stream itself woke.
            let input = ctx.input(|i| i.events.iter().any(super::user_input));
            if failed && !input {
                Ask::Failed
            } else {
                Ask::Read
            }
        };
        let before = (self.doc.pending_at(), self.blocked);
        // A `SOPEN_STATE` patch waiting for its byte has the frame's read; the screen's rows
        // come once the patches are in.
        if let Some(&(at, _)) = self.state_patches.front() {
            self.doc.view(at, 1, ask);
            while let Some(&(at, b)) = self.state_patches.front() {
                if !self.doc.set(at, b, Cursor::default()) {
                    break;
                }
                self.state_patches.pop_front();
            }
        }
        if self.state_patches.is_empty() {
            self.doc.view(off, len, ask);
        }
        if self.doc.take_renewed() {
            // A read found the stream reloaded (a Save's own reload, ahead of its report): the
            // version open now is read from here on, and the frame after this one shows it.
            ctx.request_repaint();
        }
        if self
            .blocked
            .is_some_and(|at| at != self.cursor.at || self.doc.cell(at) != Cell::Pending)
        {
            self.blocked = None;
        }
        // The status row was laid out before this read: when what it says changed, the frame
        // is painted once more.
        if before != (self.doc.pending_at(), self.blocked) {
            ctx.request_repaint();
        }
        // The stats line's events (06 §11): `first` at the first frame that draws bytes of the
        // stream, `input` when the view moves to rows not here yet, `ready` when they show.
        if len == 0 || !self.state_patches.is_empty() {
            return;
        }
        let here = self.doc.pending_at().is_none() && self.doc.cell(off) != Cell::Unreadable;
        let on_their_way = self.doc.pending_at().is_some();
        // Each place asked for is an input of its own: a second jump before the first has
        // arrived is marked too, and a frame that stays where it is marks nothing. A place the
        // thumb only passes is not asked for, so it is not one.
        if on_their_way && ask == Ask::Read && self.awaiting != Some(off) {
            self.awaiting = Some(off);
            if self.marked_first {
                let place = match self.cursor.at {
                    at if (off..off + len).contains(&at) => at,
                    _ => off,
                };
                source.mark(Mark::Input(place));
            }
        }
        if here && !self.marked_first {
            self.marked_first = true;
            self.awaiting = None;
            source.mark(Mark::First);
        } else if here && self.awaiting.take() == Some(off) {
            // The screen waited for is the one shown. Rows that were held all along (the view
            // went back to them) end the wait without a `ready`: the place never showed.
            source.mark(Mark::Ready);
        }
    }

    /// What the stream says on the status row while rows are on their way (06 §5, §7.1):
    /// `loading 0x…`, with its size once it has waited a second, `the remote is slow` after
    /// ten. The frame is painted again when the line is due to change.
    fn loading_line(&self, ctx: &egui::Context) -> Option<String> {
        use std::time::Duration;
        let at = self.doc.pending_at()?;
        let Some(wait) = self.doc.waiting() else {
            return Some(format!("loading 0x{at:X}"));
        };
        let (named, slow) = (Duration::from_secs(1), Duration::from_secs(10));
        if wait.waited >= slow {
            return Some("the remote is slow".to_string());
        }
        let next = if wait.waited >= named { slow } else { named };
        ctx.request_repaint_after(next - wait.waited + Duration::from_millis(10));
        Some(if wait.waited >= named {
            format!("loading 0x{at:X} ({} KiB)", wait.len.div_ceil(1024))
        } else {
            format!("loading 0x{at:X}")
        })
    }

    fn undo(&mut self) {
        if let Some(cursor) = self.doc.undo() {
            self.cursor = cursor;
            self.follow = true;
            self.status = Status::info("undone");
        }
    }

    fn redo(&mut self) {
        if let Some(cursor) = self.doc.redo() {
            self.cursor = cursor;
            self.follow = true;
            self.status = Status::info("redone");
        }
    }

    fn goto(&mut self) {
        let size = self.doc.size();
        match parse_offset(&self.goto) {
            Ok(at) if at < size => {
                self.place(at, self.cursor.pane);
                self.status = Status::info(format!("at 0x{at:X} ({at})"));
            }
            Ok(at) => {
                self.status = Status::error(format!(
                    "0x{at:X} is past the end of the file ({} bytes)",
                    grouped(size)
                ))
            }
            Err(e) => self.status = Status::error(format!("{e:#}")),
        }
    }

    fn start(&mut self, ctx: &egui::Context, kind: JobKind) {
        if self.job.is_some() {
            return;
        }
        if let Some(why) = &self.read_only {
            return self.status = Status::error(why.clone());
        }
        if self.reloading.is_some() {
            return self.status = Status::info(RELOADING);
        }
        if matches!(kind, JobKind::Save { .. }) {
            if !self.can_replace {
                return self.status = Status::error(CANNOT_REPLACE);
            }
            if !self.doc.dirty() {
                return self.status = Status::info("no changes to save");
            }
        }
        #[cfg(test)]
        {
            self.save_starts += 1;
        }
        let patches = self.doc.patches().clone();
        let snapshot = patches.clone();
        let file = self.file.clone();
        let copy = matches!(kind, JobKind::SaveCopy { .. });
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // A save whose worker died holding the file leaves it as it was: usable.
            let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = if copy {
                file.save_copy(&patches).map(JobDone::Copy)
            } else {
                file.save(&patches).map(JobDone::Saved)
            };
            drop(file);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.status = Status::info(if copy {
            "saving a copy…"
        } else {
            "saving…"
        });
        self.job = Some(Job { kind, snapshot, rx });
    }

    /// A finished save, taken after this frame's input (an edit or an undo in the same frame is
    /// in the comparison, never lost to a close). Closing — asked for in the prompt, or during
    /// the save — happens only when the save landed with nothing to read and nothing is unsaved;
    /// a save that did not land, or landed with notes, keeps the window open; newer edits ask.
    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow!(
                "the save stopped without a result (its worker ended); your edits are still \
                 here"
            )),
        };
        let job = self.job.take().expect("checked above");
        let (status, shown) = report(&result, &mut self.outcome.lock().unwrap());
        self.status = status;
        if self.stream.is_some() {
            let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
            // What the whole fetch set aside stays on screen, as an open's notices do.
            self.opened.extend(file.take_fetch_notices());
            // Once the `Origin` has connected, whether this server can replace a file is known.
            let can = file.can_replace();
            if self.can_replace && !can {
                self.opened.push(CANNOT_REPLACE.to_string());
            }
            self.can_replace = can;
        }
        // An edit made while the save ran is not in what it wrote.
        let newer = *self.doc.patches() != job.snapshot;
        let landed = match &result {
            Ok(JobDone::Saved(Saved::Written { .. })) => {
                self.doc.mark_saved(job.snapshot.clone());
                if self.stream.is_some() {
                    // The save reloaded the stream onto the saved file (06 §5): what is held
                    // is of the file before.
                    self.doc.refresh();
                    ctx.request_repaint();
                }
                let rebind = self
                    .file
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take_rebind();
                match rebind {
                    Some(Rebind::Rebound(file)) => {
                        if let Err(e) = self.doc.rebase(file) {
                            self.turn_read_only(&format!("{e:#}"));
                        }
                    }
                    Some(Rebind::Lost(why)) => self.turn_read_only(&why),
                    None => {}
                }
                true
            }
            Ok(JobDone::Copy(..)) => {
                self.doc.mark_copied(job.snapshot.clone());
                true
            }
            _ => false,
        };
        self.pending_notices = Some(shown);
        let notes = matches!(&result, Ok(JobDone::Saved(Saved::Written { notes, .. })) if !notes.is_empty())
            || self.read_only.is_some();
        // A close asked for during this save is used up by it, whatever it reports.
        let waited = std::mem::take(&mut self.close_when_done);
        let asked = waited
            || matches!(
                job.kind,
                JobKind::Save { close_after: true } | JobKind::SaveCopy { close_after: true }
            );
        // Closing stopped the whole fetch: nothing was saved, so closing asks, as it would
        // have without a save running.
        if waited && matches!(&result, Err(e) if stopped(e)) {
            if self.doc.unsaved() {
                self.prompt = true;
            } else {
                self.close_ok = true;
                super::request_close(ctx);
            }
            return;
        }
        if !asked || !landed {
            return;
        }
        if (newer || self.doc.unsaved()) && self.read_only.is_none() {
            if newer {
                self.status =
                    Status::info(format!("newer edits are not saved · {}", self.status.text));
            }
            self.prompt = true;
        } else if notes {
            self.status = Status::info(format!(
                "not closed: read the notes below · {}",
                self.status.text
            ));
        } else {
            self.close_ok = true;
            super::request_close(ctx);
        }
    }

    /// Whether input may change anything: not under the close prompt, not once closing is
    /// decided, not once the window is read-only.
    fn locked(&self) -> bool {
        self.prompt || self.close_ok || self.read_only.is_some()
    }

    /// Nothing more is edited or saved here (the file saved in place no longer holds what was
    /// saved, or cannot be read back): the edits are listed in a file of their own — or, when
    /// that fails, the window says so and offers to try again or to write the list elsewhere,
    /// and closing asks while they are unsaved.
    fn turn_read_only(&mut self, why: &str) {
        self.file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_read_only(why);
        self.read_only_why = why.to_string();
        if self.keep_to.is_empty() {
            let name = self
                .file
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .name();
            let dir = super::binary::download_dir();
            self.keep_to = super::binary::tilde(&dir.join(format!("{name}.hexpatch.txt")));
        }
        self.read_only = Some(format!("read-only now: {why}"));
        self.keep_edits();
    }

    /// Lists the edits among sopen's kept edits ("Keep edits…").
    fn keep_edits(&mut self) {
        let listed = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keep_patches(self.doc.patches(), &self.read_only_why);
        self.kept(
            listed
                .map(|p| (p, Vec::new()))
                .map_err(|e| format!("{e:#}")),
        );
    }

    /// Writes the list of the edits to the path in the field, as Save to… writes: a new file,
    /// never over anything ("Write the list there").
    fn keep_edits_at(&mut self) {
        let mut dest = super::binary::untilde(&self.keep_to);
        if dest.is_dir() {
            let name = self
                .file
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .name();
            dest = dest.join(format!("{name}.hexpatch.txt"));
        }
        let text = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .list_text(self.doc.patches(), &self.read_only_why);
        let fill = |f: &mut File| -> Result<u64> {
            f.write_all(text.as_bytes())?;
            Ok(text.len() as u64)
        };
        #[cfg(test)]
        let saved = match self.keep_link.as_mut() {
            Some(link) => super::binary::save_new_with(&dest, fill, link.as_mut()),
            None => super::binary::save_new(&dest, fill),
        };
        #[cfg(not(test))]
        let saved = super::binary::save_new(&dest, fill);
        let result = match saved {
            Ok(saved) => Ok((dest, saved.notes)),
            Err(super::binary::SaveError::Exists { next }) => {
                self.keep_to = super::binary::tilde(&next);
                Err(format!(
                    "{} exists — not replaced; the field now holds the next free name",
                    dest.display()
                ))
            }
            Err(e) => Err(e.to_string()),
        };
        self.kept(result);
    }

    /// Says where the edits are kept now, or why they are not.
    fn kept(&mut self, result: std::result::Result<(PathBuf, Vec<String>), String>) {
        let why = &self.read_only_why;
        let line = match result {
            Ok((path, notes)) => {
                let line = super::with_notes(
                    &format!(
                        "read-only now: {why}; your edits are listed in {}",
                        path.display()
                    ),
                    &notes,
                );
                self.retained = Some(path);
                line
            }
            Err(e) => format!(
                "read-only now: {why}; your edits could not be listed ({e}) — Keep edits… tries \
                 again, or write the list to a path of your choice"
            ),
        };
        eprintln!("sopen: {line}");
        self.read_only = Some(line);
    }

    /// The row under the read-only line while the edits are not kept anywhere.
    fn keep_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("Keep edits…")
                .on_hover_text("writes the list of your edits among sopen's kept edits")
                .clicked()
            {
                self.keep_edits();
            }
            let room = (ui.available_width() - 170.0).max(120.0);
            ui.add(
                chrome::field(&mut self.keep_to)
                    .id(keep_id())
                    .desired_width(room),
            );
            if ui
                .button("Write the list there")
                .on_hover_text("a new file at that path, never over an existing one")
                .clicked()
            {
                self.keep_edits_at();
            }
        });
    }

    /// Keys for the whole window. `goto_owns` = the Go to field has this frame's typing (it is
    /// focused, was clicked, lost focus on Enter, or Cmd/Ctrl+G gave it the keyboard): then no
    /// byte is typed and no key moves the cursor.
    fn keys(&mut self, ctx: &egui::Context, goto_owns: bool) {
        use super::command_key;
        // Esc stops a streamed file's whole fetch (06 §8): nothing was written yet.
        if self.fetching.active.load(Ordering::SeqCst)
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.fetching.cancel.store(true, Ordering::SeqCst);
        }
        if self.locked() {
            return;
        }
        // Only a streamed file reloads: without a stream the key is not this window's to take
        // (the file browser around it has its own Cmd/Ctrl+R).
        if self.stream.is_some() && command_key(ctx, Key::R, false) {
            self.reload(ctx);
        }
        if command_key(ctx, Key::S, true) {
            self.start(ctx, JobKind::SaveCopy { close_after: false });
        } else if command_key(ctx, Key::S, false) {
            self.start(ctx, JobKind::Save { close_after: false });
        }
        if goto_owns {
            return;
        }
        // F5 is PC Shell's Save, as Cmd/Ctrl+S — but a plain key: never while a field has
        // the typing.
        if bare_key(ctx, Key::F5) {
            self.start(ctx, JobKind::Save { close_after: false });
        }
        if self.reloading.is_some() {
            // Nothing is edited until the reload has landed; the cursor still moves.
        } else if command_key(ctx, Key::Z, true) {
            self.redo();
        } else if command_key(ctx, Key::Z, false) {
            self.undo();
        }
        let Some(last) = self.last() else {
            return;
        };
        let page = (self.visible.max(1) * ROW) as i64;
        let row_start = self.cursor.at - self.cursor.at % ROW;
        let mods = egui::Modifiers::NONE;
        let (home, end) = (
            command_key(ctx, Key::Home, false),
            command_key(ctx, Key::End, false),
        );
        if home {
            self.place(0, self.cursor.pane);
        }
        if end {
            self.place(last, self.cursor.pane);
        }
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(mods, key));
        if pressed(Key::ArrowLeft) {
            self.step(-1);
        }
        if pressed(Key::ArrowRight) {
            self.step(1);
        }
        if pressed(Key::ArrowUp) {
            self.step(-(ROW as i64));
        }
        if pressed(Key::ArrowDown) {
            self.step(ROW as i64);
        }
        if pressed(Key::PageUp) {
            self.step(-page);
        }
        if pressed(Key::PageDown) {
            self.step(page);
        }
        if pressed(Key::Home) {
            self.place(row_start, self.cursor.pane);
        }
        if pressed(Key::End) {
            self.place(row_start + ROW - 1, self.cursor.pane);
        }
        // F8 is PC Shell's Asc/hx: the typing column, as Tab.
        if pressed(Key::Tab) || bare_key(ctx, Key::F8) {
            let pane = match self.cursor.pane {
                Pane::Hex => Pane::Ascii,
                Pane::Ascii => Pane::Hex,
            };
            self.place(self.cursor.at, pane);
        }
        let typed: Vec<char> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) => Some(t.chars().collect::<Vec<_>>()),
                    _ => None,
                })
                .flatten()
                .collect()
        });
        for c in typed {
            if self.reloading.is_some() {
                self.status = Status::info(RELOADING);
                break;
            }
            // A byte whose page is on its way cannot be typed over (06 §7.1): the key does
            // nothing, the status row says so, and the cursor's row comes on screen, which
            // is what fetches it.
            let at = self.cursor.at.min(last);
            if self.doc.cell(at) == Cell::Pending {
                self.blocked = Some(at);
                self.follow = true;
                break;
            }
            match self.doc.type_char(self.cursor, c) {
                Some(next) => {
                    self.cursor = next;
                    self.follow = true;
                }
                None if self.cursor.pane == Pane::Ascii && !c.is_control() => {
                    self.status = Status::error(format!(
                        "'{c}' is not printable ASCII; the ASCII column takes ' ' to '~'"
                    ));
                }
                None => {}
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let ctx = ui.ctx().clone();
        // A save or a reload under way: no other one, and (a reload) no edit, starts.
        let busy = self.job.is_some() || self.reloading.is_some();
        let free = !self.locked() && self.reloading.is_none();
        // One row, whatever the state: nothing in it comes, goes or changes its label.
        ui.horizontal(|ui| {
            ui.label("Go to");
            let field = ui
                .add(
                    chrome::field(&mut self.goto)
                        .id(goto_id())
                        .hint_text("0x… or decimal")
                        .desired_width(130.0),
                )
                .on_hover_text("⌘/Ctrl+G · Enter goes there");
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.goto();
            }
            ui.separator();
            if ui
                .add_enabled(free && self.doc.can_undo(), egui::Button::new("Undo"))
                .on_hover_text("⌘/Ctrl+Z")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(free && self.doc.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("⇧⌘/Shift+Ctrl+Z")
                .clicked()
            {
                self.redo();
            }
            ui.separator();
            let save = ui
                .add_enabled(
                    free && !busy && self.doc.dirty() && self.can_replace,
                    egui::Button::new("Save"),
                )
                .on_hover_text("⌘/Ctrl+S — overwrites the original after a local backup")
                .on_disabled_hover_text(if !self.can_replace {
                    CANNOT_REPLACE
                } else if self.reloading.is_some() {
                    RELOADING
                } else if busy {
                    "a save is running"
                } else {
                    "no changes to save"
                });
            if save.clicked() {
                self.start(&ctx, JobKind::Save { close_after: false });
            }
            let copy = ui
                .add_enabled(free && !busy, egui::Button::new("Save copy"))
                .on_hover_text("⇧⌘/Shift+Ctrl+S — a new <name>.edited file next to the original")
                .on_disabled_hover_text("a save is running");
            if copy.clicked() {
                self.start(&ctx, JobKind::SaveCopy { close_after: false });
            }
            field
        })
        .inner
    }

    /// The cursor's offset and byte, as the status bar shows them.
    pub(super) fn cursor_line(&mut self) -> String {
        if self.doc.size() == 0 {
            return "empty file".to_string();
        }
        let at = self.cursor.at;
        let byte = match self.doc.cell(at) {
            Cell::Byte(b @ 0x20..=0x7e) => format!("0x{b:02X} {b} '{}'", b as char),
            Cell::Byte(b) => format!("0x{b:02X} {b}"),
            Cell::Pending => "··".to_string(),
            Cell::Unreadable => "unreadable".to_string(),
        };
        format!("0x{at:X} ({at}) · {byte}")
    }

    /// What the status row says now: a whole fetch's progress, else a reload under way, else a
    /// key that did nothing because its byte is loading, else an error already there, else
    /// rows on their way, else what the window last said. The stream's own warning is not
    /// here: see [`HexWindow::stream_warning`].
    pub(super) fn status_text(&mut self, ctx: &egui::Context) -> Status {
        if self.fetching.active.load(Ordering::SeqCst) {
            let done = self.fetching.done.load(Ordering::SeqCst);
            let total = self.fetching.total.load(Ordering::SeqCst);
            let percent = (done.saturating_mul(100)).checked_div(total).unwrap_or(0);
            return Status::info(format!(
                "fetching the file to save it — {percent} % ({} of {}) · Esc stops",
                crate::human(done),
                crate::human(total)
            ));
        }
        if self.reloading.is_some() {
            return Status::info(RELOADING);
        }
        if let Some(at) = self.blocked {
            return Status::info(format!("0x{at:X} is still loading"));
        }
        if self.status.error {
            return self.status.clone();
        }
        match self.loading_line(ctx) {
            Some(line) => Status::info(line),
            None => self.status.clone(),
        }
    }

    /// The stream's warning (06 §5: replaced, changed, failed), for as long as that is its
    /// state. It is shown whole in the rows below the grid, where no line of the status row can
    /// take it off or cut its reload hint short.
    pub(super) fn stream_warning(&self) -> Option<String> {
        self.stream.as_ref().and_then(|s| s.state().status())
    }

    /// One line whatever it says (see [`super::one_line`]): the grid above must not move.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let cursor = self.cursor_line();
        let size = self.doc.size();
        let changed = self.doc.changed_count() as u64;
        let unsaved = self.doc.unsaved();
        if let Some(e) = self.doc.read_error() {
            if !self.status.error {
                self.status = Status::error(e.to_string());
            }
        }
        let status = self.status_text(ui.ctx());
        let shown = self.job.is_some() || !status.text.is_empty();
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(cursor).monospace()).extend());
            ui.separator();
            ui.add(egui::Label::new(crate::human(size)).extend())
                .on_hover_text(format!("{} bytes", grouped(size)));
            ui.separator();
            ui.add(egui::Label::new(format!("{} changed", plural_bytes(changed))).extend());
            if unsaved {
                ui.separator();
                ui.add(egui::Label::new("unsaved").extend());
            }
            if shown {
                ui.separator();
            }
            if self.job.is_some() {
                // No taller than the status text beside it, so the bar keeps its height.
                let size = ui.text_style_height(&egui::TextStyle::Body);
                ui.add(egui::Spinner::new().size(size));
            }
            status.show(ui);
        });
    }

    /// The window, as PC Shell's File Edit: its frame, the file on its first line, then the
    /// rows — offsets, hex, ASCII, the cursor in both columns, changed bytes marked — and the
    /// scroll bar on its right border. Only the rows on screen are read and painted.
    fn grid(&mut self, ui: &mut egui::Ui, goto_focused: bool) {
        let ctx = ui.ctx().clone();
        let outer = ui.max_rect();
        let p = chrome::Palette::of(ui.visuals());
        // Single-lined in a preview: it does not have the keyboard.
        let panel = chrome::frame(&ui.painter_at(outer), outer, TITLE, !self.preview);
        let font = chrome::mono(ui);
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        // Rows touch, a cell apart, as on a text screen.
        let row_h = text_h.ceil();
        let bar = Rect::from_min_max(
            Pos2::new(panel.right(), panel.top()),
            Pos2::new(panel.right() + chrome::CELL.x, panel.bottom()),
        );
        let size = self.doc.size();
        let offset_chars = offset_text(0, size).chars().count();
        // Bytes a row: sixteen. A preview (02 §4.2) fits its rows to the side it is in: sixteen
        // where the ASCII column still fits, else eight.
        let per = if self.preview && !row_fits(panel.width(), cw, offset_chars, ROW) {
            ROW / 2
        } else {
            ROW
        };
        // The file's name on the first line, a blank line, then the rows. A preview has no
        // bars: the side's frame names the file.
        let head = if self.preview { 0.0 } else { 2.0 * row_h };
        let rows_rect = Rect::from_min_max(
            Pos2::new(panel.left(), (panel.top() + head).min(panel.bottom())),
            panel.max,
        );
        let total = size.div_ceil(per);
        let visible = ((rows_rect.height() / row_h).floor() as u64).max(1);
        // The grid got shorter (a warning or a notice took rows off its bottom, the window
        // shrank) and the cursor's row, on screen until now, would fall off: the grid scrolls
        // down by just the rows that keep it whole. A cursor already in view moves nothing, and
        // one scrolled away from is not pulled back.
        let was = std::mem::replace(&mut self.visible, visible);
        if !self.preview && self.painted && visible < was && size > 0 {
            let row = self.cursor.at / ROW;
            if (self.top_row..self.top_row + was).contains(&row) && row >= self.top_row + visible {
                self.top_row = row + 1 - visible;
            }
        }
        let max_top = total.saturating_sub(visible);

        // A preview takes no click, drag or wheel, and never the keyboard (it belongs to the
        // list beside it, 02 §4.4).
        let sense = if self.preview {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        };
        let resp = ui.interact(rows_rect, grid_id(), sense);
        let bar_resp = ui.interact(bar, grid_id().with("bar"), sense);
        // The grid holds the keyboard (arrows and Tab act on the cursor, not on focus), except
        // while a field (Go to, the read-only path field) or the close prompt has it.
        if !goto_focused && !self.prompt && !self.preview {
            if ctx.memory(|m| m.focused()) != Some(grid_id()) {
                resp.request_focus();
            }
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    grid_id(),
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        // Wheel / trackpad: whole rows; a line is three.
        if !self.preview && (resp.hovered() || bar_resp.hovered()) {
            let mut rows = 0f32;
            ctx.input(|i| {
                for event in &i.events {
                    if let egui::Event::MouseWheel { unit, delta, .. } = event {
                        rows -= match unit {
                            egui::MouseWheelUnit::Line => delta.y * 3.0,
                            egui::MouseWheelUnit::Page => delta.y * visible as f32,
                            egui::MouseWheelUnit::Point => delta.y / row_h,
                        };
                    }
                }
            });
            if rows != 0.0 {
                let rows = rows + self.wheel_rest;
                let whole = rows.trunc();
                self.wheel_rest = rows - whole;
                let moved = whole as i64;
                self.top_row = if moved < 0 {
                    self.top_row.saturating_sub(moved.unsigned_abs())
                } else {
                    self.top_row.saturating_add(moved as u64)
                };
            }
        }
        // The scroll bar: the thumb (a cell, between the arrows) follows the pointer while the
        // button is down on the bar.
        if bar_resp.is_pointer_button_down_on() && max_top > 0 {
            if let Some(at) = bar_resp.interact_pointer_pos() {
                let cell = chrome::CELL.y;
                let room = (bar.height() - 3.0 * cell).max(1.0);
                let frac = ((at.y - bar.top() - 1.5 * cell) / room).clamp(0.0, 1.0);
                self.top_row = (f64::from(frac) * max_top as f64).round() as u64;
            }
        }

        let hex_x = rows_rect.left() + cw * (offset_chars + 1) as f32;
        let ascii_x = hex_x + cw * (per * 3 + 2) as f32;
        // A click places the cursor on the byte under it, in that column.
        if resp.clicked() || resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let row = self.top_row + ((p.y - rows_rect.top()) / row_h).max(0.0) as u64;
                let hit = if p.x >= ascii_x - cw {
                    let i = ((p.x - ascii_x) / cw).max(0.0) as u64;
                    Some((i.min(ROW - 1), Pane::Ascii))
                } else if p.x >= hex_x - cw / 2.0 {
                    let c = ((p.x - hex_x) / cw).max(0.0) as u64;
                    let i = if c < 24 {
                        c / 3
                    } else {
                        8 + c.saturating_sub(25) / 3
                    };
                    Some((i.min(ROW - 1), Pane::Hex))
                } else {
                    None
                };
                if let Some((i, pane)) = hit {
                    self.place(row * ROW + i, pane);
                }
            }
        }
        // On the window's first pass the rows below the grid have not taken their height yet:
        // the cursor is followed again on the next one, or `at=` would leave its row half cut.
        let follow = self.follow;
        self.follow &= !self.painted;
        if follow && size > 0 {
            let row = self.cursor.at / ROW;
            if row < self.top_row {
                self.top_row = row;
            } else if row >= self.top_row + visible {
                self.top_row = row + 1 - visible;
            }
        }
        self.top_row = self.top_row.min(max_top);

        let rows = ((rows_rect.height() / row_h).ceil() as u64).min(total - self.top_row);
        // The frame's one read, now that the rows on screen are known.
        let screen = self.top_row * per;
        let on_bar = bar_resp.is_pointer_button_down_on();
        self.read_screen(&ctx, screen, (rows * per).min(size - screen), on_bar);

        let visuals = ui.visuals().clone();
        // The rows never run under the scroll bar (on a narrow window the ASCII column is cut).
        let painter = ui.painter_at(panel);
        if !self.preview {
            let title = chrome::fit_left(&ctx, &self.title, &font, panel.width());
            painter.text(
                panel.min,
                egui::Align2::LEFT_TOP,
                title,
                font.clone(),
                p.text,
            );
        }
        if size == 0 {
            painter.text(
                rows_rect.center(),
                egui::Align2::CENTER_CENTER,
                "empty file — nothing to show",
                font.clone(),
                p.dim,
            );
        }
        let text = p.text;
        let (changed_fg, changed_bg) = (p.changed, p.changed_back);
        let cursor_bg = visuals.selection.bg_fill;
        // No cursor in a preview: nothing there is being edited.
        let cursor_at = (!self.preview).then_some(self.cursor.at);
        let cursor_line = egui::Stroke::new(1.0, visuals.selection.stroke.color);
        for k in 0..rows {
            let row = self.top_row + k;
            let y = rows_rect.top() + k as f32 * row_h;
            let off = row * per;
            let n = (size - off).min(per);
            painter.text(
                Pos2::new(rows_rect.left(), y),
                egui::Align2::LEFT_TOP,
                offset_text(off, size),
                font.clone(),
                p.dim,
            );
            let cell = |x: f32, chars: f32| {
                Rect::from_min_size(
                    Pos2::new(x - 1.0, y - 1.0),
                    Vec2::new(cw * chars + 2.0, row_h),
                )
            };
            let mut hex_job = egui::text::LayoutJob::default();
            let mut ascii_job = egui::text::LayoutJob::default();
            let fmt = |color: Color32| egui::TextFormat::simple(font.clone(), color);
            for i in 0..n {
                let at = off + i;
                let shown = self.doc.cell(at);
                let changed = self.doc.changed(at);
                let color = if Some(at) == cursor_at {
                    p.bar_text
                } else if changed {
                    changed_fg
                } else if shown == Cell::Pending {
                    // On its way (06 §7.1): `··` in the skin's dim colour, never a blank.
                    p.dim
                } else {
                    text
                };
                let hx = hex_x + cw * hex_col(i) as f32;
                let ax = ascii_x + cw * i as f32;
                if changed {
                    painter.rect_filled(cell(hx, 2.0), 0.0, changed_bg);
                    painter.rect_filled(cell(ax, 1.0), 0.0, changed_bg);
                }
                if Some(at) == cursor_at {
                    let (active, other) = match self.cursor.pane {
                        Pane::Hex => (cell(hx, 2.0), cell(ax, 1.0)),
                        Pane::Ascii => (cell(ax, 1.0), cell(hx, 2.0)),
                    };
                    painter.rect_filled(active, 0.0, cursor_bg);
                    painter.rect_stroke(other, 0.0, cursor_line, egui::StrokeKind::Inside);
                    if self.cursor.pane == Pane::Hex && self.cursor.low {
                        // The next digit goes into the low nibble: underline it.
                        let x = hx + cw;
                        painter.line_segment(
                            [
                                Pos2::new(x, y + row_h - 3.0),
                                Pos2::new(x + cw, y + row_h - 3.0),
                            ],
                            egui::Stroke::new(2.0, text),
                        );
                    }
                }
                if i > 0 {
                    hex_job.append(if i == 8 { "  " } else { " " }, 0.0, fmt(text));
                }
                let (digits_text, glyph) = match shown {
                    Cell::Byte(b) => (format!("{b:02X}"), ascii_char(Some(b))),
                    Cell::Pending => ("··".to_string(), '·'),
                    Cell::Unreadable => ("??".to_string(), ascii_char(None)),
                };
                hex_job.append(&digits_text, 0.0, fmt(color));
                ascii_job.append(&glyph.to_string(), 0.0, fmt(color));
            }
            let hex_galley = ctx.fonts_mut(|f| f.layout_job(hex_job));
            let ascii_galley = ctx.fonts_mut(|f| f.layout_job(ascii_job));
            painter.galley(Pos2::new(hex_x, y), hex_galley, text);
            painter.galley(Pos2::new(ascii_x, y), ascii_galley, text);
        }
        self.rows_painted = rows;
        chrome::scroll_bar(
            &ui.painter_at(outer),
            bar,
            (max_top > 0).then(|| (self.top_row as f64 / max_top as f64) as f32),
        );
        self.layout = Some(Layout {
            rows: rows_rect,
            row_h,
            cw,
            hex_x,
            ascii_x,
        });
        self.painted = true;
    }

    fn close_prompt(&mut self, ctx: &egui::Context) {
        if !self.prompt {
            return;
        }
        let id = egui::Id::unique("sopen-hex-unsaved");
        let modal = super::chrome::dialog(ctx, id, "Unsaved changes", |ui| {
            ui.set_width(360.0);
            let read_only = self.read_only.is_some();
            ui.label(if read_only {
                format!(
                    "Your edits to {} are kept nowhere and cannot be saved here; close anyway?",
                    self.title
                )
            } else {
                format!("Save the changes to {} before closing?", self.title)
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if read_only {
                } else if self.can_replace && ui.button("Save").clicked() {
                    self.prompt = false;
                    self.start(ctx, JobKind::Save { close_after: true });
                }
                if !read_only && !self.can_replace && ui.button("Save copy").clicked() {
                    self.prompt = false;
                    self.start(ctx, JobKind::SaveCopy { close_after: true });
                }
                if ui.button("Discard").clicked() {
                    self.prompt = false;
                    self.close_ok = true;
                    super::request_close(ctx);
                }
                if ui.button("Cancel").clicked() {
                    self.prompt = false;
                }
            });
        });
        if modal.should_close() {
            self.prompt = false;
        }
    }
}

impl super::Window for HexWindow {
    /// One pass, in this order: Cmd/Ctrl+G (the Go to field takes the keyboard before it is
    /// laid out, so this frame's typing goes there), the toolbar, which then knows whether the
    /// field has this frame's typing, the keys and typing (to exactly one place: the field or
    /// the bytes), the panels and the grid, and last a finished save — after every input of
    /// this frame, so an edit made in the same frame counts.
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.preview {
            // The rows alone (02 §4.2): no Go to, keys, typing, toolbar, rows below or prompt.
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.grid(ui, false));
            return;
        }
        if self.state_patches.is_empty() && std::mem::take(&mut self.ask_close) {
            // `SOPEN_STATE` `close=ask`: as if closing were asked for (the prompt's receipt),
            // once the state's patches are in.
            self.close_requested();
        }
        // F6 is PC Shell's Goto, as Cmd/Ctrl+G — but a plain key: not while a field (Go to,
        // the read-only path field) has the typing.
        let cmd_g = !self.locked()
            && (super::command_key(&ctx, Key::G, false)
                || (!ctx.text_edit_focused() && bare_key(&ctx, Key::F6)));
        if cmd_g {
            self.goto.clear();
            ctx.memory_mut(|m| m.request_focus(goto_id()));
        }
        let field = egui::Panel::top("hexbar")
            .show(ui, |ui| {
                ui.add_space(3.0);
                let field = self.toolbar(ui);
                ui.add_space(1.0);
                field
            })
            .inner;
        // Whether the field has the keyboard is asked of egui's memory, not of the response:
        // `Response::has_focus` is also false whenever the platform says the window itself is
        // not focused (the owner in another application for a moment, a window that came up
        // behind another one). The grid would take the keyboard back in such a pass, and the
        // offset typed next would go over bytes.
        let goto_has = ctx.memory(|m| m.has_focus(goto_id()));
        let goto_owns = cmd_g || goto_has || field.lost_focus() || field.clicked();
        // The F-key row, the window's last (on its own: the browser around it has the row).
        super::own_keys(ui, HEX_KEYS);
        // The read-only window's path field (laid out below, with the rows) owns the typing of
        // a frame it had the keyboard in, as the Go to field does.
        let keep_had = ctx.memory(|m| m.focused()) == Some(keep_id());
        self.keys(&ctx, goto_owns || keep_had);
        // A report that lands while the button is down waits until it is let go. Its rows sit
        // below the grid: they take rows off its bottom, never move its top or its rows.
        if !ctx.input(|i| i.pointer.primary_down()) {
            if let Some(next) = self.pending_notices.take() {
                self.notices = next;
            }
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        let error = ui.visuals().error_fg_color;
        let warning = self.stream_warning();
        let lines: Vec<(String, Color32)> = warning
            .iter()
            .chain(&self.opened)
            .chain(&self.read_only)
            .chain(&self.notices)
            .map(|n| (n.clone(), error))
            .collect();
        let keep_row = self.read_only.is_some() && self.retained.is_none();
        if !lines.is_empty() || keep_row {
            egui::Panel::bottom("disclosures").show(ui, |ui| {
                ui.add_space(2.0);
                super::disclosures(ui, &lines);
                if keep_row {
                    self.keep_row(ui);
                }
                ui.add_space(2.0);
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                // A field that took the keyboard this frame (a click into the path field) keeps
                // it: the grid does not take it back.
                let field_owns = goto_owns || ctx.memory(|m| m.focused()) == Some(keep_id());
                self.grid(ui, field_owns)
            });
        self.poll_job(&ctx);
        self.close_prompt(&ctx);
    }

    /// Over a stream (06 §11): once the state's patches are in and the screen's bytes have
    /// arrived or failed — or, with `loading=1`, at the first frame with rows, those still
    /// loading shot as they are.
    fn ready(&self) -> bool {
        self.painted
            && self.job.is_none()
            && self.state_patches.is_empty()
            && self.reloading.is_none()
            && (self.shoot_loading || self.doc.pending_at().is_none())
    }

    fn keys(&self) -> &'static [chrome::FKey] {
        HEX_KEYS
    }

    /// A preview (02 §4.2) shows its first rows without bars or cursor and takes no input or
    /// focus; `false` (the hand-over) makes it the hex window again, at the same rows.
    fn set_preview(&mut self, on: bool) {
        self.preview = on;
    }

    /// Held without being drawn, a streamed window does not poll its file (06 §5).
    fn set_shown(&mut self, shown: bool) {
        if let Some(stream) = &self.stream {
            stream.set_visible(shown);
        }
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok || self.preview {
            return true;
        }
        // Read-only: nothing can be saved; closing asks only while the edits are kept nowhere.
        if self.read_only.is_some() && self.job.is_none() {
            if self.retained.is_some() || !self.doc.unsaved() {
                return true;
            }
            self.prompt = true;
            return false;
        }
        if self.job.is_some() && self.fetching.active.load(Ordering::SeqCst) {
            // The save is still fetching the whole file and has written nothing: the fetch is
            // stopped, not waited for, and its report decides (it asks about the edits).
            self.fetching.cancel.store(true, Ordering::SeqCst);
            self.close_when_done = true;
            return false;
        }
        if self.job.is_some() {
            // Never close mid-write: the window closes once the save has reported.
            self.close_when_done = true;
            self.status = Status::info("saving…");
            let mut next = self
                .pending_notices
                .take()
                .unwrap_or_else(|| self.notices.clone());
            if !next.iter().any(|n| n == CLOSES_AFTER_SAVE) {
                next.push(CLOSES_AFTER_SAVE.to_string());
            }
            self.pending_notices = Some(next);
            return false;
        }
        if !self.doc.unsaved() {
            return true;
        }
        self.prompt = true;
        false
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod preview_tests;
