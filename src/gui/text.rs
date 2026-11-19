//! The text viewer (spec §3a): a text file read-only, with syntax highlighting (syntect —
//! Sublime syntaxes on the pure-Rust regex engine), line numbers, and paging: the file is read a
//! page at a time through a sparse line index, never whole, so a file of any size opens alike.
//!
//! The index is one pass over the file when it opens (on the opener's worker, stopped by Esc
//! between chunks): a mark every
//! [`BLOCK_LINES`] lines, and at the first line start [`SPAN`] bytes past the last mark. A line
//! that does not start at a mark starts less than a span past one, so finding any line reads at
//! most a span; a longer line is followed by a mark, so its end is known without reading it.
//! Only [`LINE_MAX`] bytes of a line are shown.
//!
//! Highlighting carries state from the top of the file: the state at each mark is computed
//! once, in order, and a page is highlighted from the mark before it. It runs on a worker of
//! its own, with its own handle on the file — never on a frame: one syntect call cannot be
//! interrupted, and a pathological line can take minutes. The window shows a page plain and
//! swaps in its highlighted lines when the worker sends them; it asks for the page on screen
//! only (a newer request makes the worker drop the older one, checked between lines), and when
//! the window goes the worker is told to stop and is never waited for. At most [`MAX_WORKERS`]
//! run in the process, stuck ones included ([`Pool`]); a viewer that gets no slot stays plain. A line is parsed only whole: highlighting stops at the first
//! line longer than [`LINE_MAX`] (only its start is read, and a comment or string closed past the
//! cut would colour everything after it), and the rest of the file is plain. Files over
//! [`HIGHLIGHT_MAX`] are always shown plain.
//!
//! The file is read live; the index is of the file as opened. A file that differs from it in any
//! way ([`TextDoc::now`]: written to, grown, truncated, another file renamed over its path) is
//! never read through the old index: the viewer indexes the path again on a worker of its own —
//! one at a time, the next no sooner than max(1 s, 4 × the last index pass) after the previous
//! ended — keeps the page on screen meanwhile, and swaps the new index in (following the end if
//! it was at the end). A deleted file keeps its page. Checked before each new page: the window says so and
//! reads no more. Every offset taken from the index is checked, never trusted.

use super::{chrome, Embedded, Outcome, Status, Window};
use anyhow::{Context as _, Result};
use eframe::egui::{self, text::LayoutJob, Color32, Key, Modifiers, Pos2, Rect};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::sync::OnceLock;
use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// A mark in the line index every this many lines…
const BLOCK_LINES: u64 = 256;
/// …or at the first line start this far past the last mark, whichever comes first.
const SPAN: u64 = 64 * 1024;
/// Bytes of a line that are shown; the rest is cut (the bar says so).
pub const LINE_MAX: usize = 4096;
/// Files above this are shown without highlighting (its state runs from the top of the file).
pub const HIGHLIGHT_MAX: u64 = 16 << 20;
/// Reads while looking for the end of a line.
const CHUNK: usize = 16 * 1024;
/// Columns a tab advances to.
const TAB: usize = 4;
/// Columns Left / Right scroll by.
const SIDEWAYS: usize = 8;

/// Where line `line` starts: byte `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    line: u64,
    offset: u64,
}

/// One line as shown: its text (invalid UTF-8 replaced) and whether it was cut at [`LINE_MAX`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub cut: bool,
}

/// A text file opened for viewing: the handle it is read through, and the line index.
pub struct TextDoc {
    file: File,
    size: u64,
    marks: Vec<Mark>,
    lines: u64,
    /// The last line ends with a newline.
    ends_newline: bool,
    /// The first line longer than [`LINE_MAX`] bytes (0-based), if any.
    first_long: Option<u64>,
    /// The file as indexed: size, modification time and inode.
    opened: Identity,
    /// How long the index pass took (a re-read waits a multiple of it).
    indexed_in: std::time::Duration,
    /// The file as found right after the pass ([`TextDoc::now`] then).
    after_pass: Now,
    /// The path it was opened by (the local file, or the session's cached copy).
    path: std::path::PathBuf,
    /// Bytes read for lines (not the index pass) — what paging costs.
    page_reads: u64,
    /// Tests: the worker's copy ([`TextDoc::share`]) fails every read (not as a change).
    #[cfg(test)]
    pub(crate) fail_worker_reads: bool,
    #[cfg(test)]
    fail_reads: bool,
    /// Tests: the worker's copy reads `Stale` (the file changed under it).
    #[cfg(test)]
    pub(crate) stale_worker_reads: bool,
    #[cfg(test)]
    stale_reads: bool,
}

/// What [`TextDoc::changed`] compares: device, inode, size and modification time.
type Identity = (u64, u64, u64, Option<std::time::SystemTime>);

fn identity(meta: &std::fs::Metadata) -> Identity {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino(), meta.len(), meta.modified().ok())
}

