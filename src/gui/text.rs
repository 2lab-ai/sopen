//! The text viewer (spec §3a): a text file read-only, with syntax highlighting (syntect —
//! Sublime syntaxes on the pure-Rust regex engine), line numbers, and paging: the file is read a
//! page at a time through a sparse line index, never whole, so a file of any size opens alike.
//!
//! The index is one pass over the file when it opens (on the opener's worker, stopped by Esc
//! between chunks): a mark every
//! [`BLOCK_LINES`] lines, and at the first line start [`SPAN`] bytes past the last mark. A line
//! that does not start at a mark starts less than a span past one, so finding any line reads at
//! most a span; a longer line is followed by a mark, so its end is known without reading it. A
//! preview's pass (the file browser's, 02 §4.3) stops after its budget, 4 MiB, keeping where it
//! stopped and its marks; the hand-over resumes it from there ([`TextDoc::resume`]), so no byte is
//! indexed twice. A preview draws without its bars, takes no input and neither follows nor re-reads
//! its file ([`Window::set_preview`]).
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
//!
//! A remote file read through a [`Source`] (06 §7.2, [`TextDoc::streamed`]) reads through
//! [`Bytes`] and is not indexed at open. Its first 64 KiB block is its first screen: until it
//! arrives the window says `loading…`, and nothing else is fetched before that screen is drawn.
//! The index then grows from byte 0 on the frames, through a Bulk reader, as far as
//! [`HIGHLIGHT_MAX`] (a preview: [`PREVIEW_INDEX`]); the top bar says how far the lines are
//! known, and the gutter is sized once, for the most lines that index can hold. A read whose
//! bytes are not here yet is [`Pending`]: it travels up as [`Stale`] does, in an arm of its own
//! — the page on screen stays, where the keys asked to go is kept, and the Source's waker
//! brings the pass that reads again. A read that failed keeps the screen and the keys too, with
//! the reason on the status row; the next key, click or scroll retries. The highlighting worker
//! reads through a reader of its own with the blocking read, so it waits for its bytes and
//! never sees `Pending`; it highlights over the indexed lines as they grow. A file that
//! changed on the remote freezes the page until Cmd/Ctrl+R, which re-opens it and indexes it
//! again from its first block. Until the routes open a Source (06 §13: W8, after W5b), remote
//! text is still fetched whole and read as a local file.

mod wrap;

use super::{chrome, Embedded, Outcome, Status, Window};
use crate::stream::{Bytes, ReadError, ReaderKind, Source};
use anyhow::{Context as _, Result};
use eframe::egui::{self, text::LayoutJob, Color32, FontId, Key, Modifiers, Pos2, Rect};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::sync::OnceLock;
use std::task::Poll;
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
    backing: Backing,
    /// The bytes the index covers: the file's, or the prefix a stopped or growing pass read.
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
    /// The index pass stopped short of the file, at a preview's budget (02 §4.3): where it
    /// stopped and what it had found, for [`TextDoc::resume`] to go on from. The index covers
    /// the bytes before that point only.
    rest: Option<Scan>,
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

/// What a document reads through (06 §5's `Bytes`): a local file in place, or a remote one's
/// stream.
enum Backing {
    Local(File),
    Stream(Box<Stream>),
}

/// A remote file read through a [`Source`] (06 §7.2): the index is not made at open but grows
/// from byte 0 — the first block when it arrives, then a pass of its own.
struct Stream {
    /// The window's document keeps the Source (it makes the readers); a worker's copy has only
    /// its reader, so a worker never keeps the stream open.
    source: Option<Source>,
    /// What lines are read through: the screen's reader, or a worker's own.
    reader: Box<dyn Bytes + Send>,
    /// The index pass's reader (the window's document only).
    pass: Option<Box<dyn Bytes + Send>>,
    /// The pass's last stretch before a cap that is not the file's end: read in one piece
    /// through a reader of its own, which has no readahead (the pass's own, running in
    /// sequence, would fetch up to a megabyte past the cap).
    tail: Option<Box<dyn Bytes + Send>>,
    /// The file's size as the Source opened it.
    total: u64,
    /// The pass so far.
    scan: Scan,
    /// A worker's copy: reads wait for their bytes (the blocking `read_at`) until this is set.
    /// A frame's never waits: it gets [`Pending`].
    blocking: Option<Arc<AtomicBool>>,
}

/// A read whose bytes are not here yet (06 §7.2): the fetch is queued and the Source's waker
/// brings the next pass. Carried up as [`Stale`] is; never an error to show.
#[derive(Debug)]
pub struct Pending;

impl std::fmt::Display for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the bytes have not arrived yet")
    }
}

impl std::error::Error for Pending {}

/// Whether `e` says the bytes are still on their way ([`Pending`]).
pub fn is_pending(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Pending>())
}

/// Why a stream's read gave no bytes, if `e` is one ([`ReadError`]).
fn read_error(e: &anyhow::Error) -> Option<&ReadError> {
    e.chain().find_map(|c| c.downcast_ref::<ReadError>())
}

