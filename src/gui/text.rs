//! The text viewer (spec §3a): a text file read-only, with syntax highlighting (syntect —
//! Sublime syntaxes on the pure-Rust regex engine), line numbers, and paging: the file is read a
//! page at a time through a sparse line index, never whole, so a file of any size opens alike.
//!
//! The index is one pass over the file when it opens (on the opener's worker, stopped by Esc
//! between chunks): a mark every
//! [`BLOCK_LINES`] lines, and at the first line start [`SPAN`] bytes past the last mark. A line
//! that does not start at a mark starts less than a span past one, so finding any line reads at
//! most a span; a longer line is followed by a mark, so its end is known without reading it.
//!
//! Lines wrap by default ([`wrap`]): every line is shown whole, in rows broken by the measured
//! width of their glyphs, and a page reads only its rows' bytes — a line of hundreds of megabytes
//! too, from wherever the page is in it. The place on screen is a line and the byte of it the top
//! row starts at. W or F2 switch to one row per line, scrolled sideways, where only [`LINE_MAX`]
//! bytes of a line are shown.
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
//! cut would colour everything after it), and the rest of the file is plain (wrapped, still
//! whole). Files over [`HIGHLIGHT_MAX`] are always shown plain.
//!
//! The file is read live; the index is of the file as opened. A file that differs from it in any
//! way ([`TextDoc::now`]: written to, grown, truncated, another file renamed over its path) is
//! never read through the old index: the viewer indexes the path again on a worker of its own —
//! one at a time, the next no sooner than max(1 s, 4 × the last index pass) after the previous
//! ended — keeps the page on screen meanwhile, and swaps the new index in (following the end if
//! it was at the end). A deleted file keeps its page. Checked before each new page: the window says so and
//! reads no more. Every offset taken from the index is checked, never trusted.

mod wrap;

use super::{chrome, Embedded, Outcome, Status, Window};
use anyhow::{Context as _, Result};
use eframe::egui::{self, text::LayoutJob, Color32, FontId, Key, Modifiers, Pos2, Rect};
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
/// Bytes of a line shown when lines are not wrapped; the rest is cut (the bar says so). Also the
/// longest line highlighting parses.
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

    /// Where lines `first..first + count` are (fewer at the end of the file), for showing them
    /// whole (wrapped). Found as [`TextDoc::read_lines`] finds them — at most a span from the
    /// mark before `first`, then each line that ends before the next mark (such a line is short:
    /// it ends within a span of its block's mark), never past where the index says it must end —
    /// except that a line ending at a mark is not read at all but for its last byte (a `\r`).
    fn spans(&mut self, first: u64, count: usize) -> Result<Vec<Span>> {
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
                let newline = u64::from(next.line < self.lines || self.ends_newline);
                // Checked: a file changed since it was indexed can put `pos` past the mark.
                let mut len = next
                    .offset
                    .checked_sub(pos)
                    .and_then(|n| n.checked_sub(newline))
                    .unwrap_or(0);
                if keep {
                    if len > 0 && window.take(self, pos + len - 1, 1)?.first() == Some(&b'\r') {
                        len -= 1;
                    }
                    out.push(Span {
                        line,
                        start: pos,
                        len,
                    });
                }
                pos = next.offset;
                idx += 1;
            } else {
                // As in `read_lines`: this line ends before the next mark and within a span of
                // this block's mark.
                let end = next.offset.min(self.marks[idx].offset.saturating_add(SPAN));
                let start = pos;
                let mut before = None;
                loop {
                    let room = end.saturating_sub(pos);
                    if room == 0 {
                        return Err(anyhow::Error::new(Stale));
                    }
                    let avail = window.at(self, pos, room.min(CHUNK as u64) as usize)?;
                    let avail = &avail[..avail.len().min(room as usize)];
                    if avail.is_empty() {
                        return Err(anyhow::Error::new(Stale));
                    }
                    match avail.iter().position(|&b| b == b'\n') {
                        Some(i) => {
                            if i > 0 {
                                before = Some(avail[i - 1]);
                            }
                            let len = pos + i as u64 - start;
                            let cr = u64::from(len > 0 && before == Some(b'\r'));
                            if keep {
                                out.push(Span {
                                    line,
                                    start,
                                    len: len - cr,
                                });
                            }
                            pos += i as u64 + 1;
                            break;
                        }
                        None => {
                            before = avail.last().copied();
                            pos += avail.len() as u64;
                        }
                    }
                }
            }
            line += 1;
        }
        Ok(out)
    }

    /// Up to `n` bytes of `span` from `at` (fewer at its end), through `window`; `ahead`: each
    /// read fills it as far as [`CHUNK`] — never past the span — so the rows after are in it.
    /// Fewer bytes than the span has: [`Stale`].
    fn span_bytes(
        &mut self,
        span: Span,
        at: u64,
        n: usize,
        window: &mut Window_,
        ahead: bool,
    ) -> Result<Vec<u8>> {
        let end = span.start.saturating_add(span.len);
        let want = span.len.saturating_sub(at).min(n as u64) as usize;
        let mut out = Vec::with_capacity(want);
        let mut pos = span.start + at;
        while out.len() < want {
            let fill = if ahead {
                end.saturating_sub(pos).min(CHUNK as u64) as usize
            } else {
                want - out.len()
            };
            let avail = window.at(self, pos, fill)?;
            if avail.is_empty() {
                return Err(anyhow::Error::new(Stale));
            }
            let n = avail.len().min(want - out.len());
            out.extend_from_slice(&avail[..n]);
            pos += n as u64;
        }
        Ok(out)
    }

    /// The line byte `at` of the file is in (the last line for `at` past the end): read from the
    /// mark before it, as [`TextDoc::spans`] reads.
    fn span_at(&mut self, at: u64) -> Result<Option<Span>> {
        if self.lines == 0 {
            return Ok(None);
        }
        let idx = self
            .marks
            .partition_point(|m| m.offset <= at)
            .saturating_sub(1);
        let (from, to) = (self.marks[idx].line, self.next_mark(idx).line);
        let spans = self.spans(from, (to - from).max(1) as usize)?;
        Ok(spans
            .iter()
            .rev()
            .find(|s| s.start <= at)
            .or(spans.last())
            .copied())
    }
}