impl TextDoc {
    /// Opens `path` and indexes its lines (one pass over the file, holding no text).
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_cancellable(path, &AtomicBool::new(false))
    }

    /// [`TextDoc::open`] that stops when `cancel` is set (Esc while a big file is indexed):
    /// checked before the pass and after every chunk of it; the error is
    /// [`Cancelled`](crate::remote::Cancelled).
    pub fn open_cancellable(path: &Path, cancel: &AtomicBool) -> Result<Self> {
        Self::open_with(path, cancel, &mut |_| {})
    }

    /// [`TextDoc::open_cancellable`], with `on_chunk(bytes indexed so far)` after every chunk
    /// (tests: a cancel, or a writer, in the middle of the pass).
    fn open_with(path: &Path, cancel: &AtomicBool, on_chunk: &mut dyn FnMut(u64)) -> Result<Self> {
        let stopped = || {
            if cancel.load(Ordering::Relaxed) {
                Err(anyhow::Error::new(crate::remote::Cancelled))
            } else {
                Ok(())
            }
        };
        stopped()?;
        let started = std::time::Instant::now();
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {}", path.display()))?;
        let (size, opened) = (meta.len(), identity(&meta));
        let mut marks = vec![Mark { line: 0, offset: 0 }];
        let (mut line, mut at, mut line_len) = (0u64, 0u64, 0u64);
        let (mut first_long, mut last_byte) = (None, None);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file
                .read_at(&mut buf, at)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if n == 0 {
                break;
            }
            // Only the bytes as opened: a file growing meanwhile is noticed later.
            let n = n.min(size.saturating_sub(at) as usize);
            if n == 0 {
                break;
            }
            for (i, &b) in buf[..n].iter().enumerate() {
                if b != b'\n' {
                    line_len += 1;
                    continue;
                }
                if line_len > LINE_MAX as u64 && first_long.is_none() {
                    first_long = Some(line);
                }
                line_len = 0;
                line += 1;
                let start = at + i as u64 + 1;
                let last = marks[marks.len() - 1];
                if start < size && (line - last.line >= BLOCK_LINES || start - last.offset >= SPAN)
                {
                    marks.push(Mark {
                        line,
                        offset: start,
                    });
                }
            }
            last_byte = Some(buf[n - 1]);
            at += n as u64;
            on_chunk(at);
            stopped()?;
        }
        // The bytes actually indexed (fewer than the size as opened if it shrank meanwhile: the
        // change is noticed later), and no mark at or past their end.
        let size = at;
        marks.retain(|m| m.line == 0 || m.offset < size);
        if line_len > LINE_MAX as u64 && first_long.is_none() {
            first_long = Some(line);
        }
        let ends_newline = last_byte == Some(b'\n');
        if last_byte.is_some() && !ends_newline {
            line += 1;
        }
        let mut doc = Self {
            file,
            size,
            marks,
            lines: line,
            ends_newline,
            first_long,
            opened,
            indexed_in: started.elapsed(),
            after_pass: Now::Same,
            path: path.to_path_buf(),
            page_reads: 0,
            #[cfg(test)]
            fail_worker_reads: false,
            #[cfg(test)]
            fail_reads: false,
            #[cfg(test)]
            stale_worker_reads: false,
            #[cfg(test)]
            stale_reads: false,
        };
        // Checked here, right after the pass (on the opener's or re-read's worker), as well as
        // when the viewer takes the index: a change between the two is caught by one of them.
        doc.after_pass = doc.now();
        Ok(doc)
    }

    /// What the file is now, against the index: [`Now::Same`] only when the open handle and the
    /// path it was opened by (followed through links, as opened) both still show the indexed
    /// device, inode, size and modification time; [`Now::Gone`] when nothing is at the path
    /// (deleted); [`Now::Grown`] when both still show the indexed device and inode, the scan read
    /// every byte the file had as opened, and both are no shorter than that (appended to, or
    /// written in place: the size cannot tell); anything else — truncated, another file renamed
    /// over the path, a shrink met while scanning — [`Now::Changed`]. The viewer reads a grown
    /// file through its index (bounded reads, [`Stale`] the net) and indexes it again soon; a
    /// changed one is never read through it.
    pub fn now(&self) -> Now {
        let named = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Now::Gone,
            Err(_) => return Now::Changed,
        };
        let Ok(handle) = self.file.metadata() else {
            return Now::Changed;
        };
        let same = |m: &std::fs::Metadata| identity(m) == self.opened;
        if same(&handle) && same(&named) {
            return Now::Same;
        }
        let (dev, ino, len, _) = self.opened;
        let grown = |m: &std::fs::Metadata| {
            let (d, i, l, _) = identity(m);
            d == dev && i == ino && l >= len
        };
        if self.size == len && grown(&handle) && grown(&named) {
            Now::Grown
        } else {
            Now::Changed
        }
    }

    /// [`TextDoc::now`], counting what the check right after the pass found too: a change seen
    /// there (a shrink, another file) is not undone by the file looking grown later.
    pub fn now_since_pass(&self) -> Now {
        match (self.after_pass, self.now()) {
            (_, Now::Gone) => Now::Gone,
            (Now::Changed | Now::Gone, _) => Now::Changed,
            (_, now) => now,
        }
    }

    /// The file is not as indexed any more ([`TextDoc::now`] is not [`Now::Same`]).
    pub fn changed(&self) -> bool {
        self.now() != Now::Same
    }

    /// The path the file was opened by.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The same file and index on a handle of its own (the highlighting worker's).
    fn share(&self) -> Result<TextDoc> {
        Ok(TextDoc {
            file: self
                .file
                .try_clone()
                .context("cannot open the file again")?,
            size: self.size,
            marks: self.marks.clone(),
            lines: self.lines,
            ends_newline: self.ends_newline,
            first_long: self.first_long,
            opened: self.opened,
            indexed_in: self.indexed_in,
            after_pass: self.after_pass,
            path: self.path.clone(),
            page_reads: 0,
            #[cfg(test)]
            fail_worker_reads: false,
            #[cfg(test)]
            fail_reads: self.fail_worker_reads,
            #[cfg(test)]
            stale_worker_reads: false,
            #[cfg(test)]
            stale_reads: self.stale_worker_reads,
        })
    }

    pub fn line_count(&self) -> u64 {
        self.lines
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Some line is cut at [`LINE_MAX`] bytes.
    pub fn long_lines(&self) -> bool {
        self.first_long.is_some()
    }

    /// Bytes read for lines so far (the index pass not counted).
    pub fn page_reads(&self) -> u64 {
        self.page_reads
    }

    /// Marks in the line index.
    pub fn marks(&self) -> usize {
        self.marks.len()
    }

    /// The mark at or before `line`.
    fn mark_before(&self, line: u64) -> usize {
        self.marks
            .partition_point(|m| m.line <= line)
            .saturating_sub(1)
    }

    /// The mark after `idx`, or the end of the file.
    fn next_mark(&self, idx: usize) -> Mark {
        self.marks.get(idx + 1).copied().unwrap_or(Mark {
            line: self.lines,
            offset: self.size,
        })
    }

    fn read_at(&mut self, buf: &mut [u8], at: u64) -> Result<usize> {
        #[cfg(test)]
        if self.fail_reads {
            anyhow::bail!("an injected read error");
        }
        #[cfg(test)]
        if self.stale_reads {
            return Err(anyhow::Error::new(Stale));
        }
        let n = self.file.read_at(buf, at).context("cannot read the file")?;
        self.page_reads += n as u64;
        Ok(n)
    }

    /// Lines `first ..first + count` (fewer at the end of the file), each cut at [`LINE_MAX`]
    /// bytes. Reads from the mark before `first`: at most a span to reach it, then the shown
    /// part of each line (a line that ends at a mark is not read past what is shown), in
    /// [`CHUNK`]-sized reads.
    pub fn read_lines(&mut self, first: u64, count: usize) -> Result<Vec<Line>> {
        let mut out = Vec::new();
        let last = first.saturating_add(count as u64).min(self.lines);
        if first >= last {
            return Ok(out);
        }
        let mut idx = self.mark_before(first);
        let (mut line, mut pos) = (self.marks[idx].line, self.marks[idx].offset);
        let mut window = Window_::default();
        while line < last {
            let next = self.next_mark(idx);
            let keep = line >= first;
            if line + 1 == next.line {
                // It ends where the next mark starts: read only what is shown of it.
                let newline = u64::from(next.line < self.lines || self.ends_newline);
                // Checked: a file changed since it was indexed can put `pos` past the mark.
                let len = next
                    .offset
                    .checked_sub(pos)
                    .and_then(|n| n.checked_sub(newline))
                    .unwrap_or(0);
                if keep {
                    let want = len.min(LINE_MAX as u64) as usize;
                    let bytes = window.take(self, pos, want)?;
                    out.push(shown(bytes, len > LINE_MAX as u64));
                }
                pos = next.offset;
                idx += 1;
            } else {
                // More lines start before the next mark, and none of them started a mark: the
                // next one starts within a span of this block's mark (or a mark would be there)
                // and before the next mark. So this line ends before both, and is never read past
                // them — a file changed meanwhile could make it a line of any length.
                let end = next.offset.min(self.marks[idx].offset.saturating_add(SPAN));
                let mut text = Vec::new();
                let mut cut = false;
                loop {
                    let room_to_mark = end.saturating_sub(pos);
                    if room_to_mark == 0 {
                        return Err(anyhow::Error::new(Stale));
                    }
                    let want = room_to_mark.min(CHUNK as u64) as usize;
                    let avail = window.at(self, pos, want)?;
                    let avail = &avail[..avail.len().min(room_to_mark as usize)];
                    if avail.is_empty() {
                        return Err(anyhow::Error::new(Stale));
                    }
                    let (body, end) = match avail.iter().position(|&b| b == b'\n') {
                        Some(i) => (&avail[..i], Some(i)),
                        None => (avail, None),
                    };
                    if keep {
                        let room = LINE_MAX - text.len().min(LINE_MAX);
                        cut |= body.len() > room;
                        text.extend_from_slice(&body[..body.len().min(room)]);
                    }
                    match end {
                        Some(i) => {
                            pos += i as u64 + 1;
                            break;
                        }
                        None => pos += avail.len() as u64,
                    }
                }
                if keep {
                    out.push(shown(text, cut));
                }
            }
            line += 1;
        }
        Ok(out)
    }
}