/// What a step of a stream's index pass added ([`TextDoc::grow`]): the worker's copy of the
/// index takes the same ([`TextDoc::apply`]).
#[derive(Debug, Clone)]
struct Growth {
    size: u64,
    lines: u64,
    ends_newline: bool,
    first_long: Option<u64>,
    /// How many marks the index had before, and the ones added.
    marks_before: usize,
    marks: Vec<Mark>,
    /// The index's last line had no end yet (it may be longer now), and which line that was.
    open_line: Option<u64>,
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
        Self::open_scan(path, &|| None, &|| stopped(cancel), on_chunk)
    }

    /// Opens `path` and indexes its lines as far as `cap()` says (asked again after every
    /// chunk: `None` = to the end; a preview's budget is 4 MiB, lifted by the hand-over), with
    /// `check()` after every chunk (an error stops the pass: a cancel, a preview's deadline). A
    /// pass stopped by the cap is an index of the bytes before it ([`TextDoc::cut_short`]) that
    /// [`TextDoc::resume`] goes on from.
    pub fn open_capped(
        path: &Path,
        cap: &dyn Fn() -> Option<u64>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        Self::open_scan(path, cap, check, &mut |_| {})
    }

    fn open_scan(
        path: &Path,
        cap: &dyn Fn() -> Option<u64>,
        check: &dyn Fn() -> Result<()>,
        on_chunk: &mut dyn FnMut(u64),
    ) -> Result<Self> {
        check()?;
        let started = std::time::Instant::now();
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {}", path.display()))?;
        let opened = identity(&meta);
        let mut scan = Scan::default();
        if cap().is_some() {
            // A pass that may stop short hashes what it reads: its resume can then tell the
            // bytes it indexed are still the file's when the file was written to since.
            scan.hasher = Some(sha2::Sha256::default());
        }
        scan.run(&file, path, opened.2, cap, check, on_chunk)?;
        let mut doc = Self::indexed(file, scan, opened, path.to_path_buf());
        doc.indexed_in = started.elapsed();
        // Checked here, right after the pass (on the opener's or re-read's worker), as well as
        // when the viewer takes the index: a change between the two is caught by one of them.
        doc.after_pass = doc.now();
        Ok(doc)
    }

    /// The index a pass found (`scan`), for the file as `opened`: of the bytes it read, and no
    /// mark at or past their end. A pass the cap stopped keeps what it found, to go on from.
    fn indexed(file: File, scan: Scan, opened: Identity, path: std::path::PathBuf) -> Self {
        // The bytes actually indexed (fewer than the size as opened if it shrank meanwhile: the
        // change is noticed later).
        let mut doc = Self {
            backing: Backing::Local(file),
            size: 0,
            marks: Vec::new(),
            lines: 0,
            ends_newline: false,
            first_long: None,
            opened,
            indexed_in: std::time::Duration::ZERO,
            after_pass: Now::Same,
            path,
            page_reads: 0,
            rest: None,
            #[cfg(test)]
            fail_worker_reads: false,
            #[cfg(test)]
            fail_reads: false,
            #[cfg(test)]
            stale_worker_reads: false,
            #[cfg(test)]
            stale_reads: false,
        };
        doc.install(&scan);
        doc.rest = scan.capped.then_some(scan);
        doc
    }

    /// A remote file read through `source` (06 §7.2), nothing of it read yet: its index grows
    /// from byte 0 ([`TextDoc::grow`]) — the first block through the screen's reader, the rest
    /// through a Bulk reader of its own.
    pub fn streamed(source: Source) -> Self {
        let total = source.size();
        let scan = Scan {
            marks: vec![Mark { line: 0, offset: 0 }],
            ..Scan::default()
        };
        let stream = Stream {
            reader: Box::new(source.reader(ReaderKind::Interactive)),
            pass: Some(Box::new(source.reader(ReaderKind::Bulk))),
            tail: None,
            source: Some(source),
            total,
            scan: scan.clone(),
            blocking: None,
        };
        let mut doc = Self {
            backing: Backing::Stream(Box::new(stream)),
            size: 0,
            marks: Vec::new(),
            lines: 0,
            ends_newline: false,
            first_long: None,
            opened: (0, 0, total, None),
            indexed_in: std::time::Duration::ZERO,
            after_pass: Now::Same,
            path: std::path::PathBuf::new(),
            page_reads: 0,
            rest: None,
            #[cfg(test)]
            fail_worker_reads: false,
            #[cfg(test)]
            fail_reads: false,
            #[cfg(test)]
            stale_worker_reads: false,
            #[cfg(test)]
            stale_reads: false,
        };
        doc.install(&scan);
        doc
    }

    fn stream(&self) -> Option<&Stream> {
        match &self.backing {
            Backing::Stream(stream) => Some(stream),
            Backing::Local(_) => None,
        }
    }

    /// Read through a [`Source`], not a local file in place.
    pub fn is_stream(&self) -> bool {
        self.stream().is_some()
    }

    /// The Source a stream is read through (the window's document; a worker's copy has none).
    pub fn source(&self) -> Option<&Source> {
        self.stream().and_then(|s| s.source.as_ref())
    }

    /// The file's size: a stream's as its Source opened it, else what the index covers.
    pub fn total(&self) -> u64 {
        self.stream().map_or(self.size, |s| s.total)
    }

    /// The index covers the whole file (always, for a local file's: a preview's stopped pass
    /// is [`TextDoc::cut_short`]).
    pub fn whole(&self) -> bool {
        self.size >= self.total()
    }

    /// A stream's index pass has more to read below `cap` bytes.
    pub fn growing(&self, cap: u64) -> bool {
        self.stream().is_some_and(|s| s.scan.at < s.total.min(cap))
    }

    /// One step of a stream's index pass (06 §7.2): up to `budget` more bytes, a block at a
    /// time, as far as `cap` — the first block alone (the first screen needs it and nothing
    /// else), asked for by the screen's reader; the rest through the pass's own. `Ok(None)`:
    /// nothing more to index below the cap. [`Pending`]: the next block is on its way and
    /// nothing was added.
    fn grow(&mut self, cap: u64, budget: u64) -> Result<Option<Growth>> {
        let (before, lines, ends_newline) = (self.marks.len(), self.lines, self.ends_newline);
        let Backing::Stream(stream) = &mut self.backing else {
            return Ok(None);
        };
        let end = stream.total.min(cap);
        let from = stream.scan.at;
        if from >= end {
            return Ok(None);
        }
        // How far past a read a Bulk reader's readahead goes.
        let (ahead, _) = crate::stream::limits(ReaderKind::Bulk);
        let mut failed = None;
        while stream.scan.at < end && stream.scan.at - from < budget {
            let at = stream.scan.at;
            let block = crate::stream::BLOCK;
            let mut want = (end - at).min(block - at % block) as usize;
            if at == 0 {
                // The screen asks for the file's first byte: its block is a demand, and
                // nothing beside it is fetched (a longer read would bring its margin, 06 §4).
                match stream.reader.try_read(0, 1) {
                    Poll::Ready(Ok(_)) => {}
                    Poll::Ready(Err(e)) => return Err(anyhow::Error::new(e)),
                    Poll::Pending => return Err(anyhow::Error::new(Pending)),
                }
            }
            let last_stretch = end < stream.total && at > 0 && end - at < ahead + block;
            if last_stretch && at > from {
                // Read in one piece (below), up to a block more than a pass's budget: in a
                // pass of its own, not on top of what this one has fed.
                break;
            }
            if !last_stretch {
                stream.tail = None;
            } else if stream.tail.is_none() {
                stream.tail = stream
                    .source
                    .as_ref()
                    .map(|s| Box::new(s.reader(ReaderKind::Bulk)) as Box<dyn Bytes + Send>);
            }
            let reader = match (stream.tail.as_mut(), stream.pass.as_mut()) {
                (Some(tail), _) => {
                    want = (end - at) as usize;
                    tail
                }
                (None, Some(pass)) => pass,
                (None, None) => &mut stream.reader,
            };
            match reader.try_read(at, want) {
                Poll::Ready(Ok(bytes)) if bytes.is_empty() => break,
                Poll::Ready(Ok(bytes)) => stream.scan.feed(&bytes, stream.total),
                Poll::Ready(Err(e)) => {
                    failed = Some(anyhow::Error::new(e));
                    break;
                }
                Poll::Pending => {
                    failed = Some(anyhow::Error::new(Pending));
                    break;
                }
            }
            if at == 0 {
                // The first screen first: the pass goes on once it is up.
                break;
            }
        }
        if stream.scan.at == from {
            return match failed {
                Some(e) => Err(e),
                None => Ok(None),
            };
        }
        let scan = stream.scan.clone();
        self.install(&scan);
        Ok(Some(Growth {
            size: self.size,
            lines: self.lines,
            ends_newline: self.ends_newline,
            first_long: self.first_long,
            marks_before: before,
            marks: self.marks[before.min(self.marks.len())..].to_vec(),
            open_line: (lines > 0 && !ends_newline).then(|| lines - 1),
        }))
    }

    /// A worker's copy: its reads wait for their bytes, until `cancel` is set (06 §7.2: the
    /// blocking `read_at`, so [`Pending`] never reaches a worker). A local file's never wait.
    fn block_until(&mut self, cancel: Arc<AtomicBool>) {
        if let Backing::Stream(stream) = &mut self.backing {
            stream.blocking = Some(cancel);
        }
    }

    /// A stream's reader was made before a reload (06 §5): it reads nothing more.
    fn reloaded(&mut self) -> bool {
        match &mut self.backing {
            Backing::Stream(stream) => matches!(
                stream.reader.try_read(0, 0),
                Poll::Ready(Err(ReadError::Reloaded))
            ),
            Backing::Local(_) => false,
        }
    }

    /// A worker's copy of a stream's index takes what the window's pass added.
    fn apply(&mut self, growth: &Growth) {
        self.marks.truncate(growth.marks_before);
        self.marks.extend_from_slice(&growth.marks);
        self.size = growth.size;
        self.lines = growth.lines;
        self.ends_newline = growth.ends_newline;
        self.first_long = growth.first_long;
    }

    /// The index is what `scan` found so far: of the bytes it read, and no mark at or past
    /// their end.
    fn install(&mut self, scan: &Scan) {
        let size = scan.at;
        self.marks.clone_from(&scan.marks);
        self.marks.retain(|m| m.line == 0 || m.offset < size);
        self.first_long = scan.first_long;
        if scan.line_len > LINE_MAX as u64 && self.first_long.is_none() {
            self.first_long = Some(scan.line);
        }
        self.ends_newline = scan.last_byte == Some(b'\n');
        self.lines = scan.line + u64::from(scan.last_byte.is_some() && !self.ends_newline);
        self.size = size;
    }

    /// The index stopped short of the file (a preview's budget): it covers [`TextDoc::size`]
    /// bytes of more.
    pub fn cut_short(&self) -> bool {
        self.rest.is_some()
    }

    /// The whole file's index, for an index that stopped short ([`TextDoc::cut_short`]): the
    /// pass goes on from where it stopped, with the marks it had, through a handle of its own on
    /// the same file, to the size the file had when it was opened — each byte indexed once; the
    /// bytes before the cut are read twice more only to verify them (hashed before and after),
    /// and a mismatch is [`Stale`] (the file is indexed again whole). Stops when `cancel` is set.
    /// An index that is whole comes back as it is (on a handle of its own).
    pub fn resume(&self, cancel: &AtomicBool) -> Result<TextDoc> {
        self.resume_counted(cancel, &mut |_| {})
    }

    /// The bytes a stopped pass indexed are still the file's: the open handle and the path show
    /// the same file, no shorter ([`TextDoc::now`]), and every one of those bytes, read once
    /// more, hashes as it did when the pass read them. No stat can say they were not written
    /// over (a write keeps the size; the modification time can be put back), so they are always
    /// read. This read only checks: it is not an index read, and indexes nothing.
    fn prefix_unchanged(&self, file: &File, scan: &Scan) -> bool {
        if matches!(self.now_since_pass(), Now::Changed | Now::Gone) {
            return false;
        }
        let Some(want) = scan.hasher.clone().map(sha2::Digest::finalize) else {
            return false;
        };
        let mut hasher = sha2::Sha256::default();
        let mut buf = vec![0u8; PASS_READ];
        let mut at = 0u64;
        while at < scan.at {
            let n = (scan.at - at).min(buf.len() as u64) as usize;
            if file.read_exact_at(&mut buf[..n], at).is_err() {
                return false;
            }
            #[cfg(test)]
            log_verify_read(&self.path, at, n as u64);
            sha2::Digest::update(&mut hasher, &buf[..n]);
            at += n as u64;
        }
        sha2::Digest::finalize(hasher) == want
    }

    /// [`TextDoc::resume`], with `on_chunk(bytes indexed so far)` after every chunk.
    fn resume_counted(
        &self,
        cancel: &AtomicBool,
        on_chunk: &mut dyn FnMut(u64),
    ) -> Result<TextDoc> {
        let started = std::time::Instant::now();
        let Backing::Local(file) = &self.backing else {
            return self.share();
        };
        let file = file.try_clone().context("cannot open the file again")?;
        let mut scan = match &self.rest {
            Some(rest) => rest.clone(),
            None => return self.share(),
        };
        // The marks found so far describe the bytes as they were read: they are joined to the
        // rest only if those bytes are still there — before the rest is read, and again after
        // (written over meanwhile). Written over in place, truncated and written again: Stale,
        // and the file is read again whole.
        if !self.prefix_unchanged(&file, &scan) {
            return Err(anyhow::Error::new(Stale));
        }
        let before = scan.clone();
        scan.capped = false;
        scan.run(
            &file,
            &self.path,
            self.opened.2,
            &|| None,
            &|| stopped(cancel),
            on_chunk,
        )?;
        if !self.prefix_unchanged(&file, &before) {
            return Err(anyhow::Error::new(Stale));
        }
        let mut doc = Self::indexed(file, scan, self.opened, self.path.clone());
        doc.indexed_in = self.indexed_in + started.elapsed();
        doc.after_pass = doc.now();
        Ok(doc)
    }

    /// What the file is now, against the index: [`Now::Same`] only when the open handle and the
    /// path it was opened by (followed through links, as opened) both still show the indexed
    /// device, inode, size and modification time; [`Now::Gone`] when nothing is at the path
    /// (deleted); [`Now::Grown`] when both still show the indexed device and inode, the scan read
    /// every byte the file had as opened (or stopped at a preview's budget), and both are no
    /// shorter than that (appended to, or
    /// written in place: the size cannot tell); anything else — truncated, another file renamed
    /// over the path, a shrink met while scanning — [`Now::Changed`]. The viewer reads a grown
    /// file through its index (bounded reads, [`Stale`] the net) and indexes it again soon; a
    /// changed one is never read through it.
    pub fn now(&self) -> Now {
        let Backing::Local(file) = &self.backing else {
            // A stream's changes are its Source's to say (06 §5), not a stat's.
            return Now::Same;
        };
        let named = match std::fs::metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Now::Gone,
            Err(_) => return Now::Changed,
        };
        let Ok(handle) = file.metadata() else {
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
        // A pass a preview's budget stopped read the file's first bytes only: still every byte
        // it indexed (a pass cut short by a shrink is not one of these).
        let whole = self.size == len || self.rest.is_some();
        if whole && grown(&handle) && grown(&named) {
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
        let backing = match &self.backing {
            Backing::Local(file) => {
                Backing::Local(file.try_clone().context("cannot open the file again")?)
            }
            Backing::Stream(stream) => {
                let source = stream.source.as_ref().context("the stream is closed")?;
                Backing::Stream(Box::new(Stream {
                    source: None,
                    reader: Box::new(source.reader(ReaderKind::Bulk)),
                    pass: None,
                    tail: None,
                    total: stream.total,
                    scan: Scan::default(),
                    blocking: None,
                }))
            }
        };
        Ok(TextDoc {
            backing,
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
            rest: self.rest.clone(),
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
        let bytes: &mut dyn Bytes = match &mut self.backing {
            Backing::Local(file) => file,
            Backing::Stream(stream) => {
                if let Some(cancel) = &stream.blocking {
                    let n = stream.reader.read_at(at, buf, cancel)?;
                    self.page_reads += n as u64;
                    return Ok(n);
                }
                stream.reader.as_mut()
            }
        };
        let got = match bytes.try_read(at, buf.len()) {
            Poll::Ready(got) => got?,
            Poll::Pending => return Err(anyhow::Error::new(Pending)),
        };
        buf[..got.len()].copy_from_slice(&got);
        self.page_reads += got.len() as u64;
        Ok(got.len())
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
                    // (Through a stream's growing index this byte is one the pass has read:
                    // every offset the index names lies in the bytes already fetched.)
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

/// `Err(Cancelled)` once `cancel` is set.
fn stopped(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(anyhow::Error::new(crate::remote::Cancelled))
    } else {
        Ok(())
    }
}

/// An index pass: where it is in the file and what it found so far — every mark (also one at
/// where it stopped), the line it is in and how long that line is so far, the first long line,
/// the last byte read.
#[derive(Debug, Clone, Default)]
struct Scan {
    at: u64,
    line: u64,
    line_len: u64,
    first_long: Option<u64>,
    last_byte: Option<u8>,
    marks: Vec<Mark>,
    /// The pass stopped at its cap, short of the file as opened.
    capped: bool,
    /// The hash of every byte read, for a pass that may stop short (a preview's): what its
    /// resume checks the bytes before the cut against.
    hasher: Option<sha2::Sha256>,
}

/// Bytes an index pass reads at a time (the cancel and the cap are looked at between reads).
const PASS_READ: usize = 1 << 20;

impl Scan {
    /// The next bytes of the file (`bytes`, at where the pass is), of a file of `size` bytes.
    fn feed(&mut self, bytes: &[u8], size: u64) {
        for (i, &b) in bytes.iter().enumerate() {
            if b != b'\n' {
                self.line_len += 1;
                continue;
            }
            if self.line_len > LINE_MAX as u64 && self.first_long.is_none() {
                self.first_long = Some(self.line);
            }
            self.line_len = 0;
            self.line += 1;
            let start = self.at + i as u64 + 1;
            let last = self.marks[self.marks.len() - 1];
            if start < size && (self.line - last.line >= BLOCK_LINES || start - last.offset >= SPAN)
            {
                self.marks.push(Mark {
                    line: self.line,
                    offset: start,
                });
            }
        }
        if let Some(&b) = bytes.last() {
            self.last_byte = Some(b);
        }
        self.at += bytes.len() as u64;
    }

    /// The pass over `file` (opened at `path`, `size` bytes as opened) from where it is, to the
    /// end of those bytes or to `cap()` (asked before every read); `check()` after every read.
    /// Stops early, not capped, where the file ends sooner (it shrank meanwhile: the change is
    /// noticed later).
    fn run(
        &mut self,
        file: &File,
        path: &Path,
        size: u64,
        cap: &dyn Fn() -> Option<u64>,
        check: &dyn Fn() -> Result<()>,
        on_chunk: &mut dyn FnMut(u64),
    ) -> Result<()> {
        if self.marks.is_empty() {
            self.marks.push(Mark { line: 0, offset: 0 });
        }
        let mut buf = vec![0u8; PASS_READ];
        self.capped = false;
        loop {
            // Only the bytes as opened: a file growing meanwhile is noticed later.
            let end = match cap() {
                Some(cap) if cap < size => {
                    if self.at >= cap {
                        self.capped = true;
                        return Ok(());
                    }
                    cap
                }
                _ => size,
            };
            let want = (end.saturating_sub(self.at) as usize).min(buf.len());
            if want == 0 {
                return Ok(());
            }
            let n = file
                .read_at(&mut buf[..want], self.at)
                .with_context(|| format!("cannot read {}", path.display()))?;
            if n == 0 {
                return Ok(());
            }
            #[cfg(test)]
            log_index_read(path, self.at, n as u64);
            if let Some(hasher) = self.hasher.as_mut() {
                sha2::Digest::update(hasher, &buf[..n]);
            }
            self.feed(&buf[..n], size);
            on_chunk(self.at);
            check()?;
        }
    }
}

#[cfg(test)]
static INDEX_READS: std::sync::Mutex<Vec<(std::path::PathBuf, u64, u64)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn log_index_read(path: &Path, at: u64, n: u64) {
    INDEX_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((path.to_path_buf(), at, n));
}

#[cfg(test)]
static VERIFY_READS: std::sync::Mutex<Vec<(std::path::PathBuf, u64, u64)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn log_verify_read(path: &Path, at: u64, n: u64) {
    VERIFY_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push((path.to_path_buf(), at, n));
}

/// Tests: every read a resume made of `path` to check the bytes before the cut — not index
/// reads — as `(offset, bytes)`, in order.
#[cfg(test)]
pub(crate) fn verify_reads(path: &Path) -> Vec<(u64, u64)> {
    VERIFY_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(p, _, _)| p == path)
        .map(|&(_, at, n)| (at, n))
        .collect()
}

/// Tests: every read an index pass made of `path` (its first pass, a resume, a re-read), as
/// `(offset, bytes)`, in order.
#[cfg(test)]
pub(crate) fn index_reads(path: &Path) -> Vec<(u64, u64)> {
    INDEX_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(p, _, _)| p == path)
        .map(|&(_, at, n)| (at, n))
        .collect()
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
            // Into a buffer of its own: a read that fails, or is not here yet, leaves the
            // window as it was — never a buffer made ready for bytes that did not come.
            let mut bytes = vec![0; want.clamp(1, CHUNK)];
            let n = doc.read_at(&mut bytes, pos)?;
            bytes.truncate(n);
            self.bytes = bytes;
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

/// The syntax a file's name picks: by extension, or by the whole name (`Makefile`). `None`
/// when the name does not decide.
fn syntax_by_name(name: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    crate::kind::extension(name)
        .and_then(|ext| set.find_syntax_by_extension(&ext))
        .or_else(|| set.find_syntax_by_extension(name))
}

/// The syntax for a file named `name`: by its name ([`syntax_by_name`]), and only when the name
/// does not decide, by the first line (`#!/bin/sh`) read through `doc`. `Ok(None)` for plain
/// text. An error when that line could not be read — it is [`Pending`], or the read failed:
/// the choice is still open, and the caller asks again (never plain text for good).
fn syntax_of(name: &str, doc: &mut TextDoc) -> Result<Option<&'static SyntaxReference>> {
    let found = match syntax_by_name(name) {
        Some(found) => found,
        None => {
            let first = doc.read_lines(0, 1)?.into_iter().next();
            let first = first.map(|l| l.text).unwrap_or_default();
            match syntaxes().find_syntax_by_first_line(&first) {
                Some(found) => found,
                None => return Ok(None),
            }
        }
    };
    Ok((found.name != "Plain Text").then_some(found))
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

/// What the window sends its worker.
enum Msg {
    Page(Ask),
    /// A stream's index grew: the worker's copy takes the same, in order.
    Grown(Growth),
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
    /// A stream's read gave no bytes (it failed, changed or was reloaded, 06 §5): the worker
    /// stopped, and highlighting starts again once the stream reads again.
    Stream,
}

/// The highlighting worker, from the window's side. Dropping it tells the worker to stop; it is
/// never joined (a syntect call in progress finishes on its own, its result unused).
struct Worker {
    asks: Sender<Msg>,
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
        let (asks, rx) = mpsc::channel::<Msg>();
        let (tx, done) = mpsc::channel();
        let latest = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        // A stream's worker waits for its bytes, and no longer than its window lives.
        doc.block_until(stop.clone());
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
                while let Ok(first) = rx.recv() {
                    // Only the newest request matters; what the index grew by, all of it.
                    let (mut ask, mut next) = (None, Some(first));
                    while let Some(msg) = next {
                        match msg {
                            Msg::Page(newer) => ask = Some(newer),
                            Msg::Grown(growth) => {
                                doc.apply(&growth);
                                hl.stop = doc.first_long.unwrap_or(u64::MAX);
                            }
                        }
                        next = rx.try_recv().ok();
                    }
                    if st.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(ask) = ask else {
                        continue;
                    };
                    let gen = ask.gen;
                    let dropped = || st.load(Ordering::Relaxed) || l.load(Ordering::Relaxed) != gen;
                    match hl.page(&mut doc, ask.top, ask.count, &dropped) {
                        Ok(Some(runs)) => {
                            end.send(Done::Page { gen, runs });
                            ctx.request_repaint();
                        }
                        Ok(None) => {}
                        // The window went while a read waited: nobody to tell.
                        Err(e) if matches!(read_error(&e), Some(ReadError::Cancelled)) => break,
                        Err(e) => {
                            crate::debug!("highlighting stopped: {e:#}");
                            end.send(if is_stale(&e) {
                                Done::Stale
                            } else if read_error(&e).is_some() {
                                Done::Stream
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
        let _ = self.asks.send(Msg::Page(Ask { gen, top, count }));
    }

    /// The index grew: the worker's copy grows the same before its next page.
    fn grown(&self, growth: Growth) {
        let _ = self.asks.send(Msg::Grown(growth));
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
        self.is_empty()
    }

    /// No row on it.
    fn is_empty(&self) -> bool {
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
    /// Line `n` at the top (a stream indexed again after a reload: the place it was at).
    Line(u64),
}

/// A preview of a remote file indexes this much of it (02 §4.3's budget for text); the
/// hand-over goes on from there (06 §7.2).
pub const PREVIEW_INDEX: u64 = 4 << 20;

/// The window's title in its frame, as PC Shell names its text viewer.
const TITLE: &str = "Text Viewer";

/// What stands where bytes are still on their way (06 §7.2).
const LOADING: &str = "loading…";

/// A read on its way for this long says so (06 §5; the 1 s `loading 0x…` form is the hex
/// window's, which has an offset to name).
const SLOW_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// What the status row adds then.
const SLOW: &str = "the remote is slow";

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
    /// A preview in the file browser (02 §4.2): drawn without bars in a single-lined frame, no
    /// input of its own, no following and no re-read. Its index may stop short of the file (the
    /// preview's 4 MiB); the hand-over ([`Window::set_preview`] `false`) resumes it.
    preview: bool,
    /// A page read is on its way (06 §7.2): the page on screen is the one before it, and where
    /// the keys asked to go is kept until its bytes arrive. Also before a stream's first block,
    /// and while a place past the indexed lines waits for the index pass.
    pending: bool,
    /// Since when (06 §5: after [`SLOW_AFTER`] the status row says the remote is slow).
    pending_since: Option<std::time::Instant>,
    /// Why a stream's last read failed (06 §5): the screen and the keys stay, the status row
    /// says it, and the next pass reads again.
    failure: Option<String>,
    /// A stream's state, looked at once a pass (06 §5).
    streamed: Option<crate::stream::State>,
    /// A stream's reload was asked for (Cmd/Ctrl+R), or met: it is indexed again from its
    /// first block once the Source has re-opened it.
    reloading: bool,
    /// The first line picks the syntax and has not been read yet (a stream's first block is on
    /// its way; a read failed).
    syntax_due: bool,
    /// A stream's gutter width in digits, sized once when its first block arrived — and again
    /// for the first block of a reload (`true`; the page of before keeps its width until then).
    gutter: Option<usize>,
    regutter: bool,
    /// A stream's first screen was drawn (`first`, 06 §11): its index pass goes on from here.
    first: bool,
    /// A stream's highlighter ended on a failed read: it starts again when the stream is back.
    hl_retry: bool,
    /// A highlighter started with a page already on screen: that page is asked of it.
    reask: bool,
    /// This pass has a key, a click or a scroll in it (06 §5: what retries a failed stream).
    input: bool,
    /// Called as a re-read's worker starts (tests: hold it).
    #[cfg(test)]
    reindex_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Called after every chunk of a re-read's pass (tests: change the file during it).
    #[cfg(test)]
    reindex_chunk_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Called right after a re-read's index is installed (tests: write to the file there).
    #[cfg(test)]
    after_install: Option<Box<dyn FnMut()>>,
    /// Called in a pass after its keys and before the stream is looked at (tests: what the
    /// stream's own thread may do in between, as a reload that ends there).
    #[cfg(test)]
    before_poll: Option<Box<dyn FnMut()>>,
}

/// A re-read of the file from its path, on a worker. Dropping it (the viewer gone) cancels it;
/// it is never waited for.
struct Reindex {
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<TextDoc>>,
    /// The viewer was at the end when the change was seen (it follows the new end).
    at_end: bool,
    /// The rest of an index a preview stopped short (the hand-over), not a re-read: the bytes
    /// indexed so far and the size as opened, for the status row.
    resume: Option<(Arc<AtomicU64>, u64)>,
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
        // The first line refines the choice only through an index that may be read — and for a
        // stream, once its first block is here: asked again then, and after a read that failed.
        let readable = matches!(now, Now::Same | Now::Grown) && !doc.is_stream();
        let (syntax, syntax_due) = match readable.then(|| syntax_of(name, &mut doc)) {
            Some(Ok(syntax)) => (syntax, false),
            Some(Err(_)) => (None, true),
            None => {
                let by_name = syntax_by_name(name).filter(|s| s.name != "Plain Text");
                (by_name, doc.is_stream() && syntax_by_name(name).is_none())
            }
        };
        let index_took = doc.indexed_in;
        let status = match now {
            Now::Gone => Status::error("the file was deleted"),
            _ => opening_status(syntax, &doc),
        };
        Self {
            live: !doc.is_stream(),
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
            preview: false,
            #[cfg(test)]
            reindex_hook: None,
            #[cfg(test)]
            reindex_chunk_hook: None,
            #[cfg(test)]
            after_install: None,
            #[cfg(test)]
            before_poll: None,
            pending: false,
            pending_since: None,
            failure: None,
            streamed: None,
            reloading: false,
            syntax_due,
            gutter: None,
            regutter: false,
            first: false,
            hl_retry: false,
            reask: false,
            input: false,
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

    /// How far a stream's index pass goes: a preview's budget, else 16 MiB.
    fn pass_cap(&self) -> u64 {
        if self.preview {
            PREVIEW_INDEX
        } else {
            HIGHLIGHT_MAX
        }
    }

    /// A stream's index pass has more to index (it runs on the frames).
    fn growing(&self) -> bool {
        self.doc.growing(self.pass_cap())
    }

    /// A stream's state, once a pass; and a reload that has re-opened the file: it is indexed
    /// again from its first block, the page on screen staying up until the new one is read.
    fn poll_stream(&mut self, ctx: &egui::Context) {
        self.streamed = self.doc.source().map(Source::state);
        // Asked for (Cmd/Ctrl+R), or met by a read; not done yet: the Source's waker brings
        // the pass that sees it.
        if !self.reloading || !self.doc.reloaded() {
            return;
        }
        let Some(source) = self.doc.source().cloned() else {
            return;
        };
        self.reloading = false;
        // The place it was at, kept until the new index has reached it.
        if let Some(page) = self.page.as_ref().filter(|p| p.top > 0 || p.top_off > 0) {
            self.jump = Some(if page.wrapped {
                Jump::Byte(page.top_start + page.top_off)
            } else {
                Jump::Line(page.top)
            });
        }
        self.doc = TextDoc::streamed(source);
        self.syntax_due = syntax_by_name(&self.name).is_none();
        if self.syntax_due {
            self.syntax = None;
        }
        self.worker = None;
        self.hl_tried = false;
        self.hl_retry = false;
        self.memo.reset(self.room);
        self.regutter = true;
        self.stale_page = true;
        self.failure = None;
        self.status = Status::default();
        self.streamed = self.doc.source().map(Source::state);
        ctx.request_repaint();
    }

    /// The first line picks the syntax, once it can be read (06 §7.2: in the pass that shows
    /// the first screen; a read that failed is asked again, not taken for plain text).
    fn pick_syntax(&mut self) {
        if !self.syntax_due || self.frozen() || self.doc.line_count() == 0 {
            return;
        }
        if let Ok(syntax) = syntax_of(&self.name, &mut self.doc) {
            self.syntax = syntax;
            self.syntax_due = false;
            // The highlighter starts on it (none ran: there was no syntax).
            self.hl_tried = false;
            if self.status.text.is_empty() {
                self.status = opening_status(self.syntax, &self.doc);
            }
        }
    }

    /// One step of a stream's index pass (06 §7.2), on the frame: the first block when it
    /// arrives, then — once the first screen is up — what is here of the next megabyte. The
    /// page on screen is read again only if it shows the line the index ended in.
    fn grow_index(&mut self, ctx: &egui::Context) {
        if !self.doc.is_stream() || self.frozen() {
            return;
        }
        if self.stream_down() && !self.input {
            // A failed stream is asked again by a key, a click or a scroll, not by a pass.
            return;
        }
        if self.doc.size() > 0 && !self.first && self.jump.is_none() {
            // The first screen first (unless it is to be a place the index has to reach).
            return;
        }
        let long = self.doc.long_lines();
        let growth = match self.doc.grow(self.pass_cap(), PASS_READ as u64) {
            Ok(Some(growth)) => growth,
            Ok(None) => return,
            // On its way: the Source's waker brings the next pass.
            Err(e) if is_pending(&e) => return,
            Err(e) => return self.read_failed(&e),
        };
        self.failure = None;
        if growth.open_line.is_some() {
            // The line the index ended in may go on now: what wrapping found is of the index
            // before. (The page on screen is not read again for that, below.)
            self.memo.reset(self.room);
        }
        // The page is read again only if a line on it got longer — the line the index ended
        // in, with no line end yet — or it was short of the window and may gain rows.
        let grew = self.page.as_ref().is_some_and(|p| {
            if p.wrapped {
                // (`at_end`: its last row ended the last line indexed.)
                p.rows.len() < p.visible || (p.at_end && growth.open_line.is_some())
            } else {
                let shown = p.top..p.top + p.runs.len() as u64;
                p.runs.len() < p.visible
                    || growth.open_line.is_some_and(|line| shown.contains(&line))
            }
        });
        if grew {
            self.stale_page = true;
        }
        if !long && self.doc.long_lines() && !self.status.error {
            self.status = opening_status(self.syntax, &self.doc);
        }
        if let Some(worker) = &self.worker {
            worker.grown(growth);
        }
        // More may be here already.
        ctx.request_repaint();
    }

    /// The gutter's width in digits. A local file's line count is known before its first
    /// screen. A stream's grows with its index, so its gutter is sized once, when its first
    /// block arrives (06 §7.2): by its line count when that block is the whole file, else by
    /// the digits of min(size, 16 MiB), at most 8 — an index never holds more lines than
    /// bytes. A width that followed the count moved the text and wrapped its rows again.
    fn digits(&mut self, lines: u64) -> usize {
        let digits = |n: u64| n.max(1).to_string().len();
        if !self.doc.is_stream() {
            return digits(lines);
        }
        if (self.gutter.is_none() || self.regutter) && lines > 0 {
            self.regutter = false;
            self.gutter = Some(if self.doc.whole() {
                digits(lines)
            } else {
                digits(self.doc.total().min(HIGHLIGHT_MAX)).min(8)
            });
        }
        self.gutter.unwrap_or(1)
    }

    /// A stream's read failed (06 §5): the screen and the keys stay, and the status row says
    /// why. A reader left behind by a reload starts the index again instead.
    fn read_failed(&mut self, e: &anyhow::Error) {
        match read_error(e) {
            Some(ReadError::Reloaded) => self.reloading = true,
            // The Source's state says these (frozen from the next pass on).
            Some(ReadError::Changed | ReadError::Closed) => {}
            _ => self.failure = Some(format!("{e:#}")),
        }
    }

    /// Nothing is read now: the file differs from its index (a re-read is due or under way), or
    /// it was deleted.
    fn frozen(&self) -> bool {
        self.waiting || self.reindex.is_some() || self.gone || self.stream_frozen()
    }

    /// A stream that failed (06 §5): what is cached still reads, and the next key, click or
    /// scroll retries.
    fn stream_down(&self) -> bool {
        matches!(self.streamed, Some(crate::stream::State::Failed(_)))
    }

    /// A stream that changed on the remote, or is being reloaded: the page stays and nothing
    /// is read until it has been re-opened (06 §5).
    fn stream_frozen(&self) -> bool {
        self.reloading
            || matches!(
                self.streamed,
                Some(crate::stream::State::Changed | crate::stream::State::Closed)
            )
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
        if self.reindex.is_some() || self.doc.is_stream() {
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
            resume: None,
            exited,
        });
    }

    /// The hand-over of a preview whose index stopped short: the rest of the file is indexed on
    /// a worker, from where the preview's pass stopped ([`TextDoc::resume`]: nothing indexed twice),
    /// and swapped in as a re-read is; meanwhile the page stays and the status row counts.
    fn start_resume(&mut self, ctx: &egui::Context) {
        if !self.doc.cut_short() || self.reindex.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicU64::new(self.doc.size()));
        let total = self.doc.opened.2;
        let doc = match self.doc.share() {
            Ok(doc) => doc,
            Err(e) => {
                // Read again from the path, as a change would be, after the usual wait.
                self.status = Status::error(format!("{e:#}"));
                self.waiting = true;
                return;
            }
        };
        let (stop, ex, waker, counted) =
            (cancel.clone(), exited.clone(), ctx.clone(), done.clone());
        std::thread::spawn(move || {
            let result = doc.resume_counted(&stop, &mut |at| {
                counted.store(at, Ordering::Relaxed);
                waker.request_repaint();
            });
            let _ = tx.send(result);
            ex.store(true, Ordering::Relaxed);
            waker.request_repaint();
        });
        // The highlighter reads through the short index; it starts again on the whole one.
        self.worker = None;
        self.reindex = Some(Reindex {
            cancel,
            rx,
            at_end: false,
            resume: Some((done, total)),
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
        let resumed = reindex.resume.is_some();
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
                // The first line may have changed (another shebang): the grammar follows it
                // (asked again on the next pass if it could not be read now).
                let picked = syntax_of(&self.name, &mut self.doc);
                self.syntax_due = picked.is_err();
                self.syntax = picked.unwrap_or(None);
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
                self.status = if resumed {
                    opening_status(self.syntax, &self.doc)
                } else {
                    Status::info(if grew && at_end {
                        "the file grew; following the end"
                    } else {
                        "the file changed; re-read"
                    })
                };
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
            Err(e) if resumed && is_stale(&e) => {
                // Written over since the preview read it: read again whole, after the usual wait.
                self.waiting = true;
                self.status = Status::info("the file changed; re-reading it in a moment…");
            }
            Err(e) => {
                // Tried again after the usual wait — a failed resume too, as a whole re-read.
                self.waiting |= resumed;
                self.index_took = std::time::Duration::ZERO;
                self.status = Status::error(format!("cannot read the file again: {e:#}"));
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if self.preview {
            return;
        }
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        // Cmd on macOS (Ctrl elsewhere), or Ctrl.
        let command = |key: Key| {
            ctx.input_mut(|i| {
                i.consume_key(Modifiers::COMMAND, key) || i.consume_key(Modifiers::CTRL, key)
            })
        };
        if let Some(source) = self.doc.source() {
            // Cmd/Ctrl+R (06 §5): the Source re-opens the path — not waited for on a frame;
            // its waker brings the pass that indexes the file again.
            if command(Key::R) && !self.reloading {
                source.request_reload();
                self.reloading = true;
                // This pass's input is the reload: it retries nothing besides (below).
                self.input = false;
            }
        }
        if self.waiting || self.reindex.is_some() || self.stream_frozen() {
            // Scrolling waits for the new index (the page on screen stays meanwhile).
            return;
        }
        if (pressed(Key::W) | super::hex::bare_key(ctx, Key::F2)) && !self.gone {
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
        let page = page as i64;
        for (key, lines) in [
            (Key::ArrowDown, 1),
            (Key::ArrowUp, -1),
            (Key::PageDown, page),
            (Key::PageUp, -page),
        ] {
            if !pressed(key) {
                continue;
            }
            let no_page_yet =
                self.doc.is_stream() && self.doc.line_count() == 0 && self.doc.total() > 0;
            if self.jump.is_some() || no_page_yet {
                // A place asked for and not shown yet (a stream's index has to reach it), or
                // no page at all yet (its first block is on its way): these lines are
                // counted from there, once it is.
                self.moved += lines;
            } else if lines < 0 {
                self.top = self.top.saturating_sub(lines.unsigned_abs());
            } else {
                self.top = self.top.saturating_add(lines as u64);
            }
        }
        if pressed(Key::Home) | file_start {
            self.top = 0;
            (self.jump, self.moved) = (None, 0);
        }
        if pressed(Key::End) | file_end {
            if self.growing() {
                // The end is not indexed yet: kept until the pass has reached it.
                (self.jump, self.moved) = (Some(Jump::FileEnd), 0);
            } else {
                // Past the end: the body puts the last page on screen with this pass's row
                // count.
                self.top = self.doc.line_count();
            }
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
        if self.waiting && self.reindex.is_none() && !self.preview {
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
        if self.doc.is_stream() && self.doc.line_count() == 0 && self.doc.total() > 0 {
            // A stream's first block is on its way, or failed (the status row says why): no
            // page yet — never an empty one.
            self.pending = self.failure.is_none() && !self.stream_down();
            return;
        }
        if !same && self.stream_down() && !self.input {
            // A failed stream (06 §5): the screen stays, and the next key, click or scroll
            // reads again — which asks the Source to retry, at most once a second. No pass
            // retries by itself.
            self.pending = false;
            self.worker_results();
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
                Err(e) if is_pending(&e) => {
                    // On its way (06 §7.2): the page on screen stays, and where the keys asked
                    // to go is kept for the pass the Source's waker brings. Never the error
                    // arm below, which blanks the page and drops the keys.
                    self.pending = true;
                    self.worker_results();
                    return;
                }
                Err(e) if self.doc.is_stream() => {
                    // A stream's read failed (06 §5), or its bytes are not what was indexed:
                    // the screen and the keys stay, the status row says why, and the next
                    // pass — a key, or the stream coming back — reads again.
                    self.pending = false;
                    if is_stale(&e) {
                        self.failure = Some(format!("{Stale} — Cmd/Ctrl+R reloads"));
                    } else {
                        self.read_failed(&e);
                    }
                    self.worker_results();
                    return;
                }
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
            (self.pending, self.failure, self.reask) = (false, None, false);
            self.gen += 1;
            page.gen = self.gen;
            page.highlighted = self.worker.is_none();
            if let Some(worker) = &self.worker {
                worker.ask(self.gen, page.top, page.count);
            }
            self.page = Some(page);
            // The status row above was drawn before this page was read: once more for it.
            ctx.request_repaint();
        } else {
            self.pending = false;
            if std::mem::take(&mut self.reask) {
                // A highlighter that started after this page was read.
                if let (Some(worker), Some(page)) = (&self.worker, self.page.as_mut()) {
                    self.gen += 1;
                    page.gen = self.gen;
                    page.highlighted = false;
                    worker.ask(self.gen, page.top, page.count);
                }
            }
        }
        self.worker_results();
        if (self.waiting || due) && self.reindex.is_none() && !self.preview {
            // A change (also one the highlighter found): the re-read starts, or is scheduled,
            // in this frame, not at the next input.
            self.start_reindex(ctx);
        }
    }

    /// The page of `count` lines at `top`, one row each, cut at [`LINE_MAX`] (not wrapped).
    fn read_unwrapped(&mut self, count: usize, plain: Color32) -> Result<Page> {
        let lines = self.doc.line_count();
        let last_top = lines.saturating_sub(self.visible);
        // A place past what a stream has indexed so far waits for the pass to reach it.
        let growing = self.growing();
        let top = match self.jump {
            Some(Jump::Byte(at)) if growing && at >= self.doc.size() => {
                return Err(anyhow::Error::new(Pending));
            }
            Some(Jump::Byte(at)) => match self.doc.span_at(at)? {
                Some(span) => span.line.min(last_top),
                None => self.top,
            },
            Some(Jump::FileEnd) if growing => return Err(anyhow::Error::new(Pending)),
            Some(Jump::FileEnd) => last_top,
            Some(Jump::Line(line)) if growing && line > last_top => {
                return Err(anyhow::Error::new(Pending));
            }
            Some(Jump::Line(line)) => line.min(last_top),
            _ => self.top,
        };
        // The lines the keys moved by since that place was asked for.
        let top = if self.moved < 0 {
            top.saturating_sub(self.moved.unsigned_abs())
        } else {
            top.saturating_add(self.moved as u64).min(last_top)
        };
        let read = self.doc.read_lines(top, count)?;
        // Only now: a read that is on its way keeps where the keys asked to go.
        (self.jump, self.moved, self.resnap) = (None, 0, false);
        self.top = top;
        let runs = read
            .iter()
            .map(|l| expand_tabs(vec![(plain, l.text.clone())]))
            .collect();
        Ok(Page::lines(top, count, runs, false, 0))
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
        // A place past what a stream has indexed so far waits for the pass to reach it.
        let growing = self.growing();
        let indexed = self.doc.size();
        let waits = || Err(anyhow::Error::new(Pending));
        if self.memo.rewidth(self.room) {
            self.resnap = true;
        }
        let cell = ctx.fonts_mut(|f| f.glyph_width(font, '0'));
        let mut w = Wrapper {
            doc: &mut self.doc,
            memo: &mut self.memo,
            m: Measure::new(ctx, font.clone(), plain),
            width: self.room,
            read: wrap::read_cap(self.room, cell),
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
                Some(Jump::FileEnd) if growing => return waits(),
                Some(Jump::FileEnd) => w.end(rows)?,
                Some(Jump::Home) if pos.off > 0 => Pos {
                    line: pos.line,
                    off: 0,
                },
                Some(Jump::Home) => Pos::default(),
                // The index's last line may go on, and the file's end is not known yet.
                Some(Jump::End) if growing && pos.line + 1 >= lines => return waits(),
                Some(Jump::End) => {
                    let line_end = w.last_of(pos.line)?;
                    let top = w.back(line_end, back)?;
                    if top > pos {
                        top
                    } else if growing {
                        return waits();
                    } else {
                        w.end(rows)?
                    }
                }
                Some(Jump::Byte(at)) if growing && at >= indexed => return waits(),
                Some(Jump::Byte(at)) => w.at_byte(at)?,
                Some(Jump::Line(line)) if growing && line >= lines => return waits(),
                Some(Jump::Line(line)) => Pos {
                    line: line.min(lines - 1),
                    off: 0,
                },
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
                Ok(Done::Stale) if self.doc.is_stream() => {
                    // A stream is not read again from a path: said, and Cmd/Ctrl+R reloads.
                    self.failure = Some(format!("{Stale} — Cmd/Ctrl+R reloads"));
                    gone = Some(None);
                    break;
                }
                Ok(Done::Stale) => {
                    // The file changed under the worker: read again from the path.
                    self.waiting = true;
                    gone = Some(None);
                    break;
                }
                Ok(Done::Stream) => {
                    // The stream failed under it (the Source's state is the status row's
                    // text, 06 §5): highlighting starts again when the stream is back. One
                    // Pending or failed read never ends it for good.
                    self.hl_retry = true;
                    if self.doc.reloaded() {
                        self.reloading = true;
                    }
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
        let syntax = self
            .syntax
            .map_or("plain text", |s| s.name.as_str())
            .to_string();
        let mut parts = if self.doc.is_stream() && !self.doc.whole() {
            // Not `N lines` while more may come: how far the lines are known, of the file's
            // size as the Source has it (06 §7.2).
            let known = format!(
                "lines known to {} of {}",
                crate::human(self.doc.size()),
                crate::human(self.doc.total())
            );
            vec![known, syntax]
        } else {
            vec![
                format!("{} lines", super::hex::grouped(self.doc.line_count())),
                crate::human(self.doc.size()),
                syntax,
            ]
        };
        if self.doc.long_lines() && !self.wrap {
            parts.push(format!("lines over {LINE_MAX} bytes are cut"));
        }
        ui.horizontal(|ui| {
            super::one_line(ui, &self.title, p.text);
            ui.separator();
            super::one_line(ui, &parts.join(" · "), p.dim);
        });
    }

    /// `loading…`, and after [`SLOW_AFTER`] of waiting that the remote is slow.
    fn loading(&self) -> String {
        let slow = self
            .pending_since
            .is_some_and(|since| (self.clock)().saturating_duration_since(since) >= SLOW_AFTER);
        if slow {
            format!("{LOADING} · {SLOW}")
        } else {
            LOADING.to_string()
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let lines = self.doc.line_count();
        // A stream describes the page on screen — never where the keys asked to go — and says
        // `of N` only once N is the file's (06 §7.2).
        let stream = self.doc.is_stream();
        let shown = match &self.page {
            Some(page) if stream && !page.is_empty() => {
                let (first, last) = match page.rows.last() {
                    Some(row) if page.wrapped => (page.top + 1, row.line + 1),
                    _ => (page.top + 1, page.top + page.runs.len() as u64),
                };
                let mut first_text = first.to_string();
                if page.wrapped && page.top_off > 0 && page.top_len > 0 {
                    first_text
                        .push_str(&format!(" ({} % of it)", page.top_off * 100 / page.top_len));
                }
                let mut shown = if last == first {
                    format!("line {first_text}")
                } else {
                    format!("lines {first_text}–{last}")
                };
                if self.doc.whole() {
                    shown.push_str(&format!(" of {lines}"));
                }
                if self.pending {
                    shown.push_str(" · ");
                    shown.push_str(&self.loading());
                }
                shown
            }
            // Before a stream's first block: not `empty`, for a file that is not.
            // Its first block failed: only the reason, beside this.
            _ if stream && lines == 0 && (self.failure.is_some() || self.stream_down()) => {
                String::new()
            }
            _ if stream && self.doc.total() > 0 && (lines == 0 || self.pending) => self.loading(),
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
        let status = match self.reindex.as_ref().and_then(|r| r.resume.as_ref()) {
            Some((done, total)) => Status::info(format!(
                "indexing the rest of the file… {} of {}",
                crate::human(done.load(Ordering::Relaxed)),
                crate::human(*total)
            )),
            // A stream's own state first (06 §5), then why its last read failed.
            None => match self.streamed.as_ref().and_then(|s| s.status()) {
                Some(text) => Status::error(text),
                None => match &self.failure {
                    Some(text) => Status::error(text.clone()),
                    None => self.status.clone(),
                },
            },
        };
        ui.horizontal(|ui| {
            let place = !shown.is_empty();
            if place {
                ui.monospace(shown);
            }
            if !status.text.is_empty() {
                if place {
                    ui.separator();
                }
                status.show(ui);
            }
        });
    }

    /// Where the scroll bar's thumb is (0 at the top, 1 at the end; `None`: nothing to scroll).
    /// Wrapped, by bytes: rows cannot be counted without reading every line.
    fn thumb(&self) -> Option<f32> {
        match &self.page {
            Some(p) if p.wrapped => {
                let at_start = p.top == 0 && p.top_off == 0;
                // The end of what is indexed is the file's end only once the index is whole.
                match (p.at_end && self.doc.whole(), at_start) {
                    (true, true) => None,
                    (true, false) => Some(1.0),
                    _ => Some((p.top_start + p.top_off) as f32 / self.doc.total().max(1) as f32),
                }
            }
            _ => {
                let max_top = self.doc.line_count().saturating_sub(self.visible);
                // A stream's thumb is where the page on screen is, of the file: the lines
                // known so far are this share of it.
                let (top, known) = match &self.page {
                    Some(p) if self.doc.is_stream() => (
                        p.top,
                        self.doc.size() as f32 / self.doc.total().max(1) as f32,
                    ),
                    _ => (self.top, 1.0),
                };
                (max_top > 0).then(|| top.min(max_top) as f32 / max_top as f32 * known)
            }
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // A re-read that landed is swapped in before anything reads or is measured (the
        // highlighter below starts on the new index).
        self.poll_reindex();
        #[cfg(test)]
        if let Some(hook) = self.before_poll.as_mut() {
            hook();
        }
        self.poll_stream(&ctx);
        if self.input && self.stream_down() {
            // A key, a click or a scroll retries a failed stream (06 §5) — also one whose page
            // is cached, which reads without asking. At most one attempt a second is made.
            // Not beside a reload (`input` is off for that pass: Cmd/Ctrl+R is its own
            // attempt, and a retry queued with it would follow a failed reload with no key).
            if let Some(source) = self.doc.source() {
                source.retry();
            }
        }
        let outer = ui.max_rect();
        let p = chrome::Palette::of(ui.visuals());
        // The window: its frame, titled as PC Shell's (single-lined in a preview: it does not
        // have the keyboard), the lines inside it.
        let framed = chrome::frame(&ui.painter_at(outer), outer, TITLE, !self.preview);
        let font = chrome::mono(ui);
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        // Rows touch, a cell apart, as on a text screen.
        let row_h = text_h.ceil();
        // A preview that shows only the start of its file says so on its last row (02 §4.3).
        // (A stream's pass stops at the preview's budget; a local file's was stopped there.)
        let short = self.doc.cut_short()
            || (self.doc.is_stream() && !self.doc.whole() && self.doc.size() >= PREVIEW_INDEX);
        let note = (self.preview && short && framed.height() >= 2.0 * row_h)
            .then(|| format!("preview of the first {}", amount(self.doc.size())));
        let rect = match note {
            Some(_) => Rect::from_min_max(
                framed.min,
                Pos2::new(framed.right(), framed.bottom() - row_h),
            ),
            None => framed,
        };
        if !self.preview && self.doc.cut_short() && !self.waiting && self.reindex.is_none() {
            // Handed over: the index goes on from where the preview's pass stopped.
            self.start_resume(&ctx);
        }
        self.grow_index(&ctx);
        self.pick_syntax();
        let lines = self.doc.line_count();
        self.visible = ((rect.height() / row_h).floor() as u64).max(1);
        let digits = self.digits(lines);
        let gutter = cw * (digits as f32 + 1.0);
        let text_x = rect.left() + gutter + cw;
        self.room = (rect.right() - text_x).max(0.0);
        // Scrolling waits for a re-read, as the keys do; a preview takes no wheel.
        if !self.preview
            && ui.rect_contains_pointer(outer)
            && !self.waiting
            && self.reindex.is_none()
            && !self.stream_frozen()
        {
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
        if self.hl_retry && !self.frozen() && self.failure.is_none() {
            // The stream is back (or was never down for what the page needs): once more.
            if matches!(
                self.streamed,
                Some(crate::stream::State::Live | crate::stream::State::Replaced)
            ) {
                (self.hl_retry, self.hl_tried) = (false, false);
            }
        }
        // A stream's highlighter starts once its first block is indexed and the first line
        // has had its say; it then runs over the indexed lines as they grow (06 §7.2).
        let indexed = !self.doc.is_stream() || (lines > 0 && !self.syntax_due);
        if !self.hl_tried && indexed {
            self.hl_tried = true;
            // (A stream's index never passes 16 MiB: its lines are highlighted as far as it
            // goes. A local file over that is plain, as it always was.)
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
                        // The page on screen, if one is, is asked of it.
                        self.reask = started.is_some();
                        started
                    }
                    _ => None,
                };
            }
        }
        self.ensure_page(&ctx, p.text, &font);
        if self.live && !self.preview && !self.frozen() && self.at_end() {
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
                    // As it was measured (one section).
                    let measured = || {
                        ctx.fonts_mut(|f| f.layout_no_wrap(row.cut.text(), font.clone(), p.text))
                    };
                    let rtl = row
                        .cut
                        .shown
                        .iter()
                        .any(|&(c, _)| direction(c) == Some(true));
                    let plain = || {
                        if rtl {
                            let job = directed_job(row_chars(row, None, p.text), &font);
                            ctx.fonts_mut(|f| f.layout_job(job))
                        } else {
                            measured()
                        }
                    };
                    let mut galley = match colors {
                        Some(colors) => {
                            let job = directed_job(row_chars(row, Some(colors), p.text), &font);
                            ctx.fonts_mut(|f| f.layout_job(job))
                        }
                        None => plain(),
                    };
                    // Sections are shaped apart and can come out a hair wider than the row was
                    // measured: plain then, and at worst as measured (never cut at the edge).
                    if galley.size().x > page.width + 0.5 && colors.is_some() {
                        galley = plain();
                    }
                    if galley.size().x > page.width + 0.5 && rtl {
                        galley = measured();
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
                    let mut shown: Vec<(char, Color32)> = Vec::new();
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
                        let part = text
                            .chars()
                            .skip(std::mem::take(&mut skip))
                            .take(room)
                            .map(|c| (chrome::shown_char(c), *color));
                        let before = shown.len();
                        shown.extend(part);
                        room -= shown.len() - before;
                    }
                    let galley = ctx.fonts_mut(|f| f.layout_job(directed_job(shown, &font)));
                    text_painter.galley(
                        Pos2::new(text_x, rect.top() + k as f32 * row_h),
                        galley,
                        p.text,
                    );
                }
                if self.growing() {
                    // The rows below the last line indexed so far (a line that runs past
                    // what is indexed): their starts are not known yet. They fill in as the
                    // index pass reaches them (06 §7.2).
                    for k in page.runs.len()..self.visible as usize {
                        text_painter.text(
                            Pos2::new(text_x, rect.top() + k as f32 * row_h),
                            egui::Align2::LEFT_TOP,
                            LOADING,
                            font.clone(),
                            p.dim,
                        );
                    }
                }
            }
            None => {}
        }
        if self.pending && self.page.as_ref().is_none_or(Page::is_empty) {
            // Nothing to show yet (a stream's first block is on its way): one dim row.
            painter.text(
                Pos2::new(rect.left() + cw, rect.top()),
                egui::Align2::LEFT_TOP,
                LOADING,
                font.clone(),
                p.dim,
            );
        }
        if self.pending {
            // How long this read has been on its way; a pass when the status row says more.
            let now = (self.clock)();
            let since = *self.pending_since.get_or_insert(now);
            let waited = now.saturating_duration_since(since);
            if waited < SLOW_AFTER {
                ctx.request_repaint_after(SLOW_AFTER - waited);
            }
        } else {
            self.pending_since = None;
        }
        if !self.first && self.page.as_ref().is_some_and(|p| !p.is_empty()) {
            // The first screen is drawn: `first` on the stats line (06 §11). The index pass
            // goes on from the next pass.
            self.first = true;
            if let Some(source) = self.doc.source() {
                source.mark(crate::stream::Mark::First);
                ctx.request_repaint();
            }
        }
        if let Some(note) = &note {
            ui.painter_at(framed).text(
                Pos2::new(rect.left(), rect.bottom()),
                egui::Align2::LEFT_TOP,
                note,
                font.clone(),
                p.dim,
            );
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

/// A wrapped row's characters as painted (a tab as its spaces), each in its colour: by
/// `colors`, the line's colour per character with the row's first at index `first`, else plain.
fn row_chars(
    row: &wrap::Row,
    colors: Option<(usize, &[Color32])>,
    plain: Color32,
) -> Vec<(char, Color32)> {
    let mut out = Vec::with_capacity(row.cut.shown.len());
    for (i, &(c, n)) in row.cut.shown.iter().enumerate() {
        let color = colors
            .and_then(|(first, colors)| colors.get(first + i).copied())
            .unwrap_or(plain);
        out.extend(std::iter::repeat_n((c, color), usize::from(n)));
    }
    out
}

/// Whether `c` is written right to left (`Some(true)`: Hebrew, Arabic and the other
/// right-to-left scripts' letters), left to right (`Some(false)`: any other letter or digit,
/// Arabic-Indic digits too), or neither (spaces, punctuation, marks).
fn direction(c: char) -> Option<bool> {
    match c {
        '\u{0660}'..='\u{0669}' | '\u{06F0}'..='\u{06F9}' | '\u{07C0}'..='\u{07C9}' => Some(false),
        '\u{200F}' | '\u{061C}' => Some(true),
        '\u{0590}'..='\u{08FF}'
        | '\u{FB1D}'..='\u{FDFF}'
        | '\u{FE70}'..='\u{FEFF}'
        | '\u{10800}'..='\u{10FFF}'
        | '\u{1E800}'..='\u{1EFFF}' => c.is_alphabetic().then_some(true),
        c if c.is_alphanumeric() => Some(false),
        _ => None,
    }
}

/// A row as painted, laid out left to right: a section per run of one colour and one direction.
/// egui shapes a section in the direction of its first strong character, so a row starting with a
/// right-to-left word (Hebrew is in the VGA font, shaped in one run with the English after it)
/// came out reversed whole, its English too. Here each run of right-to-left letters — with the
/// spaces and marks between them — is a section of its own (shaped right to left, as it reads),
/// and everything else is shaped left to right; the runs stay in their order, as in a
/// left-to-right paragraph. Sections never split a grapheme cluster (a cluster takes its base's
/// direction and colour). The text is the row's own: only how egui shapes it changes.
fn directed_job(chars: Vec<(char, Color32)>, font: &FontId) -> LayoutJob {
    use unicode_segmentation::UnicodeSegmentation;
    let text: String = chars.iter().map(|&(c, _)| c).collect();
    // By grapheme cluster, each with its base's direction and colour: a mark, a ZWJ or a
    // variation selector goes where its base goes, never into a section of its own (egui would
    // shape it apart from its base).
    let mut clusters: Vec<(&str, Option<bool>, Color32)> = Vec::new();
    let mut at = 0;
    for g in text.graphemes(true) {
        let (base, color) = chars[at];
        clusters.push((g, direction(base), color));
        at += g.chars().count();
    }
    // A neutral cluster goes right to left only between two right-to-left letters.
    let mut next_rtl = vec![false; clusters.len()];
    let mut next = None;
    for i in (0..clusters.len()).rev() {
        next_rtl[i] = next == Some(true);
        if clusters[i].1.is_some() {
            next = clusters[i].1;
        }
    }
    let mut job = LayoutJob::default();
    // A section of its own for each run, also next to one in the same colour: `append` would
    // join them, and egui would shape them as one again.
    let push = |job: &mut LayoutJob, run: &str, color: Color32| {
        use egui::text::ByteIndex;
        let start = job.text.len();
        job.text.push_str(run);
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(start)..ByteIndex(job.text.len()),
            format: egui::TextFormat::simple(font.clone(), color),
        });
    };
    let (mut run, mut key) = (String::new(), None);
    let mut prev = None;
    for (i, &(g, class, color)) in clusters.iter().enumerate() {
        let rtl = class.unwrap_or(prev == Some(true) && next_rtl[i]);
        if class.is_some() {
            prev = class;
        }
        if let Some((was, _)) = key.filter(|&k| k != (color, rtl)) {
            push(&mut job, &run, was);
            run.clear();
        }
        key = Some((color, rtl));
        run.push_str(g);
    }
    if let Some((color, _)) = key {
        push(&mut job, &run, color);
    }
    job
}

impl Window for TextWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        if self.preview {
            // Only the lines, in the frame: no bars, no notes, no keys (02 §4.2).
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.body(ui));
            return;
        }
        // Whether this pass's input may retry a failed stream is settled here, from how the
        // pass began: one that began with a reload under way, or whose input starts one
        // (`keys`), retries nothing — whenever in the pass that reload ends. One gesture is
        // one attempt.
        let reloading = self.reloading;
        self.input = !reloading
            && ui.ctx().input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { pressed: true, .. }
                            | egui::Event::PointerButton { pressed: true, .. }
                            | egui::Event::MouseWheel { .. }
                    )
                })
            });
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
        if self.pending {
            // A page read is on its way (06 §7.2): what was asked for is not on screen yet.
            return false;
        }
        // A stream whose first block failed has no page to wait for: the status row has the
        // reason (06 §11: ready once the screen's bytes have arrived or failed).
        let failed = self.page.is_none() && (self.failure.is_some() || self.stream_down());
        let page = failed
            || self
                .page
                .as_ref()
                .is_some_and(|p| p.highlighted || self.worker.is_none());
        if self.preview {
            // A preview whose file changed keeps what it shows: it is not re-read.
            return self.painted && (page || self.frozen());
        }
        self.painted && !self.waiting && self.reindex.is_none() && !self.reloading && page
    }

    /// A preview (02 §4.2): no bars, a single-lined frame, no keys or wheel, no following and no
    /// re-read. `false` (the hand-over) brings all of that back; an index the preview stopped
    /// short goes on from where it stopped on the next pass.
    fn set_preview(&mut self, on: bool) {
        self.preview = on;
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

/// The status a viewer opens with: why highlighting is off or stops early, if it is.
fn opening_status(syntax: Option<&SyntaxReference>, doc: &TextDoc) -> Status {
    match (syntax, doc.first_long) {
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
    }
}

/// `4 MiB` for a whole number of MiB (a preview's budget), else the size for people.
fn amount(bytes: u64) -> String {
    if bytes > 0 && bytes.is_multiple_of(1 << 20) {
        format!("{} MiB", bytes >> 20)
    } else {
        crate::human(bytes)
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

/// The text viewer on a remote file read through `source` (06 §7.2): the window opens at once,
/// shows the first block when it arrives and indexes on from there. `title`, `name`, `notices`
/// and `at` as [`embed`]'s; it is never followed at its end.
pub fn embed_source(
    source: Source,
    title: String,
    name: &str,
    notices: Vec<String>,
    at: Option<u64>,
) -> Embedded {
    let mut window = TextWindow::new(TextDoc::streamed(source), title, name, notices);
    window.jump = at.map(Jump::Byte);
    Embedded {
        window: Box::new(window),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

#[cfg(test)]
mod preview_tests;

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod tests;