/// Where a line is: its first byte in the file, and how many bytes it shows (its newline, and a
/// `\r` before it, not counted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    line: u64,
    start: u64,
    len: u64,
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
    /// Lines on it (asked of the highlighter).
    count: usize,
    /// Rows it was read for.
    visible: usize,
    /// Not wrapped: each line's runs as painted. Wrapped: the highlighter's runs, once they come.
    runs: Vec<Runs>,
    highlighted: bool,
    /// The generation its highlighting was asked under.
    gen: u64,
    /// Read wrapped: the fields below are its rows.
    wrapped: bool,
    /// Wrapped: the byte of line `top` its first row starts at; the line's place in the file and
    /// length (the scroll bar, the status row).
    top_off: u64,
    top_start: u64,
    top_len: u64,
    /// Wrapped: the width its rows were laid out for, the rows, whether they reach the end of the
    /// file, and each line's colours by character once highlighted.
    width: f32,
    rows: Vec<wrap::Row>,
    at_end: bool,
    colors: Vec<Vec<Color32>>,
}

impl Page {
    /// A page of plain lines, not wrapped.
    fn lines(top: u64, count: usize, runs: Vec<Runs>, highlighted: bool, gen: u64) -> Self {
        Self {
            top,
            count,
            visible: count,
            runs,
            highlighted,
            gen,
            wrapped: false,
            top_off: 0,
            top_start: 0,
            top_len: 0,
            width: 0.0,
            rows: Vec::new(),
            at_end: false,
            colors: Vec::new(),
        }
    }

    /// Nothing on it (tests).
    #[cfg(test)]
    fn is_blank(&self) -> bool {
        self.runs.is_empty() && self.rows.is_empty()
    }
}

/// Where the keys asked a wrapped view to go (applied when the next page is read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jump {
    /// Home: the first row of the line at the top, or the start of the file when already there.
    Home,
    /// End: the last row of the line at the top at the bottom when the line runs past the page,
    /// else the end of the file.
    End,
    FileStart,
    FileEnd,
    /// The row byte `n` of the file is in (`SOPEN_STATE` `at=`).
    Byte(u64),
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
    /// The first line on screen, and how many rows fit (the last pass's).
    top: u64,
    visible: u64,
    /// Columns scrolled to the right (not wrapped).
    left: usize,
    /// Lines wrap (the default; W or F2 switch it).
    wrap: bool,
    /// Wrapped: the byte of line `top` the top row starts at.
    top_off: u64,
    /// Wrapped: where the keys asked to go, and how many rows the keys and the wheel moved by —
    /// applied before the next page is read.
    jump: Option<Jump>,
    moved: i64,
    /// Wrapped: the top is put back on a row start before the next page (the width, the index or
    /// the mode changed).
    resnap: bool,
    /// Wrapped: what wrapping found so far (for this index and width).
    memo: wrap::Memo,
    /// The width rows are laid out in (the last pass's).
    room: f32,
    /// A plain row was laid out wider than its room (the fonts changed since it was wrapped): the
    /// page was wrapped again, once.
    rewrapped: bool,
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