/// What the file is now, against the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Now {
    Same,
    /// Written to since, but only grown: the same file (device and inode, at the handle and the
    /// path), every byte indexed still there as far as the size can tell, and no shorter than
    /// them (a log being written to). The index still describes the bytes as it scanned them.
    Grown,
    Changed,
    /// Nothing is at the path any more.
    Gone,
}

/// The file is not what was indexed: a line that should end before the next mark does not.
#[derive(Debug)]
pub struct Stale;

impl std::fmt::Display for Stale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the file changed since it was opened")
    }
}

impl std::error::Error for Stale {}

/// Whether `e` says the file changed under the index ([`Stale`]).
pub fn is_stale(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Stale>())
}

/// The last [`CHUNK`] read for [`TextDoc::read_lines`]: short lines next to each other cost one
/// read.
#[derive(Default)]
struct Window_ {
    start: u64,
    bytes: Vec<u8>,
}

impl Window_ {
    /// The bytes from `pos` on, as far as one read of up to `want` bytes reaches (empty at the
    /// end of the file).
    fn at(&mut self, doc: &mut TextDoc, pos: u64, want: usize) -> Result<&[u8]> {
        let end = self.start + self.bytes.len() as u64;
        if pos < self.start || pos >= end {
            self.bytes.resize(want.clamp(1, CHUNK), 0);
            let n = doc.read_at(&mut self.bytes, pos)?;
            self.bytes.truncate(n);
            self.start = pos;
        }
        Ok(&self.bytes[(pos - self.start) as usize..])
    }

    /// `len` bytes from `pos` (fewer at the end of the file).
    fn take(&mut self, doc: &mut TextDoc, mut pos: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let avail = self.at(doc, pos, len - out.len())?;
            if avail.is_empty() {
                break;
            }
            let n = avail.len().min(len - out.len());
            out.extend_from_slice(&avail[..n]);
            pos += n as u64;
        }
        Ok(out)
    }
}

/// Where a character cut in two at the end of `bytes` starts (`bytes.len()` when none is),
/// found from the end alone, whatever comes before: the last up-to-4 bytes from their last
/// non-continuation byte on must be the start of a real character (`from_utf8` checks the
/// lead byte, overlongs, surrogates and the U+10FFFF bound) that only lacks its end. Bytes that
/// can never start a character are not a cut character: they show as U+FFFD.
fn cut_char_start(bytes: &[u8]) -> usize {
    let from = bytes.len().saturating_sub(4);
    let Some(i) = (from..bytes.len()).rev().find(|&i| bytes[i] & 0xC0 != 0x80) else {
        return bytes.len();
    };
    match std::str::from_utf8(&bytes[i..]) {
        Err(e) if e.valid_up_to() == 0 && e.error_len().is_none() => i,
        _ => bytes.len(),
    }
}

/// A line's shown bytes as text: a trailing `\r` dropped; when `cut`, a character cut in two
/// at the end left out; anything else that is not UTF-8 shown as U+FFFD.
fn shown(mut bytes: Vec<u8>, cut: bool) -> Line {
    if cut {
        let keep = cut_char_start(&bytes);
        bytes.truncate(keep);
    } else if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Line {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        cut,
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

/// Loads the bundled syntaxes and builds the skin's theme (tens of milliseconds, once): the
/// opener calls it on its worker, so the window does not wait for it.
pub fn warm() {
    syntaxes();
    chrome::syntax_theme();
}

/// The syntax for a file named `name`: by extension, by the whole name (`Makefile`), and only
/// when the name does not decide, by the first line (`#!/bin/sh`) read through `doc` — an index
/// that may be read (`None`: the name alone). `None` for plain text.
fn syntax_of(name: &str, doc: Option<&mut TextDoc>) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let by_name = crate::kind::extension(name)
        .and_then(|ext| set.find_syntax_by_extension(&ext))
        .or_else(|| set.find_syntax_by_extension(name));
    let found = match (by_name, doc) {
        (Some(found), _) => found,
        (None, Some(doc)) => {
            let first = doc
                .read_lines(0, 1)
                .ok()
                .and_then(|l| l.into_iter().next())
                .map(|l| l.text)
                .unwrap_or_default();
            set.find_syntax_by_first_line(&first)?
        }
        (None, None) => return None,
    };
    (found.name != "Plain Text").then_some(found)
}

/// A highlighted line: runs of text and their colour.
type Runs = Vec<(Color32, String)>;

/// Parser and highlighter state after some line.
type State = (ParseState, HighlightState);

/// Called with each line before it is parsed (tests: a parser that blocks on a line).
#[cfg(test)]
pub(crate) type ParseHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Highlighting state at each mark, computed in order from the top of the file — the worker's.
struct Highlight {
    highlighter: Highlighter<'static>,
    /// The state at mark `i`'s line, for every `i < states.len()`.
    states: Vec<State>,
    /// Lines from here on are plain (the first line cut for display, see the module doc).
    stop: u64,
    plain: Color32,
    /// Lines parsed so far (tests).
    parsed: Arc<AtomicU64>,
    #[cfg(test)]
    hook: Option<ParseHook>,
    /// Called right after the worker's thread asked the window to wake as it ends (tests: the
    /// end must already be visible to the window by then).
    #[cfg(test)]
    after_wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// Lines read at a time while highlighting.
const BATCH: usize = 64;

impl Highlight {
    fn new(syntax: &'static SyntaxReference, plain: Color32, stop: u64) -> Option<Self> {
        let highlighter = Highlighter::new(chrome::syntax_theme());
        let start = (
            ParseState::new(syntax),
            HighlightState::new(&highlighter, ScopeStack::new()),
        );
        Some(Self {
            highlighter,
            states: vec![start],
            stop,
            plain,
            parsed: Arc::default(),
            #[cfg(test)]
            hook: None,
            #[cfg(test)]
            after_wake: None,
        })
    }

    fn run(&self, state: &mut State, line: &str) -> Runs {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(line);
        }
        self.parsed.fetch_add(1, Ordering::Relaxed);
        let Ok(ops) = state.0.parse_line(line, syntaxes()) else {
            return vec![(self.plain, line.to_string())];
        };
        HighlightIterator::new(&mut state.1, &ops, line, &self.highlighter)
            .map(|(style, text)| {
                let c = style.foreground;
                (Color32::from_rgb(c.r, c.g, c.b), text.to_string())
            })
            .collect()
    }

    /// Parses lines `next..to` from `state`, keeping each line's runs in `keep` when given.
    /// `Ok(false)`: `dropped()` said to stop (checked before every line).
    fn walk(
        &self,
        doc: &mut TextDoc,
        mut next: u64,
        to: u64,
        state: &mut State,
        dropped: &dyn Fn() -> bool,
        mut keep: Option<&mut Vec<Runs>>,
    ) -> Result<bool> {
        while next < to {
            let want = (to - next).min(BATCH as u64) as usize;
            let lines = doc.read_lines(next, want)?;
            if lines.is_empty() {
                return Err(anyhow::Error::new(Stale));
            }
            for line in &lines {
                if dropped() {
                    return Ok(false);
                }
                let runs = self.run(state, &line.text);
                if let Some(keep) = keep.as_deref_mut() {
                    keep.push(runs);
                }
                next += 1;
            }
        }
        Ok(true)
    }

    /// The page of `count` lines at `top`, highlighted up to the stop line (plain after it).
    /// `Ok(None)`: dropped for a newer request (or the window went).
    fn page(
        &mut self,
        doc: &mut TextDoc,
        top: u64,
        count: usize,
        dropped: &dyn Fn() -> bool,
    ) -> Result<Option<Vec<Runs>>> {
        let lines = doc.read_lines(top, count)?;
        let end = (top + lines.len() as u64).min(self.stop);
        let mut runs = Vec::new();
        if top < end {
            // The states up to the mark before the page, a whole block at a time.
            let k = doc.mark_before(top);
            while self.states.len() <= k {
                let i = self.states.len() - 1;
                let mut state = self.states[i].clone();
                let (from, to) = (doc.marks[i].line, doc.next_mark(i).line);
                if !self.walk(doc, from, to, &mut state, dropped, None)? {
                    return Ok(None);
                }
                self.states.push(state);
            }
            // From that mark to the page, then the page's own lines.
            let mut state = self.states[k].clone();
            let from = doc.marks[k].line;
            if !self.walk(doc, from, top, &mut state, dropped, None)?
                || !self.walk(doc, top, end, &mut state, dropped, Some(&mut runs))?
            {
                return Ok(None);
            }
        }
        runs.truncate(lines.len());
        let plain = self.plain;
        runs.extend(
            lines[runs.len()..]
                .iter()
                .map(|l| vec![(plain, l.text.clone())]),
        );
        Ok(Some(runs))
    }
}

/// Highlighting workers that may run at once, in the whole process: a worker stuck in one
/// parse call outlives its viewer (it cannot be interrupted), and opening the same file again
/// must not pile up parsers, handles and indexes.
pub const MAX_WORKERS: usize = 3;

/// Admission for highlighting workers: a slot is taken when a worker starts and given back only
/// when its thread ends ([`Slot`], owned by the thread) — so it counts live workers and those
/// whose viewer is gone but whose parse still runs.
pub(crate) struct Pool {
    cap: usize,
    live: AtomicUsize,
}

impl Pool {
    pub(crate) fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            cap,
            live: AtomicUsize::new(0),
        })
    }

    /// The process's pool ([`MAX_WORKERS`]; tests, which run many viewers side by side, get a
    /// roomier one — the bound itself is tested on pools of their own).
    pub(crate) fn global() -> Arc<Self> {
        static POOL: OnceLock<Arc<Pool>> = OnceLock::new();
        let cap = if cfg!(test) { 64 } else { MAX_WORKERS };
        POOL.get_or_init(|| Pool::new(cap)).clone()
    }

    /// Workers holding a slot now (tests).
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }

    /// A slot, if one is free.
    fn admit(self: &Arc<Self>) -> Option<Slot> {
        let mut n = self.live.load(Ordering::Acquire);
        loop {
            if n >= self.cap {
                return None;
            }
            match self
                .live
                .compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Some(Slot(self.clone())),
                Err(now) => n = now,
            }
        }
    }
}