#[cfg(test)]
thread_local! {
    static FOLLOWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Tests: how many times a viewer on this thread asked to look at its file again
/// ([`FOLLOW_EVERY`]) — the follow itself, not any repaint that happens to be a second away (the
/// title row's clock asks for one in a minute's last second).
#[cfg(test)]
pub(crate) fn follows() -> usize {
    FOLLOWS.with(std::cell::Cell::get)
}

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
            wrap: true,
            top_off: 0,
            jump: None,
            moved: 0,
            resnap: false,
            memo: wrap::Memo::default(),
            room: 0.0,
            rewrapped: false,
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

    /// The end of the file is on screen: a local file is followed there, and a re-read shows the
    /// new end. Wrapped, the page knows (its last row ends the file); else by lines.
    fn at_end(&self) -> bool {
        match &self.page {
            Some(p) if p.wrapped && self.wrap => p.at_end,
            // Nothing wrapped on screen yet: no end seen to follow.
            _ if self.wrap => false,
            _ => self.top.saturating_add(self.visible) >= self.doc.line_count(),
        }
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
        let at_end = self.at_end();
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
                let lines = self.doc.line_count();
                // What wrapping found was of the old index (also when not wrapped now).
                self.memo.reset(self.room);
                if self.wrap {
                    // Wrapped again through the new index: the new end, or the same place put
                    // back on a row start (the line may have changed).
                    if at_end {
                        self.jump = Some(Jump::FileEnd);
                    } else {
                        self.top = self.top.min(lines.saturating_sub(1));
                        self.resnap = true;
                    }
                } else {
                    let last_top = lines.saturating_sub(self.visible);
                    self.top = if at_end {
                        last_top
                    } else {
                        self.top.min(last_top)
                    };
                }
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
        // Cmd on macOS (Ctrl elsewhere), or Ctrl.
        let command = |key: Key| {
            ctx.input_mut(|i| {
                i.consume_key(Modifiers::COMMAND, key) || i.consume_key(Modifiers::CTRL, key)
            })
        };
        if (pressed(Key::W) | pressed(Key::F2)) && !self.gone {
            // The same line at the top, from its start. (A deleted file keeps its page as it
            // was read: nothing is read again to show it the other way.)
            self.wrap = !self.wrap;
            self.top_off = 0;
            self.left = 0;
            self.jump = None;
            self.moved = 0;
        }
        let page = self.visible.saturating_sub(1).max(1);
        let (file_start, file_end) = (command(Key::Home), command(Key::End));
        if self.wrap {
            // By rows, from the rows read when the next page is (going back reads only from the
            // restart point before a row).
            let page = page as i64;
            for (key, rows) in [
                (Key::ArrowDown, 1),
                (Key::ArrowUp, -1),
                (Key::PageDown, page),
                (Key::PageUp, -page),
            ] {
                if pressed(key) {
                    self.moved += rows;
                }
            }
            for (wanted, jump) in [
                (file_start, Jump::FileStart),
                (file_end, Jump::FileEnd),
                (pressed(Key::Home), Jump::Home),
                (pressed(Key::End), Jump::End),
            ] {
                if wanted {
                    self.jump = Some(jump);
                }
            }
            return;
        }
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
        if pressed(Key::Home) | file_start {
            self.top = 0;
        }
        if pressed(Key::End) | file_end {
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
    fn ensure_page(&mut self, ctx: &egui::Context, plain: Color32, font: &FontId) {
        let (top, count) = (self.top, self.visible as usize);
        let moving = self.jump.is_some() || self.moved != 0 || self.resnap;
        let same = !self.stale_page
            && !moving
            && self.page.as_ref().is_some_and(|p| {
                p.wrapped == self.wrap
                    && p.top == top
                    && p.visible == count
                    && (!p.wrapped || (p.top_off == self.top_off && p.width == self.room))
            });
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
                self.page = Some(Page::lines(top, count, Vec::new(), true, 0));
            }
            if let Some(page) = self.page.as_mut() {
                page.highlighted = true;
            }
            return;
        }
        if !same {
            let read = if self.wrap {
                self.read_wrapped(ctx, plain, font, count)
            } else {
                self.read_unwrapped(count, plain)
            };
            let mut page = match read {
                Ok(page) => page,
                Err(e) if is_stale(&e) => {
                    // Changed after the check, during the read: read again from the path.
                    self.waiting = true;
                    return self.ensure_page(ctx, plain, font);
                }
                Err(e) => {
                    self.status = Status::error(format!("{e:#}"));
                    // Not asked again until something moves.
                    (self.jump, self.moved, self.resnap) = (None, 0, false);
                    let mut page = Page::lines(self.top, count, Vec::new(), false, 0);
                    page.wrapped = self.wrap;
                    page.top_off = self.top_off;
                    page.width = self.room;
                    page
                }
            };
            self.stale_page = false;
            self.gen += 1;
            page.gen = self.gen;
            page.highlighted = self.worker.is_none();
            if let Some(worker) = &self.worker {
                worker.ask(self.gen, page.top, page.count);
            }
            self.page = Some(page);
            // The status row above was drawn before this page was read: once more for it.
            ctx.request_repaint();
        }
        self.worker_results();
        if (self.waiting || due) && self.reindex.is_none() {
            // A change (also one the highlighter found): the re-read starts, or is scheduled,
            // in this frame, not at the next input.
            self.start_reindex(ctx);
        }
    }

    /// The page of `count` lines at `top`, one row each, cut at [`LINE_MAX`] (not wrapped).
    fn read_unwrapped(&mut self, count: usize, plain: Color32) -> Result<Page> {
        let lines = self.doc.line_count();
        if let Some(Jump::Byte(at)) = self.jump {
            if let Some(span) = self.doc.span_at(at)? {
                self.top = span.line.min(lines.saturating_sub(self.visible));
            }
        }
        (self.jump, self.moved, self.resnap) = (None, 0, false);
        let read = self.doc.read_lines(self.top, count)?;
        let runs = read
            .iter()
            .map(|l| expand_tabs(vec![(plain, l.text.clone())]))
            .collect();
        Ok(Page::lines(self.top, count, runs, false, 0))
    }

    /// The page of `rows` rows from the top, wrapped — after the moves the keys asked for — and
    /// the top it ends up at: never past the last page (the file's last row at the bottom).
    fn read_wrapped(
        &mut self,
        ctx: &egui::Context,
        plain: Color32,
        font: &FontId,
        rows: usize,
    ) -> Result<Page> {
        use wrap::{Measure, Pos, Wrapper};
        let lines = self.doc.line_count();
        if self.memo.rewidth(self.room) {
            self.resnap = true;
        }
        let cell = ctx.fonts_mut(|f| f.glyph_width(font, '0')).max(1.0);
        let mut w = Wrapper {
            doc: &mut self.doc,
            memo: &mut self.memo,
            m: Measure::new(ctx, font.clone(), plain),
            width: self.room,
            // A row is read at most this far (a row of combining marks is cut there).
            read: 16 * (self.room / cell).ceil().max(1.0) as usize + 256,
        };
        let mut pos = Pos {
            line: self.top.min(lines.saturating_sub(1)),
            off: self.top_off,
        };
        if lines > 0 {
            if self.resnap {
                pos = w.snap(pos)?;
            }
            let back = rows.saturating_sub(1) as u64;
            pos = match self.jump {
                None => pos,
                Some(Jump::FileStart) => Pos::default(),
                Some(Jump::FileEnd) => w.end(rows)?,
                Some(Jump::Home) if pos.off > 0 => Pos {
                    line: pos.line,
                    off: 0,
                },
                Some(Jump::Home) => Pos::default(),
                Some(Jump::End) => {
                    let line_end = w.last_of(pos.line)?;
                    let top = w.back(line_end, back)?;
                    if top > pos {
                        top
                    } else {
                        w.end(rows)?
                    }
                }
                Some(Jump::Byte(at)) => w.at_byte(at)?,
            };
            if self.moved > 0 {
                pos = w.forward(pos, self.moved as u64)?;
            } else if self.moved < 0 {
                pos = w.back(pos, self.moved.unsigned_abs())?;
            }
        }
        let (mut got, mut at_end) = w.page(pos, rows)?;
        if got.len() < rows && pos != Pos::default() {
            // Short of the window: the last page instead, no blank rows under the end.
            pos = w.end(rows)?;
            (got, at_end) = w.page(pos, rows)?;
        }
        let (top_start, top_len) = match lines {
            0 => (0, 0),
            _ => {
                let span = w.span(pos.line)?;
                (span.start, span.len)
            }
        };
        (self.jump, self.moved, self.resnap) = (None, 0, false);
        (self.top, self.top_off) = (pos.line, pos.off);
        let count = got.last().map_or(0, |r| (r.line - pos.line + 1) as usize);
        let mut page = Page::lines(pos.line, count, Vec::new(), false, 0);
        page.visible = rows;
        page.wrapped = true;
        page.top_off = pos.off;
        page.top_start = top_start;
        page.top_len = top_len;
        page.width = self.room;
        page.rows = got;
        page.at_end = at_end;
        Ok(page)
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
                    if page.wrapped {
                        // Each character's colour: the rows take theirs by index.
                        page.colors = runs
                            .iter()
                            .map(|line| {
                                line.iter()
                                    .flat_map(|(c, t)| std::iter::repeat_n(*c, t.chars().count()))
                                    .collect()
                            })
                            .collect();
                    } else {
                        page.runs = runs.into_iter().map(expand_tabs).collect();
                    }
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
        if self.doc.long_lines() && !self.wrap {
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
        let shown = match &self.page {
            _ if lines == 0 => "empty".to_string(),
            Some(page) if page.wrapped && !page.rows.is_empty() => {
                // Lines as on screen; inside a line, how far into it the top row is.
                let last = page.rows.last().map_or(page.top, |r| r.line) + 1;
                let mut first = (page.top + 1).to_string();
                if page.top_off > 0 && page.top_len > 0 {
                    first.push_str(&format!(" ({} % of it)", page.top_off * 100 / page.top_len));
                }
                if last == page.top + 1 {
                    format!("line {first} of {lines}")
                } else {
                    format!("lines {first}–{last} of {lines}")
                }
            }
            _ => {
                let last = self.top.saturating_add(self.visible).min(lines);
                format!("lines {}–{last} of {lines}", self.top + 1)
            }
        };
        ui.horizontal(|ui| {
            ui.monospace(shown);
            if !self.status.text.is_empty() {
                ui.separator();
                self.status.show(ui);
            }
        });
    }

    /// Where the scroll bar's thumb is (0 at the top, 1 at the end; `None`: nothing to scroll).
    /// Wrapped, by bytes: rows cannot be counted without reading every line.
    fn thumb(&self) -> Option<f32> {
        match &self.page {
            Some(p) if p.wrapped => {
                let at_start = p.top == 0 && p.top_off == 0;
                match (p.at_end, at_start) {
                    (true, true) => None,
                    (true, false) => Some(1.0),
                    _ => Some((p.top_start + p.top_off) as f32 / self.doc.size().max(1) as f32),
                }
            }
            _ => {
                let max_top = self.doc.line_count().saturating_sub(self.visible);
                (max_top > 0).then(|| self.top as f32 / max_top as f32)
            }
        }
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
        let digits = lines.max(1).to_string().len();
        let gutter = cw * (digits as f32 + 1.0);
        let text_x = rect.left() + gutter + cw;
        self.room = (rect.right() - text_x).max(0.0);
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
                if self.wrap {
                    self.moved += moved;
                } else {
                    self.top = if moved < 0 {
                        self.top.saturating_sub(moved.unsigned_abs())
                    } else {
                        self.top.saturating_add(moved as u64)
                    };
                }
            }
        }
        if !self.wrap {
            self.top = self.top.min(lines.saturating_sub(self.visible));
        }
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
        self.ensure_page(&ctx, p.text, &font);
        if self.live && !self.frozen() && self.at_end() {
            // At the end of a local file: looked at again each second with no input (one stat),
            // so a growing log is followed.
            ctx.request_repaint_after(FOLLOW_EVERY);
            #[cfg(test)]
            FOLLOWS.with(|n| n.set(n.get() + 1));
        }
        let painter = ui.painter_at(rect);
        // The scroll bar on the right border, beside the lines.
        chrome::scroll_bar(
            &ui.painter_at(outer),
            Rect::from_min_max(
                Pos2::new(rect.right(), rect.top()),
                Pos2::new(rect.right() + chrome::CELL.x, rect.bottom()),
            ),
            self.thumb(),
        );
        let text_painter =
            ui.painter_at(Rect::from_min_max(Pos2::new(text_x, rect.top()), rect.max));
        let number = |k: usize, line: u64| {
            painter.text(
                Pos2::new(rect.left() + gutter, rect.top() + k as f32 * row_h),
                egui::Align2::RIGHT_TOP,
                (line + 1).to_string(),
                font.clone(),
                p.dim,
            );
        };
        let mut overflow = false;
        match &self.page {
            Some(page) if page.wrapped => {
                for (k, row) in page.rows.iter().enumerate() {
                    // The number on a line's first row only.
                    if row.start == 0 {
                        number(k, row.line);
                    }
                    let colors = row.first_char.and_then(|first| {
                        let line = page.colors.get((row.line - page.top) as usize)?;
                        Some((first, line.as_slice()))
                    });
                    let plain = || {
                        ctx.fonts_mut(|f| f.layout_no_wrap(row.cut.text(), font.clone(), p.text))
                    };
                    let mut galley = match colors {
                        Some(colors) => {
                            ctx.fonts_mut(|f| f.layout_job(row_job(row, colors, &font, p.text)))
                        }
                        None => plain(),
                    };
                    if galley.size().x > page.width + 0.5 && colors.is_some() {
                        // Coloured runs are shaped apart and can come out wider: plain then.
                        galley = plain();
                    }
                    overflow |= galley.size().x > page.width + 0.5;
                    text_painter.galley(
                        Pos2::new(text_x, rect.top() + k as f32 * row_h),
                        galley,
                        p.text,
                    );
                }
            }
            Some(page) => {
                // Only the columns that fit are laid out: a line can be thousands of characters.
                let columns = ((rect.right() - text_x) / cw).floor().max(0.0) as usize;
                for (k, runs) in page.runs.iter().enumerate() {
                    number(k, page.top + k as u64);
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
                    text_painter.galley(
                        Pos2::new(text_x, rect.top() + k as f32 * row_h),
                        galley,
                        p.text,
                    );
                }
            }
            None => {}
        }
        if overflow && !self.rewrapped && !self.frozen() {
            // A plain row wider than it was wrapped for: the fonts changed since (a fallback
            // font came in). Wrapped again from scratch, once.
            self.rewrapped = true;
            self.memo.reset(self.room);
            self.resnap = true;
            ctx.request_repaint();
        }
        self.painted = true;
    }
}