/// One worker's place in its [`Pool`], given back when dropped (when the worker's thread ends,
/// however it ends).
struct Slot(Arc<Pool>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A page the window wants highlighted: asked under generation `gen`.
struct Ask {
    gen: u64,
    top: u64,
    count: usize,
}

/// What the worker sends back.
enum Done {
    Page {
        gen: u64,
        runs: Vec<Runs>,
    },
    /// The file changed under the index: the worker stopped.
    Stale,
    /// The worker stopped for another reason (a read error): why.
    Failed(String),
}

/// The highlighting worker, from the window's side. Dropping it tells the worker to stop; it is
/// never joined (a syntect call in progress finishes on its own, its result unused).
struct Worker {
    asks: Sender<Ask>,
    done: Receiver<Done>,
    /// The newest generation asked: the worker drops older work (checked between lines).
    latest: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    /// Lines the worker parsed (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    parsed: Arc<AtomicU64>,
    /// The worker's thread ended (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    exited: Arc<AtomicBool>,
}

/// The worker thread's end, made visible before it is announced: dropped as the thread ends
/// (however it ends), it drops the sender first — the window's channel closes, so the end is
/// receivable — and only then wakes the window.
struct End {
    tx: Option<Sender<Done>>,
    ctx: egui::Context,
    #[cfg(test)]
    after_wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl End {
    fn send(&self, done: Done) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(done);
        }
    }
}

impl Drop for End {
    fn drop(&mut self) {
        drop(self.tx.take());
        self.ctx.request_repaint();
        #[cfg(test)]
        if let Some(after) = &self.after_wake {
            after();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Worker {
    /// Starts the worker on `doc` (a handle of its own) with `hl`, if `pool` has a slot free;
    /// `ctx` is woken when a page is done or the worker stops.
    fn start(
        mut doc: TextDoc,
        mut hl: Highlight,
        ctx: egui::Context,
        pool: &Arc<Pool>,
    ) -> Option<Self> {
        let slot = pool.admit()?;
        let (asks, rx) = mpsc::channel::<Ask>();
        let (tx, done) = mpsc::channel();
        let latest = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let parsed = hl.parsed.clone();
        let (l, st, ex) = (latest.clone(), stop.clone(), exited.clone());
        std::thread::spawn(move || {
            // Given back when this thread ends — normally, or by a panic in the parser.
            let _slot = slot;
            let end = End {
                tx: Some(tx),
                ctx: ctx.clone(),
                #[cfg(test)]
                after_wake: hl.after_wake.clone(),
            };
            let work = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                while let Ok(mut ask) = rx.recv() {
                    // Only the newest request matters.
                    while let Ok(newer) = rx.try_recv() {
                        ask = newer;
                    }
                    if st.load(Ordering::Relaxed) {
                        break;
                    }
                    let gen = ask.gen;
                    let dropped = || st.load(Ordering::Relaxed) || l.load(Ordering::Relaxed) != gen;
                    match hl.page(&mut doc, ask.top, ask.count, &dropped) {
                        Ok(Some(runs)) => {
                            end.send(Done::Page { gen, runs });
                            ctx.request_repaint();
                        }
                        Ok(None) => {}
                        Err(e) => {
                            crate::debug!("highlighting stopped: {e:#}");
                            end.send(if is_stale(&e) {
                                Done::Stale
                            } else {
                                Done::Failed(format!("{e:#}"))
                            });
                            break;
                        }
                    }
                }
            }));
            if work.is_err() {
                end.send(Done::Failed("the highlighter panicked".to_string()));
            }
            ex.store(true, Ordering::Relaxed);
            // The channel closes, then the window is woken.
            drop(end);
        });
        Some(Self {
            asks,
            done,
            latest,
            stop,
            parsed,
            exited,
        })
    }

    fn ask(&self, gen: u64, top: u64, count: usize) {
        self.latest.store(gen, Ordering::Relaxed);
        let _ = self.asks.send(Ask { gen, top, count });
    }
}

/// `runs` with tabs turned into spaces up to the next tab stop.
fn expand_tabs(runs: Runs) -> Runs {
    let mut col = 0;
    runs.into_iter()
        .map(|(color, text)| {
            let mut out = String::with_capacity(text.len());
            for c in text.chars() {
                if c == '\t' {
                    let n = TAB - col % TAB;
                    out.extend(std::iter::repeat_n(' ', n));
                    col += n;
                } else {
                    out.push(c);
                    col += 1;
                }
            }
            (color, out)
        })
        .collect()
}

/// The page on screen: its first line, how many, and how its lines are drawn.
struct Page {
    top: u64,
    count: usize,
    runs: Vec<Runs>,
    highlighted: bool,
    /// The generation its highlighting was asked under.
    gen: u64,
}

/// The window's title in its frame, as PC Shell names its text viewer.
const TITLE: &str = "Text Viewer";

struct TextWindow {
    /// `host:/abs`.
    title: String,
    /// The file's name (it picks the syntax, with the first line when it does not decide).
    name: String,
    doc: TextDoc,
    syntax: Option<&'static SyntaxReference>,
    /// Started on the first pass (it needs the theme: dark or light); `None` without a syntax,
    /// for a file over [`HIGHLIGHT_MAX`], or when the file cannot be opened again.
    worker: Option<Worker>,
    hl_tried: bool,
    /// The last generation asked of the worker.
    gen: u64,
    #[cfg(test)]
    hook: Option<ParseHook>,
    #[cfg(test)]
    after_wake: Option<Arc<dyn Fn() + Send + Sync>>,
    pool: Arc<Pool>,
    /// The first line on screen, and how many fit (the last pass's).
    top: u64,
    visible: u64,
    /// Columns scrolled to the right.
    left: usize,
    wheel_rest: f32,
    page: Option<Page>,
    /// The page on screen is from an index replaced since: it stays up until a page is read
    /// through the new one.
    stale_page: bool,
    /// Lines from open that must be read whole (where earlier copies' unsaved edits went).
    notices: Vec<String>,
    status: Status,
    painted: bool,
    /// The file differs from its index: nothing is read through it until a re-read lands.
    waiting: bool,
    /// The file was deleted: the page stays, nothing more is read.
    gone: bool,
    /// The re-read under way, if one is.
    reindex: Option<Reindex>,
    /// The time (tests move it), when the last index pass ended and how long it took.
    clock: Box<dyn Fn() -> std::time::Instant>,
    index_done: std::time::Instant,
    index_took: std::time::Duration,
    /// Re-reads started so far.
    reindexes: usize,
    /// A local file (followed at its end); a remote one's cached copy is not.
    live: bool,
    /// Called as a re-read's worker starts (tests: hold it).
    #[cfg(test)]
    reindex_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Called after every chunk of a re-read's pass (tests: change the file during it).
    #[cfg(test)]
    reindex_chunk_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Called right after a re-read's index is installed (tests: write to the file there).
    #[cfg(test)]
    after_install: Option<Box<dyn FnMut()>>,
}

/// A re-read of the file from its path, on a worker. Dropping it (the viewer gone) cancels it;
/// it is never waited for.
struct Reindex {
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<TextDoc>>,
    /// The viewer was at the end when the change was seen (it follows the new end).
    at_end: bool,
    /// The worker's thread ended (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    exited: Arc<AtomicBool>,
}

impl Drop for Reindex {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The shortest wait between the end of one index pass and the start of the next.
const REINDEX_GAP: std::time::Duration = std::time::Duration::from_secs(1);

/// How often a local file viewed at its end is looked at again.
const FOLLOW_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

impl TextWindow {
    fn new(mut doc: TextDoc, title: String, name: &str, notices: Vec<String>) -> Self {
        // Changed since it was indexed (between the opener's worker and now): nothing is read
        // through that index; the first pass re-reads it.
        let now = doc.now_since_pass();
        // The first line refines the choice only through an index that may be read.
        let readable = matches!(now, Now::Same | Now::Grown);
        let syntax = syntax_of(name, readable.then_some(&mut doc));
        let index_took = doc.indexed_in;
        let status = match (syntax, doc.first_long) {
            _ if now == Now::Gone => Status::error("the file was deleted"),
            (None, _) => Status::default(),
            _ if doc.size() > HIGHLIGHT_MAX => Status::info(format!(
                "over {}: shown without highlighting",
                crate::human(HIGHLIGHT_MAX)
            )),
            (Some(_), Some(line)) => Status::info(format!(
                "highlighting stops at line {}: it is over {LINE_MAX} bytes",
                line + 1
            )),
            (Some(_), None) => Status::default(),
        };
        Self {
            title,
            name: name.to_string(),
            doc,
            syntax,
            worker: None,
            hl_tried: false,
            gen: 0,
            #[cfg(test)]
            hook: None,
            #[cfg(test)]
            after_wake: None,
            pool: Pool::global(),
            waiting: now == Now::Changed,
            gone: now == Now::Gone,
            reindex: None,
            clock: Box::new(std::time::Instant::now),
            index_done: std::time::Instant::now(),
            index_took,
            reindexes: 0,
            live: true,
            #[cfg(test)]
            reindex_hook: None,
            #[cfg(test)]
            reindex_chunk_hook: None,
            #[cfg(test)]
            after_install: None,
            top: 0,
            visible: 1,
            left: 0,
            wheel_rest: 0.0,
            page: None,
            stale_page: false,
            notices,
            status,
            painted: false,
        }
    }

    /// Nothing is read now: the file differs from its index (a re-read is due or under way), or
    /// it was deleted.
    fn frozen(&self) -> bool {
        self.waiting || self.reindex.is_some() || self.gone
    }

    /// When the next index pass may start: max(1 s, 4 × the last pass) after it ended.
    fn next_pass_at(&self) -> std::time::Instant {
        self.index_done + REINDEX_GAP.max(self.index_took * 4)
    }

    /// The file differs from its index: a re-read starts now if the last pass is long enough
    /// ago, else a repaint is asked for when it will be. Only a viewer's very first attempt with
    /// nothing ever shown starts at once; every later one waits (a page on screen or not).
    fn start_reindex(&mut self, ctx: &egui::Context) {
        if self.reindex.is_some() {
            return;
        }
        let now = (self.clock)();
        let due = self.next_pass_at();
        let first = self.reindexes == 0 && self.page.is_none();
        if !first && now < due {
            self.status = Status::info("the file changed; re-reading it in a moment…");
            ctx.request_repaint_after(due - now);
            return;
        }
        let at_end = self.top.saturating_add(self.visible) >= self.doc.line_count();
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let (stop, ex, path, waker) = (
            cancel.clone(),
            exited.clone(),
            self.doc.path().to_path_buf(),
            ctx.clone(),
        );
        #[cfg(test)]
        let (hook, chunk_hook) = (self.reindex_hook.clone(), self.reindex_chunk_hook.clone());
        std::thread::spawn(move || {
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            let mut on_chunk = |_: u64| {
                #[cfg(test)]
                if let Some(hook) = &chunk_hook {
                    hook();
                }
            };
            let result = TextDoc::open_with(&path, &stop, &mut on_chunk);
            let _ = tx.send(result);
            ex.store(true, Ordering::Relaxed);
            waker.request_repaint();
        });
        self.reindexes += 1;
        // The old index is not read any more: its highlighting goes too.
        self.worker = None;
        self.status = Status::info("re-reading the file…");
        self.reindex = Some(Reindex {
            cancel,
            rx,
            at_end,
            exited,
        });
    }