/// A wrapped row as painted in its line's colours: `colors` holds the line's colour per
/// character, the row's first character at index `first`.
fn row_job(
    row: &wrap::Row,
    (first, colors): (usize, &[Color32]),
    font: &FontId,
    plain: Color32,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let (mut run, mut color) = (String::new(), plain);
    for (i, &(c, n)) in row.cut.shown.iter().enumerate() {
        let now = colors.get(first + i).copied().unwrap_or(plain);
        if now != color && !run.is_empty() {
            job.append(&run, 0.0, egui::TextFormat::simple(font.clone(), color));
            run.clear();
        }
        color = now;
        run.extend(std::iter::repeat_n(c, usize::from(n)));
    }
    if !run.is_empty() {
        job.append(&run, 0.0, egui::TextFormat::simple(font.clone(), color));
    }
    job
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

    /// F2 switches wrapping: the row says what it does now.
    fn keys(&self) -> &'static [chrome::FKey] {
        if self.wrap {
            chrome::TEXT_WRAPPED_KEYS
        } else {
            chrome::TEXT_UNWRAPPED_KEYS
        }
    }
}

/// The text viewer inside another window (the file browser). `title` is `host:/abs`, `name` the
/// file's name (it picks the syntax); `notices` are shown whole in its rows. `live`: the file is
/// read in place (followed at its end), not a fetched copy. `at`: the row byte `at` of the file
/// is in goes to the top (`SOPEN_STATE` `at=`, a receipt).
pub fn embed(
    doc: TextDoc,
    title: String,
    name: &str,
    notices: Vec<String>,
    live: bool,
    at: Option<u64>,
) -> Embedded {
    let mut window = TextWindow::new(doc, title, name, notices);
    window.live = live;
    window.jump = at.map(Jump::Byte);
    Embedded {
        window: Box::new(window),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

#[cfg(test)]
mod tests;