    /// A re-read that landed: if the file is still as it read it, or only grown since (as the
    /// worker found it right after the pass, and as it is now), its index replaces the old one,
    /// with a fresh generation and a new highlighter (next pass); the same top line (clamped), or
    /// the new end if the viewer was at the end. A file grown meanwhile is re-read again, no
    /// sooner than the usual wait. Any other change meanwhile: the page on screen stays and the
    /// next pass waits the usual time.
    fn poll_reindex(&mut self) {
        let Some(reindex) = &self.reindex else {
            return;
        };
        let result = match reindex.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow::anyhow!("the re-read stopped")),
        };
        let at_end = reindex.at_end;
        self.reindex = None;
        self.index_done = (self.clock)();
        let result = result.map(|doc| {
            let now = doc.now_since_pass();
            (doc, now)
        });
        match result {
            Ok((_, Now::Gone)) => {
                self.waiting = false;
                self.gone = true;
                self.status = Status::error("the file was deleted");
            }
            Ok((doc, Now::Changed)) => {
                // Overtaken by another change: not readable either, so the old page stays — and
                // the old index is not read through either (the file may look grown to it).
                self.index_took = doc.indexed_in;
                self.waiting = true;
                self.status = Status::info("the file changed again; re-reading it in a moment…");
            }
            Ok((doc, _)) => {
                let grew = doc.size() > self.doc.size();
                self.index_took = doc.indexed_in;
                self.doc = doc;
                // The first line may have changed (another shebang): the grammar follows it.
                self.syntax = syntax_of(&self.name, Some(&mut self.doc));
                self.waiting = false;
                // The old page stays up until one is read through the new index (a write
                // landing first leaves it up).
                self.stale_page = true;
                self.worker = None;
                self.hl_tried = false;
                let last_top = self.doc.line_count().saturating_sub(self.visible);
                self.top = if at_end {
                    last_top
                } else {
                    self.top.min(last_top)
                };
                self.status = Status::info(if grew && at_end {
                    "the file grew; following the end"
                } else {
                    "the file changed; re-read"
                });
                #[cfg(test)]
                if let Some(hook) = self.after_install.as_mut() {
                    hook();
                }
            }
            Err(_) if self.doc.now() == Now::Gone => {
                self.waiting = false;
                self.gone = true;
                self.status = Status::error("the file was deleted");
            }
            Err(e) => {
                // Tried again after the usual wait.
                self.index_took = std::time::Duration::ZERO;
                self.status = Status::error(format!("cannot read the file again: {e:#}"));
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if self.waiting || self.reindex.is_some() {
            // Scrolling waits for the new index (the page on screen stays meanwhile).
            return;
        }
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        let page = self.visible.saturating_sub(1).max(1);
        if pressed(Key::ArrowDown) {
            self.top = self.top.saturating_add(1);
        }
        if pressed(Key::ArrowUp) {
            self.top = self.top.saturating_sub(1);
        }
        if pressed(Key::PageDown) {
            self.top = self.top.saturating_add(page);
        }
        if pressed(Key::PageUp) {
            self.top = self.top.saturating_sub(page);
        }
        if pressed(Key::Home) {
            self.top = 0;
        }
        if pressed(Key::End) {
            // Past the end: the body puts the last page on screen with this pass's row count.
            self.top = self.doc.line_count();
        }
        if pressed(Key::ArrowRight) {
            self.left += SIDEWAYS;
        }
        if pressed(Key::ArrowLeft) {
            self.left = self.left.saturating_sub(SIDEWAYS);
        }
    }

    /// The page at `top`, read once and highlighted as soon as the states reach it.
    fn ensure_page(&mut self, ctx: &egui::Context, plain: Color32) {
        let (top, count) = (self.top, self.visible as usize);
        let same = !self.stale_page
            && self
                .page
                .as_ref()
                .is_some_and(|p| p.top == top && p.count == count);
        let mut due = false;
        if !self.frozen() {
            match self.doc.now_since_pass() {
                Now::Same => {}
                // Only grown (a log being written): the index still describes the bytes it
                // scanned, so the page is read through it; a re-read is due (after the read).
                Now::Grown => due = true,
                Now::Changed => self.waiting = true,
                Now::Gone => {
                    self.gone = true;
                    self.worker = None;
                    self.status = Status::error("the file was deleted");
                }
            }
        }
        if self.waiting && self.reindex.is_none() {
            self.start_reindex(ctx);
        }
        if self.frozen() {
            // The page on screen stays (from memory); nothing is read through the old index. A
            // file deleted before anything was shown shows an empty page.
            if self.gone && self.page.is_none() {
                self.page = Some(Page {
                    top,
                    count,
                    runs: Vec::new(),
                    highlighted: true,
                    gen: 0,
                });
            }
            if let Some(page) = self.page.as_mut() {
                page.highlighted = true;
            }
            return;
        }
        if !same {
            let lines = match self.doc.read_lines(top, count) {
                Ok(lines) => lines,
                Err(e) if is_stale(&e) => {
                    // Changed after the check, during the read: read again from the path.
                    self.waiting = true;
                    return self.ensure_page(ctx, plain);
                }
                Err(e) => {
                    self.status = Status::error(format!("{e:#}"));
                    Vec::new()
                }
            };
            self.stale_page = false;
            let runs = lines
                .iter()
                .map(|l| expand_tabs(vec![(plain, l.text.clone())]))
                .collect();
            self.gen += 1;
            self.page = Some(Page {
                top,
                count,
                runs,
                highlighted: self.worker.is_none(),
                gen: self.gen,
            });
            if let Some(worker) = &self.worker {
                worker.ask(self.gen, top, count);
            }
        }
        self.worker_results();
        if (self.waiting || due) && self.reindex.is_none() {
            // A change (also one the highlighter found): the re-read starts, or is scheduled,
            // in this frame, not at the next input.
            self.start_reindex(ctx);
        }
    }

    /// What the worker finished meanwhile: the page on screen's lines replace its plain ones.
    fn worker_results(&mut self) {
        let (Some(worker), Some(page)) = (self.worker.as_ref(), self.page.as_mut()) else {
            return;
        };
        let mut gone = None;
        loop {
            match worker.done.try_recv() {
                Ok(Done::Page { gen, runs }) if gen == page.gen => {
                    page.runs = runs.into_iter().map(expand_tabs).collect();
                    page.highlighted = true;
                }
                Ok(Done::Page { .. }) => {}
                Ok(Done::Stale) => {
                    // The file changed under the worker: read again from the path.
                    self.waiting = true;
                    gone = Some(None);
                    break;
                }
                Ok(Done::Failed(why)) => {
                    gone = Some(Some(format!("highlighting stopped ({why}); shown plain")));
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    gone = Some(Some(
                        "highlighting stopped unexpectedly; shown plain".to_string(),
                    ));
                    break;
                }
            }
        }
        if let Some(why) = gone {
            // The worker is over: the page stays plain and is ready, and nothing more is asked.
            page.highlighted = true;
            self.worker = None;
            if let Some(why) = why {
                self.status = Status::error(why);
            }
        }
    }

    fn bar(&self, ui: &mut egui::Ui) {
        let p = chrome::Palette::of(ui.visuals());
        let mut parts = vec![
            format!("{} lines", super::hex::grouped(self.doc.line_count())),
            crate::human(self.doc.size()),
            self.syntax
                .map_or("plain text", |s| s.name.as_str())
                .to_string(),
        ];
        if self.doc.long_lines() {
            parts.push(format!("lines over {LINE_MAX} bytes are cut"));
        }
        ui.horizontal(|ui| {
            super::one_line(ui, &self.title, p.text);
            ui.separator();
            super::one_line(ui, &parts.join(" · "), p.dim);
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let lines = self.doc.line_count();
        let shown = if lines == 0 {
            "empty".to_string()
        } else {
            let last = self.top.saturating_add(self.visible).min(lines);
            format!("lines {}–{last} of {lines}", self.top + 1)
        };
        ui.horizontal(|ui| {
            ui.monospace(shown);
            if !self.status.text.is_empty() {
                ui.separator();
                self.status.show(ui);
            }
        });
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // A re-read that landed is swapped in before anything reads or is measured (the
        // highlighter below starts on the new index).
        self.poll_reindex();
        let outer = ui.max_rect();
        let p = chrome::Palette::of(ui.visuals());
        // The window: its frame, titled as PC Shell's, the lines inside it.
        let rect = chrome::frame(&ui.painter_at(outer), outer, TITLE, true);
        let font = chrome::mono(ui);
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        // Rows touch, a cell apart, as on a text screen.
        let row_h = text_h.ceil();
        let lines = self.doc.line_count();
        self.visible = ((rect.height() / row_h).floor() as u64).max(1);
        // Scrolling waits for a re-read, as the keys do.
        if ui.rect_contains_pointer(outer) && !self.waiting && self.reindex.is_none() {
            let mut rows = 0f32;
            ctx.input(|i| {
                for event in &i.events {
                    if let egui::Event::MouseWheel { unit, delta, .. } = event {
                        rows -= match unit {
                            egui::MouseWheelUnit::Line => delta.y * 3.0,
                            egui::MouseWheelUnit::Page => delta.y * self.visible as f32,
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
                self.top = if moved < 0 {
                    self.top.saturating_sub(moved.unsigned_abs())
                } else {
                    self.top.saturating_add(moved as u64)
                };
            }
        }
        self.top = self.top.min(lines.saturating_sub(self.visible));
        if !self.hl_tried {
            self.hl_tried = true;
            if self.doc.size() <= HIGHLIGHT_MAX && !self.frozen() {
                let stop = self.doc.first_long.unwrap_or(u64::MAX);
                let hl = self.syntax.and_then(|s| Highlight::new(s, p.text, stop));
                #[cfg(test)]
                let hl = hl.map(|mut h| {
                    h.hook = self.hook.clone();
                    h.after_wake = self.after_wake.clone();
                    h
                });
                self.worker = match (hl, self.doc.share()) {
                    (Some(hl), Ok(doc)) => {
                        let started = Worker::start(doc, hl, ctx.clone(), &self.pool);
                        if started.is_none() {
                            self.status =
                                Status::info("highlighting is busy in other viewers; shown plain");
                        }
                        started
                    }
                    _ => None,
                };
            }
        }
        self.ensure_page(&ctx, p.text);
        if self.live && !self.frozen() && self.top.saturating_add(self.visible) >= lines {
            // At the end of a local file: looked at again each second with no input (one stat),
            // so a growing log is followed.
            ctx.request_repaint_after(FOLLOW_EVERY);
        }
        let painter = ui.painter_at(rect);
        // The scroll bar on the right border, beside the lines.
        let max_top = lines.saturating_sub(self.visible);
        chrome::scroll_bar(
            &ui.painter_at(outer),
            Rect::from_min_max(
                Pos2::new(rect.right(), rect.top()),
                Pos2::new(rect.right() + chrome::CELL.x, rect.bottom()),
            ),
            (max_top > 0).then(|| self.top as f32 / max_top as f32),
        );
        let digits = lines.max(1).to_string().len();
        let gutter = cw * (digits as f32 + 1.0);
        let text_x = rect.left() + gutter + cw;
        let text_painter =
            ui.painter_at(Rect::from_min_max(Pos2::new(text_x, rect.top()), rect.max));
        // Only the columns that fit are laid out: a line can be thousands of characters.
        let columns = ((rect.right() - text_x) / cw).floor().max(0.0) as usize;
        if let Some(page) = &self.page {
            for (k, runs) in page.runs.iter().enumerate() {
                let y = rect.top() + k as f32 * row_h;
                painter.text(
                    Pos2::new(rect.left() + gutter, y),
                    egui::Align2::RIGHT_TOP,
                    (page.top + k as u64 + 1).to_string(),
                    font.clone(),
                    p.dim,
                );
                let mut job = LayoutJob::default();
                let (mut skip, mut room) = (self.left, columns);
                for (color, text) in runs {
                    if room == 0 {
                        break;
                    }
                    let n = text.chars().count();
                    if skip >= n {
                        skip -= n;
                        continue;
                    }
                    let part: String = text
                        .chars()
                        .skip(std::mem::take(&mut skip))
                        .take(room)
                        .map(chrome::shown_char)
                        .collect();
                    room -= part.chars().count();
                    job.append(&part, 0.0, egui::TextFormat::simple(font.clone(), *color));
                }
                let galley = ctx.fonts_mut(|f| f.layout_job(job));
                text_painter.galley(Pos2::new(text_x, y), galley, p.text);
            }
        }
        self.painted = true;
    }
}

impl Window for TextWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.keys(&ui.ctx().clone());
        chrome::bar_panel(egui::Panel::top("sopen-text-bar")).show(ui, |ui| self.bar(ui));
        chrome::bar_panel(egui::Panel::bottom("sopen-text-status"))
            .show(ui, |ui| self.status_bar(ui));
        if !self.notices.is_empty() {
            let error = ui.visuals().error_fg_color;
            let lines: Vec<(String, Color32)> =
                self.notices.iter().map(|n| (n.clone(), error)).collect();
            egui::Panel::bottom("sopen-text-notes").show(ui, |ui| {
                ui.add_space(2.0);
                super::disclosures(ui, &lines);
                ui.add_space(2.0);
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.body(ui));
    }

    fn ready(&self) -> bool {
        self.painted
            && !self.waiting
            && self.reindex.is_none()
            && self
                .page
                .as_ref()
                .is_some_and(|p| p.highlighted || self.worker.is_none())
    }
}

/// The text viewer inside another window (the file browser). `title` is `host:/abs`, `name` the
/// file's name (it picks the syntax); `notices` are shown whole in its rows. `live`: the file is
/// read in place (followed at its end), not a fetched copy.
pub fn embed(
    doc: TextDoc,
    title: String,
    name: &str,
    notices: Vec<String>,
    live: bool,
) -> Embedded {
    let mut window = TextWindow::new(doc, title, name, notices);
    window.live = live;
    Embedded {
        window: Box::new(window),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

#[cfg(test)]
mod tests;
