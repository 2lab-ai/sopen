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
//! The index's memory is bounded (06 §7.2): at [`MARKS_MAX`] marks, 16 MiB, the line spacing
//! doubles and the marks no longer due are dropped ([`Scan::thin`]) — never one whose going
//! would leave a line start a span past its mark — so it holds at most 16 MiB, or three marks
//! to each 64 KiB of the file where that is more (17.6 MB for 24 GB of any text). The document
//! holds the one copy of it; the highlighter's copy is of the first [`HIGHLIGHT_MAX`] only.
//!
//! The place on screen is a byte offset (06 §7.2): where the top line starts in the file, with
//! its line number where the index reaches it, and the byte of it the top row starts at.
//! Lines wrap by default ([`wrap`]): every line is shown whole, in rows broken by the measured
//! width of their glyphs, and a page reads only its rows' bytes — a line of hundreds of megabytes
//! too, from wherever the page is in it. W or F2 switch to one row per line, where the place
//! within a line is a byte of it: → and ←, Home, End and the wheel sideways move it, and a row
//! is read under the same bound as a wrapped one, so a line of any length can be scrolled to
//! any byte. Cmd/Ctrl+G opens Go to in the status row: a line number, or `0x…` for a byte
//! offset. The scroll bar is clicked or dragged: by lines when the lines are all known and not
//! wrapped, else by bytes.
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
//! whole). A file over [`HIGHLIGHT_MAX`] is highlighted over its first 16 MiB and plain past
//! them, read through a stream or in place (06 Tensions, settled by W5b). A run of more than
//! [`RUN_MAX`] word characters (a hex blob, a long name) reaches the parser shortened — the
//! grammars' patterns cost the square of a run's length — to pieces of itself that keep its
//! ends, its characters and their order, and is coloured as the whole run, but for the
//! differences known, which [`Shortened`] lists (most in a varied run: the left-out part where a
//! grammar colours inside a word; a back-reference between two long runs that shorten alike).
//! So does a chain of names longer than that (`a.b.c`, `a,b,c`, `a$b$c`, `a-b-c`), and in YAML
//! a stretch of a line without its `: # [ ] { } " ' \` (a long value): whole units of them from
//! each end, an even number left out between, the left-out part coloured as the character kept
//! before it — as it is, but where a grammar colours a chain's names apart. A Makefile is
//! parsed with its grammar's lookaheads for a rule and a variable rewritten to mean the same
//! without their cost, a power of a line's length ([`parsing`], [`balanced_once`]).
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
//! [`HIGHLIGHT_MAX`] (a preview: the budget its route gives it, 02 §4.3); the top bar says how far the lines are
//! known, and the gutter is sized once. A read whose
//! bytes are not here yet is [`Pending`]: it travels up as [`Stale`] does, in an arm of its own
//! — the page on screen stays, where the keys asked to go is kept, and the Source's waker
//! brings the pass that reads again. A read that failed keeps the screen and the keys too, with
//! the reason on the status row; the next key, click or scroll retries. The highlighting worker
//! reads through a reader of its own with the blocking read, so it waits for its bytes and
//! never sees `Pending`; it highlights over the indexed lines as they grow. A file that
//! changed on the remote freezes the page until Cmd/Ctrl+R, which re-opens it and indexes it
//! again from its first block.
//!
//! Past the lines known — a stream over 16 MiB (06 §7.2, W5b) — the file is read by byte
//! position. The index is a prefix that only grows from byte 0: by the pass, by what the
//! screen reads at its end ([`TextDoc::grow_from_screen`]), and — only when Go to asks for a
//! line past it — by the pass run on to that line, with progress, until Esc. A jump (End, Go
//! to an offset, the scroll bar, a place opened at) asks once for the blocks around its place
//! ([`TextDoc::bring`]: at most three, the file's end two) and looks back for the line start
//! from the 16 KiB grid point at or before it ([`TextDoc::piece_holding`]), within 64 KiB of
//! that cell's end. A line found so has its byte offset in the gutter in place of a number. A
//! line whose start is not in reach is shown in pieces that begin on the grid, `…` before
//! their offset, and the status row says `line start not known`; up and down then step a grid
//! point. No line is read or laid out whole. A local file keeps its whole-file pass, which
//! runs before its first screen: every line of it has its number (06 Tensions, settled by
//! W5b). The browser's route still opens a stream only for remote text of at most
//! [`HIGHLIGHT_MAX`] (06 §13 W8a); W8b routes the larger ones here.
//!
//! Lines are read in an encoding (08 §7, [`crate::encoding`]): the default the file's first
//! 8 KiB pick, in the pass that reads them (a local file's index pass, a stream's first
//! block), or one chosen for this opening (`SOPEN_STATE` `enc=`), kept by every pass made
//! again (D21). Every offset the document makes is a multiple of the encoding's unit; a line
//! ends at its LF unit at an aligned offset, never at a 0x0A byte inside another character, and
//! its CR is the CR unit before it. A BOM is skipped under its own encoding: line 1 starts after
//! it. A piece cut on the grid, a wrap restart, a jump and a place inside a line not wrapped
//! start where the encoding's rules say (08 §7.3); where no rule proves a character start
//! (CP949, D28), the bytes show as placeholder marks. Changing the encoding reads the file
//! again only when the LF unit changes (D22).

mod wrap;

use super::{chrome, Embedded, Outcome, Status, Window};
use crate::encoding::{Around, Encoding, Pick};
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
use syntect::parsing::syntax_definition::Pattern;
use syntect::parsing::{
    ParseState, Regex, ScopeStack, SyntaxReference, SyntaxSet, SyntaxSetBuilder,
};

/// A mark in the line index every this many lines…
const BLOCK_LINES: u64 = 256;
/// …or at the first line start this far past the last mark, whichever comes first.
const SPAN: u64 = 64 * 1024;
/// The longest line highlighting parses, and the most of a line [`TextDoc::read_lines`] gives.
/// (On screen a line is shown whole: wrapped in rows, or — not wrapped — from any byte of it.)
pub const LINE_MAX: usize = 4096;
/// Highlighting covers this much of a file, from its start (its state runs from the top of the
/// file); the rest is plain. Also how far a stream's index pass reads without being asked.
pub const HIGHLIGHT_MAX: u64 = 16 << 20;
/// The longest run of word characters, chain of names or YAML stretch the parser is handed as
/// it is. A longer one is shortened ([`Shortened`]) — to more than this, so it never reads as a
/// shorter one: a run to less than twice it, a chain or stretch to at most three units more.
const RUN_MAX: usize = 2 * RUN_END + RUN_MIDDLE;
/// Characters a shortened run keeps at each end: twice the longest counted repeat in the bundled
/// grammars that can match word characters (16, C++'s raw-string delimiter), so a prefix or
/// suffix (`0x`, `u64`, `_t`) is seen as it is.
const RUN_END: usize = 32;
/// A shortened run's middle is longer than this: room for every ASCII word character, so a test
/// over the whole run (all capitals, all hex digits) answers as it does on the run itself.
const RUN_MIDDLE: usize = 64;
/// A stretch of one repeated character in a shortened run's middle keeps its length modulo this
/// (and two characters at least), so runs that differ only in a stretch's length mostly differ.
const RUN_STRETCH: usize = 16;
/// Reads while looking for the end of a line.
const CHUNK: usize = 16 * 1024;
/// The most marks the index holds before it thins them (06 §7.2): 16 MiB of them.
const MARKS_MAX: usize = 1 << 20;
/// The widest the line spacing of marks gets. Lines that far apart are a span apart too, so
/// from there on every mark is a span past the one before: 16 bytes per 64 KiB.
const SPACING_MAX: u64 = SPAN;
/// The grid a line whose start is not known is cut on, and wrapping restarts on (06 §7.2).
const GRID: u64 = wrap::RESTART;
/// How far before a place's grid point its line's start is looked for (06 §7.2: at most a
/// span back — what a jump's one ask brings). A line that starts further back has no known
/// start there, and is shown in pieces from the grid.
const REACH: u64 = SPAN;
/// Columns a tab advances to.
const TAB: usize = 4;
/// Bytes of a line Left / Right scroll by (not wrapped).
const SIDEWAYS: u64 = 8;

/// Where line `line` starts: byte `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mark {
    line: u64,
    offset: u64,
}

/// One line as shown: its text (decoded in the document's encoding, what is not a character
/// shown as U+FFFD) and whether it was cut at [`LINE_MAX`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub cut: bool,
}

/// A text file opened for viewing: the handle it is read through, and the line index.
pub struct TextDoc {
    backing: Backing,
    /// The encoding lines are read in (08 §7): the default the first bytes picked, or the one
    /// chosen for this opening.
    enc: Encoding,
    /// The default (§7.2), once the first bytes were read: the bar's `(guessed)`.
    pick: Option<Pick>,
    /// The file's first bytes (four, fewer when it is shorter): which BOM is skipped.
    head: Vec<u8>,
    /// The encoding chosen for this opening (`enc=`), if one was: every pass made again —
    /// a re-read, a reload, a resume — reads in it (D21).
    chosen: Option<Encoding>,
    /// The bytes the index covers: the file's, or the prefix a stopped or growing pass read.
    size: u64,
    marks: Vec<Mark>,
    lines: u64,
    /// The last line ends with a newline.
    ends_newline: bool,
    /// Where the last line indexed starts (`size` when it ended with its newline).
    open_at: u64,
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
    /// The file as its owner knows it — `host:/abs` when `path` is a remote file's copy, else
    /// the path itself: how every message about a read of it names it, in the first pass and
    /// in any later one. The copy's path is never said.
    shown: String,
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
    /// How far the index pass goes now (the window's to say: 16 MiB, a preview's budget, or
    /// the whole file while Go to looks for a line).
    cap: u64,
    /// A jump's readers (06 §7.2): one for each block it asks for, each asking for one byte
    /// of its block — a demand with no margin beside it — so the jump is one ask.
    pins: Vec<Box<dyn Bytes + Send>>,
    /// What reads a jump's blocks once they are here: a Bulk reader out of sequence, which
    /// fetches nothing beside what it is asked.
    quiet: Option<Box<dyn Bytes + Send>>,
    /// The blocks the last jump brought, and where they start: its page is read from them.
    near: Option<(u64, bytes::Bytes)>,
    /// The last bytes the screen read at the end of the lines known: the index grows by them.
    seen: Option<(u64, bytes::Bytes)>,
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

    /// [`TextDoc::open_cancellable`] of a remote file's copy in sopen's cache: what fails is
    /// said about `shown`, the file as its owner knows it (`host:/abs`) — never about the
    /// copy's path.
    pub fn open_cancellable_as(path: &Path, shown: &str, cancel: &AtomicBool) -> Result<Self> {
        Self::open_scan(
            path,
            shown,
            None,
            &|| None,
            &|| stopped(cancel),
            &mut |_| {},
        )
    }

    /// [`TextDoc::open`], its lines read in `enc` whatever the first bytes would pick (08 §7.5:
    /// a choice that overrides any guess).
    pub fn open_encoded(path: &Path, enc: Encoding) -> Result<Self> {
        let shown = path.display().to_string();
        let cancel = AtomicBool::new(false);
        Self::open_scan(
            path,
            &shown,
            Some(enc),
            &|| None,
            &|| stopped(&cancel),
            &mut |_| {},
        )
    }

    /// [`TextDoc::open_cancellable`], with `on_chunk(bytes indexed so far)` after every chunk
    /// (tests: a cancel, or a writer, in the middle of the pass).
    fn open_with(path: &Path, cancel: &AtomicBool, on_chunk: &mut dyn FnMut(u64)) -> Result<Self> {
        Self::open_in(path, None, cancel, on_chunk)
    }

    /// [`TextDoc::open_with`] in the encoding `chosen` for this opening, if one was (D21).
    fn open_in(
        path: &Path,
        chosen: Option<Encoding>,
        cancel: &AtomicBool,
        on_chunk: &mut dyn FnMut(u64),
    ) -> Result<Self> {
        let shown = path.display().to_string();
        Self::open_scan(
            path,
            &shown,
            chosen,
            &|| None,
            &|| stopped(cancel),
            on_chunk,
        )
    }

    /// The same file indexed again from its first byte in `enc` (D22: the LF unit changed), on
    /// a handle of its own, named as this one is; `on_chunk(bytes indexed so far)` after every
    /// chunk, stopped when `cancel` is set. A stream has no local pass: it is
    /// [`TextDoc::streamed_as`] again.
    fn reencoded(
        &self,
        enc: Encoding,
        cancel: &AtomicBool,
        on_chunk: &mut dyn FnMut(u64),
    ) -> Result<TextDoc> {
        let mut doc = Self::open_scan(
            &self.path,
            &self.shown,
            Some(enc),
            &|| None,
            &|| stopped(cancel),
            on_chunk,
        )?;
        // The pass is a re-read: the next change waits as long after it as after the first.
        doc.indexed_in += self.indexed_in;
        Ok(doc)
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
        Self::open_capped_as(path, &path.display().to_string(), cap, check)
    }

    /// [`TextDoc::open_capped`] of a remote file's private copy: what fails — now, and in every
    /// later pass over the copy — is said about `shown`, the file as its owner knows it
    /// (`host:/abs`), never about the copy's path.
    pub fn open_capped_as(
        path: &Path,
        shown: &str,
        cap: &dyn Fn() -> Option<u64>,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        Self::open_scan(path, shown, None, cap, check, &mut |_| {})
    }

    /// The pass over `path`: its lines in `chosen`, or — `None` — in the default its first
    /// bytes pick (08 §7.2, in the pass itself: the marks depend on it).
    fn open_scan(
        path: &Path,
        shown: &str,
        chosen: Option<Encoding>,
        cap: &dyn Fn() -> Option<u64>,
        check: &dyn Fn() -> Result<()>,
        on_chunk: &mut dyn FnMut(u64),
    ) -> Result<Self> {
        check()?;
        let started = std::time::Instant::now();
        let file = File::open(path).with_context(|| format!("cannot open {shown}"))?;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {shown}"))?;
        let opened = identity(&meta);
        let mut scan = Scan::reading(chosen);
        if cap().is_some() {
            // A pass that may stop short hashes what it reads: its resume can then tell the
            // bytes it indexed are still the file's when the file was written to since.
            scan.hasher = Some(sha2::Sha256::default());
        }
        scan.run(&file, path, shown, opened.2, cap, check, on_chunk)?;
        let mut doc = Self::indexed(file, scan, opened, path.to_path_buf(), shown.to_string());
        doc.indexed_in = started.elapsed();
        // Checked here, right after the pass (on the opener's or re-read's worker), as well as
        // when the viewer takes the index: a change between the two is caught by one of them.
        doc.after_pass = doc.now();
        Ok(doc)
    }

    /// The index a pass found (`scan`), for the file as `opened`: of the bytes it read, and no
    /// mark at or past their end. A pass the cap stopped keeps what it found, to go on from.
    fn indexed(
        file: File,
        scan: Scan,
        opened: Identity,
        path: std::path::PathBuf,
        shown: String,
    ) -> Self {
        // The bytes actually indexed (fewer than the size as opened if it shrank meanwhile: the
        // change is noticed later).
        let mut doc = Self {
            backing: Backing::Local(file),
            enc: scan.enc,
            pick: None,
            head: Vec::new(),
            chosen: scan.chosen,
            size: 0,
            marks: Vec::new(),
            lines: 0,
            ends_newline: false,
            open_at: 0,
            first_long: None,
            opened,
            indexed_in: std::time::Duration::ZERO,
            after_pass: Now::Same,
            path,
            shown,
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
        let mut scan = scan;
        if scan.capped {
            // A preview's stopped pass keeps its marks to go on from (a few megabytes' worth).
            doc.marks.clone_from(&scan.marks);
        } else {
            // The one copy of the index (06 §7.2: its memory is bounded, and counted once).
            doc.marks = std::mem::take(&mut scan.marks);
        }
        doc.adopt(&scan);
        doc.rest = scan.capped.then_some(scan);
        doc
    }

    /// A remote file read through `source` (06 §7.2), nothing of it read yet: its index grows
    /// from byte 0 ([`TextDoc::grow`]) — the first block through the screen's reader, the rest
    /// through a Bulk reader of its own.
    pub fn streamed(source: Source) -> Self {
        Self::streamed_as(source, None)
    }

    /// [`TextDoc::streamed`], its lines read in `chosen` when one was chosen for this opening
    /// (D21), else in the default its first block picks (08 §7.2: in the pass that shows the
    /// first screen and picks the syntax).
    pub fn streamed_as(source: Source, chosen: Option<Encoding>) -> Self {
        let total = source.size();
        let scan = Scan {
            marks: vec![Mark { line: 0, offset: 0 }],
            ..Scan::reading(chosen)
        };
        let stream = Stream {
            reader: Box::new(source.reader(ReaderKind::Interactive)),
            pass: Some(Box::new(source.reader(ReaderKind::Bulk))),
            tail: None,
            source: Some(source),
            total,
            scan: Scan::reading(chosen),
            blocking: None,
            cap: HIGHLIGHT_MAX,
            pins: Vec::new(),
            quiet: None,
            near: None,
            seen: None,
        };
        let mut doc = Self {
            backing: Backing::Stream(Box::new(stream)),
            enc: scan.enc,
            pick: None,
            head: Vec::new(),
            chosen,
            size: 0,
            marks: Vec::new(),
            lines: 0,
            ends_newline: false,
            open_at: 0,
            first_long: None,
            opened: (0, 0, total, None),
            indexed_in: std::time::Duration::ZERO,
            after_pass: Now::Same,
            path: std::path::PathBuf::new(),
            shown: String::new(),
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
        // The index has one copy of its marks, the document's: the pass has them for its step.
        std::mem::swap(&mut stream.scan.marks, &mut self.marks);
        let stepped = Self::step(stream, cap, budget);
        std::mem::swap(&mut stream.scan.marks, &mut self.marks);
        if !stepped? {
            return Ok(None);
        }
        let scan = std::mem::take(&mut stream.scan);
        self.adopt(&scan);
        if let Backing::Stream(stream) = &mut self.backing {
            stream.scan = scan;
        }
        Ok(Some(self.growth(before, lines, ends_newline)))
    }

    /// What the index gained since it had `before` marks and `lines` lines.
    fn growth(&self, before: usize, lines: u64, ends_newline: bool) -> Growth {
        // (Thinned meanwhile: the marks are all new to a copy. A worker's copy is of the
        // first 16 MiB, where the index is never thinned.)
        let before = before.min(self.marks.len());
        Growth {
            size: self.size,
            lines: self.lines,
            ends_newline: self.ends_newline,
            first_long: self.first_long,
            marks_before: before,
            marks: self.marks[before..].to_vec(),
            open_line: (lines > 0 && !ends_newline).then(|| lines - 1),
        }
    }

    /// [`TextDoc::grow`]'s reads: whether the pass moved on.
    fn step(stream: &mut Stream, cap: u64, budget: u64) -> Result<bool> {
        let end = stream.total.min(cap);
        let from = stream.scan.at;
        if from >= end {
            return Ok(false);
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
                None => Ok(false),
            };
        }
        Ok(true)
    }

    /// The index grows by what the screen read at its end (06 §7.2): past the pass's cap the
    /// lines known go on only through blocks the screen brought in, while those are contiguous
    /// with them. `None`: nothing of the kind is here.
    fn grow_from_screen(&mut self) -> Option<Growth> {
        let (before, lines, ends_newline) = (self.marks.len(), self.lines, self.ends_newline);
        let Backing::Stream(stream) = &mut self.backing else {
            return None;
        };
        if stream.source.is_none() || stream.scan.at < stream.total.min(stream.cap) {
            // A worker's copy; or the pass is still on its way here.
            return None;
        }
        let from = stream.scan.at;
        std::mem::swap(&mut stream.scan.marks, &mut self.marks);
        loop {
            let at = stream.scan.at;
            let piece = [&stream.seen, &stream.near]
                .into_iter()
                .flatten()
                .find(|(lo, bytes)| *lo <= at && at < lo + bytes.len() as u64)
                .map(|(lo, bytes)| bytes.slice((at - lo) as usize..));
            match piece {
                Some(piece) if at < stream.total => stream.scan.feed(&piece, stream.total),
                _ => break,
            }
        }
        std::mem::swap(&mut stream.scan.marks, &mut self.marks);
        if stream.scan.at == from {
            return None;
        }
        let scan = std::mem::take(&mut stream.scan);
        self.adopt(&scan);
        if let Backing::Stream(stream) = &mut self.backing {
            stream.scan = scan;
        }
        Some(self.growth(before, lines, ends_newline))
    }

    /// How far a stream's index pass goes (a local file's index is whole).
    fn cap(&self) -> u64 {
        self.stream().map_or(u64::MAX, |s| s.cap)
    }

    /// How far a stream's index pass goes from now on.
    fn set_cap(&mut self, cap: u64) {
        if let Backing::Stream(stream) = &mut self.backing {
            stream.cap = cap;
        }
    }

    /// A stream's index pass is on its way to its cap: the lines are known as far as it has
    /// read, the last of them maybe not to its end, and what lies past them waits for it.
    fn passing(&self) -> bool {
        self.stream()
            .is_some_and(|s| s.scan.at < s.total.min(s.cap))
    }

    /// The pass asks for nothing more (Go to was stopped, or found its line): what it had
    /// queued is not sent, and only what was on its way arrives.
    fn stop_pass(&mut self) {
        if let Backing::Stream(stream) = &mut self.backing {
            for reader in [stream.pass.as_mut(), stream.tail.as_mut()]
                .into_iter()
                .flatten()
            {
                let _ = reader.try_read(0, 0);
            }
        }
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
        self.marks.clone_from(&scan.marks);
        self.adopt(scan);
    }

    /// [`TextDoc::install`] without the marks (the document has them already).
    fn adopt(&mut self, scan: &Scan) {
        let size = scan.at;
        self.marks.retain(|m| m.line == 0 || m.offset < size);
        // (A unit cut by the end of what was read is the open line's too.)
        let open = scan.line_len + size % scan.enc.unit() as u64;
        self.open_at = match scan.last {
            Some(true) | None => size,
            Some(false) => size - open.min(size),
        };
        self.first_long = scan.first_long;
        if scan.line_len > LINE_MAX as u64 && self.first_long.is_none() {
            self.first_long = Some(scan.line);
        }
        self.ends_newline = scan.last == Some(true);
        self.lines = scan.line + u64::from(scan.last == Some(false));
        self.size = size;
        self.enc = scan.enc;
        if scan.pick.is_some() {
            self.pick = scan.pick;
            self.head.clone_from(&scan.head);
        }
    }

    /// The encoding the lines are read in (08 §7).
    pub fn encoding(&self) -> Encoding {
        self.enc
    }

    /// The encoding as the bar says it (§7.5): `UTF-16LE (BOM)`, `UTF-8 (guessed)`; `None`
    /// before the first bytes were read (a stream's first block on its way), when nothing has
    /// picked it yet — unless it was chosen.
    pub fn encoding_label(&self) -> Option<String> {
        match self.pick {
            Some(pick) => Some(pick.label(self.enc, &self.head)),
            None => self.chosen.map(|e| e.label().to_string()),
        }
    }

    /// Where line 1 starts: after a BOM skipped under the encoding (D26, D37), else at 0.
    fn first_start(&self) -> u64 {
        self.enc.bom_len(&self.head) as u64
    }

    /// Whether reading the lines in `enc` needs a new index (D22): its LF unit is not this
    /// encoding's. Else [`TextDoc::reencode`] changes it in place.
    pub fn needs_index_for(&self, enc: Encoding) -> bool {
        !self.enc.same_lines(enc)
    }

    /// The lines read in `enc` from now on, chosen for this opening (D21) — one whose LF unit
    /// is this encoding's (D22: [`TextDoc::needs_index_for`] is false), so the index stays: only
    /// line 1's start follows the BOM rule (§7.2), and the passes still to run read in it. A
    /// stream that has read nothing yet takes any encoding so (its pass has not begun).
    pub fn reencode(&mut self, enc: Encoding) {
        let fresh = self.is_stream() && self.size == 0;
        debug_assert!(
            fresh || !self.needs_index_for(enc),
            "{enc:?} needs a new index"
        );
        self.enc = enc;
        self.chosen = Some(enc);
        let first = self.first_start();
        if let Some(mark) = self.marks.first_mut() {
            mark.offset = first;
        }
        if self.lines <= 1 && !self.ends_newline {
            // Line 1 is the open line, or there is none: it starts where the BOM rule says now
            // — a file of only a BOM has one line under another encoding, none under its own.
            let any = first < self.size;
            self.lines = u64::from(any);
            self.open_at = if any { first } else { self.size };
        }
        let rest = self.rest.as_mut();
        let pass = match &mut self.backing {
            Backing::Stream(stream) => Some(&mut stream.scan),
            Backing::Local(_) => None,
        };
        for scan in [rest, pass].into_iter().flatten() {
            scan.choose(enc, first);
        }
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
            &self.shown,
            self.opened.2,
            &|| None,
            &|| stopped(cancel),
            on_chunk,
        )?;
        if !self.prefix_unchanged(&file, &before) {
            return Err(anyhow::Error::new(Stale));
        }
        let mut doc = Self::indexed(
            file,
            scan,
            self.opened,
            self.path.clone(),
            self.shown.clone(),
        );
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

    /// The file on a handle of its own with the index of its first `cap` bytes — the
    /// highlighting worker's: its state runs from the top of the file, so it goes no further
    /// than [`HIGHLIGHT_MAX`], and its copy of the index is of that much and no more (06
    /// §7.2). The copy ends at a mark: at a line start.
    fn share_first(&self, cap: u64) -> Result<TextDoc> {
        if self.size <= cap {
            return self.share();
        }
        let keep = self.marks.partition_point(|m| m.offset <= cap).max(1);
        let end = self.marks[keep - 1];
        let mut doc = self.share_with(self.marks[..(keep - 1).max(1)].to_vec())?;
        doc.size = end.offset;
        doc.lines = end.line;
        doc.ends_newline = true;
        doc.open_at = end.offset;
        doc.rest = None;
        Ok(doc)
    }

    /// The same file and index on a handle of its own.
    fn share(&self) -> Result<TextDoc> {
        self.share_with(self.marks.clone())
    }

    fn share_with(&self, marks: Vec<Mark>) -> Result<TextDoc> {
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
                    scan: Scan::reading(self.chosen),
                    blocking: None,
                    cap: stream.cap,
                    pins: Vec::new(),
                    quiet: None,
                    near: None,
                    seen: None,
                }))
            }
        };
        Ok(TextDoc {
            backing,
            enc: self.enc,
            pick: self.pick,
            head: self.head.clone(),
            chosen: self.chosen,
            size: self.size,
            marks,
            lines: self.lines,
            ends_newline: self.ends_newline,
            open_at: self.open_at,
            first_long: self.first_long,
            opened: self.opened,
            indexed_in: self.indexed_in,
            after_pass: self.after_pass,
            path: self.path.clone(),
            shown: self.shown.clone(),
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
                // From the blocks the last jump brought, when they hold all of it: no reader
                // is asked, so nothing beside them is fetched.
                if let Some((lo, near)) = &stream.near {
                    let end = lo + near.len() as u64;
                    let want = at.saturating_add(buf.len() as u64).min(stream.total);
                    if *lo <= at && at < end && want <= end {
                        let n = (want - at) as usize;
                        let from = (at - lo) as usize;
                        buf[..n].copy_from_slice(&near[from..from + n]);
                        self.page_reads += n as u64;
                        return Ok(n);
                    }
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
        if let Backing::Stream(stream) = &mut self.backing {
            // Read at the end of the lines known: the index grows by it (06 §7.2).
            let end = stream.scan.at;
            if stream.source.is_some() && at <= end && end < at + got.len() as u64 {
                stream.seen = Some((at, got.clone()));
            }
        }
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
        let enc = self.enc;
        let unit = enc.unit() as u64;
        let mut idx = self.mark_before(first);
        let (mut line, mut pos) = (self.marks[idx].line, self.marks[idx].offset);
        let mut window = Window_::default();
        while line < last {
            let next = self.next_mark(idx);
            let keep = line >= first;
            let start = pos;
            if line + 1 == next.line {
                // It ends where the next mark starts: read only what is shown of it.
                let newline = if next.line < self.lines || self.ends_newline {
                    unit
                } else {
                    0
                };
                // Checked: a file changed since it was indexed can put `pos` past the mark.
                let len = next
                    .offset
                    .checked_sub(pos)
                    .and_then(|n| n.checked_sub(newline))
                    .unwrap_or(0);
                if keep {
                    let want = len.min(LINE_MAX as u64) as usize;
                    let bytes = window.take(self, pos, want)?;
                    out.push(shown(enc, start, bytes, len > LINE_MAX as u64));
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
                    let avail = window.units(self, pos, want)?;
                    let avail = &avail[..avail.len().min(room_to_mark as usize)];
                    if avail.is_empty() {
                        return Err(anyhow::Error::new(Stale));
                    }
                    let (body, end) = match enc.find_lf(avail, pos) {
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
                            pos += i as u64 + unit;
                            break;
                        }
                        None => pos += avail.len() as u64,
                    }
                }
                if keep {
                    out.push(shown(enc, start, text, cut));
                }
            }
            line += 1;
        }
        Ok(out)
    }

    /// Where lines `first..first + count` of the index are (fewer at its end), for showing them
    /// whole (wrapped). Found as [`TextDoc::read_lines`] finds them — at most a span from the
    /// mark before `first`, then each line that ends before the next mark (such a line is short:
    /// it ends within a span of its block's mark), never past where the index says it must end.
    /// A line ending at a mark is not read at all: not its last byte either, which may be far
    /// away and not fetched (06 §7.2) — the row that holds its end looks for the CR
    /// ([`Span::cr`]).
    fn spans(&mut self, first: u64, count: usize) -> Result<Vec<Span>> {
        let mut out = Vec::new();
        let last = first.saturating_add(count as u64).min(self.lines);
        if first >= last {
            return Ok(out);
        }
        let enc = self.enc;
        let unit = enc.unit() as u64;
        let mut idx = self.mark_before(first);
        let (mut line, mut pos) = (self.marks[idx].line, self.marks[idx].offset);
        let mut window = Window_::default();
        while line < last {
            let next = self.next_mark(idx);
            let keep = line >= first;
            if line + 1 == next.line {
                let newline = if next.line < self.lines || self.ends_newline {
                    unit
                } else {
                    0
                };
                // Checked: a file changed since it was indexed can put `pos` past the mark.
                let len = next
                    .offset
                    .checked_sub(pos)
                    .and_then(|n| n.checked_sub(newline))
                    .unwrap_or(0);
                if keep {
                    out.push(Span {
                        line: Some(line),
                        start: pos,
                        len,
                        next: next.offset,
                        loose: false,
                        ends: true,
                        cr: len > 0,
                        blind: false,
                    });
                }
                pos = next.offset;
                idx += 1;
            } else {
                // As in `read_lines`: this line ends before the next mark and within a span of
                // this block's mark.
                let end = next.offset.min(self.marks[idx].offset.saturating_add(SPAN));
                let start = pos;
                // The unit before the LF: whether the last read ended with the CR unit.
                let mut before_cr = false;
                loop {
                    let room = end.saturating_sub(pos);
                    if room == 0 {
                        return Err(anyhow::Error::new(Stale));
                    }
                    let avail = window.units(self, pos, room.min(CHUNK as u64) as usize)?;
                    let avail = &avail[..avail.len().min(room as usize)];
                    if avail.is_empty() {
                        return Err(anyhow::Error::new(Stale));
                    }
                    match enc.find_lf(avail, pos) {
                        Some(i) => {
                            if i > 0 {
                                before_cr = enc.ends_with_cr(&avail[..i], pos);
                            }
                            let len = pos + i as u64 - start;
                            let cr = if len > 0 && before_cr { unit } else { 0 };
                            pos += i as u64 + unit;
                            if keep {
                                out.push(Span {
                                    line: Some(line),
                                    start,
                                    len: len - cr,
                                    next: pos,
                                    loose: false,
                                    ends: true,
                                    cr: false,
                                    blind: false,
                                });
                            }
                            break;
                        }
                        None => {
                            before_cr = enc.ends_with_cr(avail, pos);
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

    /// The line of the index byte `at` of the file is in (its last line for `at` past them):
    /// read from the mark before it, as [`TextDoc::spans`] reads.
    fn span_at(&mut self, at: u64) -> Result<Option<Span>> {
        if self.lines == 0 {
            return Ok(None);
        }
        // A place inside a skipped BOM is line 1's start (D37) — never the last line read
        // below, which no line starting at or before it would be.
        let at = at.max(self.marks[0].offset);
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

    /// The lines the index gives whole, and the byte they end at: all of them once the file is
    /// indexed whole, and while the pass is on its way (the last maybe short: it grows with
    /// the pass); else the lines that end with their newline. From that byte on a place is
    /// found by its bytes ([`TextDoc::piece_holding`]).
    fn known(&self) -> (u64, u64) {
        if self.whole() || self.passing() || self.ends_newline || self.lines == 0 {
            (self.lines, self.size)
        } else {
            (self.lines - 1, self.open_at)
        }
    }

    /// The last LF unit in `[lo, x)` — before the unit `x` is in, which ends `x`'s line when it
    /// is one — read backwards a little at a time: a short line's start costs a short read.
    /// `lo` is a multiple of the unit, and so is every read's start: no unit is cut in two.
    fn newline_before(&mut self, x: u64, lo: u64, window: &mut Window_) -> Result<Option<u64>> {
        let enc = self.enc;
        let (mut hi, mut step) = (enc.align_down(x), 1024u64);
        while hi > lo {
            let from = hi.saturating_sub(step).max(lo);
            let bytes = window.take(self, from, (hi - from) as usize)?;
            if bytes.len() < (hi - from) as usize {
                return Err(anyhow::Error::new(Stale));
            }
            if let Some(i) = enc.rfind_lf(&bytes, from) {
                return Ok(Some(from + i as u64));
            }
            hi = from;
            step = (step * 4).min(CHUNK as u64);
        }
        Ok(None)
    }

    /// Where both pieces cut at grid point `g` meet (08 §7.3 rule 1, D36): at `g`, or up to
    /// three bytes before it, at the start of the character `g` falls in — found from the
    /// encoding's look-back before `g` (or from `lo`, a character start, when that is nearer).
    /// `None`: no character start is proven there (CP949, D28): the piece after it shows its
    /// bytes as placeholder marks ([`Span::blind`]).
    fn grid_cut(&mut self, g: u64, lo: u64, window: &mut Window_) -> Result<Option<u64>> {
        let enc = self.enc;
        let from = g.saturating_sub(enc.look_back() as u64).max(lo);
        let bytes = window.take(self, from, (g - from) as usize)?;
        let near = Around {
            at: from,
            bytes: &bytes,
            anchored: from == lo,
        };
        Ok(enc.grid_cut(near, g))
    }

    /// Rule 3 (08 §7.3), a jump's place `x`: rounded down to the unit, and in UTF-16 back
    /// across a valid pair's low half — read from the few bytes around it.
    fn jump_place(&mut self, x: u64, window: &mut Window_) -> Result<u64> {
        let enc = self.enc;
        if enc.unit() == 1 {
            return Ok(x);
        }
        let q = enc.align_down(x);
        let from = q.saturating_sub(enc.look_back() as u64);
        let bytes = window.take(self, from, (q - from) as usize + crate::encoding::AHEAD)?;
        Ok(enc.jump_place(Around::new(from, &bytes), x))
    }

    /// The line that starts at byte `start`, past the index — or, `loose`, the piece that
    /// starts there of a line whose start is not known: read on to its newline, and no further
    /// than the grid point that ends it (06 §7.2: a line is never read whole). A line with a
    /// known start is one piece as far as [`REACH`] past the grid point at or before its
    /// start: from every byte of that piece its start is found again, and the piece lies in
    /// the blocks a jump to any byte of it asks for ([`TextDoc::bring`]) — placing a line is
    /// never a second ask. A piece from the grid runs to the next grid point.
    fn scan_span(&mut self, start: u64, loose: bool, window: &mut Window_) -> Result<Span> {
        let total = self.total();
        let enc = self.enc;
        let unit = enc.unit() as u64;
        let bound = if loose {
            ((start + 3) / GRID + 1) * GRID
        } else {
            start / GRID * GRID + REACH
        };
        let bound = bound.min(total);
        // A piece from the grid point itself, where no character start was proven (D28; only
        // an encoding that needs one asks, so the others read nothing more). Nothing before
        // the grid point is known to be a character start: the look-back is not anchored.
        let blind = loose
            && enc.needs_phase()
            && start.is_multiple_of(GRID)
            && self.grid_cut(start, 0, window)?.is_none();
        let piece = |len: u64, next: u64, ends: bool, cr: bool| Span {
            line: None,
            start,
            len,
            next,
            loose,
            ends,
            cr,
            blind,
        };
        let (mut pos, mut before_cr) = (start, false);
        while pos < bound {
            let room = bound - pos;
            let avail = window.units(self, pos, room.min(CHUNK as u64) as usize)?;
            let avail = &avail[..avail.len().min(room as usize)];
            if avail.is_empty() {
                return Err(anyhow::Error::new(Stale));
            }
            if let Some(i) = enc.find_lf(avail, pos) {
                if i > 0 {
                    before_cr = enc.ends_with_cr(&avail[..i], pos);
                }
                let len = pos + i as u64 - start;
                let cr = if len > 0 && before_cr { unit } else { 0 };
                return Ok(piece(len - cr, pos + i as u64 + unit, true, false));
            }
            before_cr = enc.ends_with_cr(avail, pos);
            pos += avail.len() as u64;
        }
        if bound >= total {
            return Ok(piece(total - start, total, true, total > start));
        }
        // No character start proven at the grid point: the next piece begins on it, blind.
        let cut = self.grid_cut(bound, start, window)?.unwrap_or(bound);
        Ok(piece(cut - start, cut, false, false))
    }

    /// The line byte `x` (past the index) is in, when its start is within reach — after the
    /// last newline less than [`REACH`] before `x`'s grid point, or at the end of the index —
    /// else the piece from that grid point (06 §7.2: an unknown line start).
    fn piece_holding(&mut self, x: u64, window: &mut Window_) -> Result<Span> {
        let (_, known) = self.known();
        let grid = x / GRID * GRID;
        let lo = grid.saturating_sub(REACH).max(known);
        let start = match self.newline_before(x, lo, window)? {
            Some(newline) => Some(newline + self.enc.unit() as u64),
            // No newline since the index's end: its last line goes on. While the pass is on
            // its way that line is the pass's to place.
            None if lo == known && self.passing() && !self.ends_newline && known > 0 => {
                return Err(anyhow::Error::new(Pending));
            }
            None if lo == known => Some(known),
            None => None,
        };
        // (A start found is less than a span before the grid point — or it is the end of the
        // index, whose piece ends at that grid point at the latest: what follows it, below.)
        let span = match start {
            Some(start) => self.scan_span(start, false, window)?,
            None => {
                // (No character start proven at the grid point: the piece begins on it, blind.)
                let cut = self.grid_cut(grid, lo, window)?.unwrap_or(grid);
                self.scan_span(cut, true, window)?
            }
        };
        let mut span = span;
        while x >= span.next && span.next < self.total() {
            // Past the piece that was found — it ends on the grid, or a few bytes before a
            // grid point, at the start of the character that falls on it: the pieces after
            // it, as far as the one `x` is in.
            span = self.scan_span(span.next, !span.ends, window)?;
        }
        Ok(span)
    }

    /// The line (or piece) byte `x` of the file is in; `None` for a file with no line.
    fn span_holding(&mut self, x: u64, window: &mut Window_) -> Result<Option<Span>> {
        let (_, known) = self.known();
        let total = self.total();
        if total == 0 || self.lines == 0 {
            return Ok(None);
        }
        let x = x.min(total - 1);
        if x < known || self.whole() {
            return self.span_at(x);
        }
        self.piece_holding(x, window).map(Some)
    }

    /// The line that starts at byte `start` (`loose`: the piece that does). A place that is no
    /// line start of the index any more — the index grew over a piece, or changed — gives the
    /// line that holds it.
    fn span_starting(&mut self, start: u64, loose: bool, window: &mut Window_) -> Result<Span> {
        let (_, known) = self.known();
        if start < known || self.whole() {
            return self
                .span_at(start)?
                .ok_or_else(|| anyhow::Error::new(Stale));
        }
        if start >= self.total() {
            return Err(anyhow::Error::new(Stale));
        }
        self.scan_span(start, loose, window)
    }

    /// What follows `span` in the file: the next line of the index, or past it the line (or
    /// piece) that starts where `span` ends. `None` at the end of the file — and at the end of
    /// the lines known while the pass is on its way (06 §7.2: scrolling stops there, and is
    /// not held).
    fn span_after(&mut self, span: &Span, window: &mut Window_) -> Result<Option<Span>> {
        let (lines, known) = self.known();
        if let Some(line) = span.line {
            if line + 1 < lines {
                return Ok(self.spans(line + 1, 1)?.into_iter().next());
            }
            if self.whole() || self.passing() {
                return Ok(None);
            }
        }
        if span.next >= self.total() {
            return Ok(None);
        }
        if span.next < known {
            return self.span_at(span.next);
        }
        self.scan_span(span.next, !span.ends, window).map(Some)
    }

    /// What comes before `span` in the file: the line before it in the index, or past the
    /// index the line whose start is within reach of `span`'s start, else the piece from the
    /// grid point before it.
    fn span_before(&mut self, span: &Span, window: &mut Window_) -> Result<Option<Span>> {
        if span.start == 0 {
            return Ok(None);
        }
        if let Some(line) = span.line {
            return Ok(match line {
                0 => None,
                line => self.spans(line - 1, 1)?.into_iter().next(),
            });
        }
        let (_, known) = self.known();
        // The byte before it: its line's newline, or — a piece — the last of the piece before.
        let x = span.start - 1;
        if x < known {
            return self.span_at(x);
        }
        self.piece_holding(x, window).map(Some)
    }

    /// The file's last line (or piece); `None` for a file with no line. `known`: the last
    /// line known, while the pass is on its way (where scrolling stops meanwhile).
    fn last_span(&mut self, known: bool, window: &mut Window_) -> Result<Option<Span>> {
        let (lines, _) = self.known();
        if self.lines == 0 {
            return Ok(None);
        }
        if self.whole() || (known && self.passing()) {
            return Ok(self.spans(lines.saturating_sub(1), 1)?.into_iter().next());
        }
        let total = self.total();
        self.span_holding(total - 1, window)
    }

    /// The blocks around byte `x` (06 §7.2: a jump asks once): the 64 KiB before the 16 KiB
    /// grid point at or before `x` and the 64 KiB after it — at most three blocks; the file's
    /// end is two — each asked for by a reader of its own, as one byte of it: a demand, with
    /// no margin beside it. [`Pending`] until all of them are here; then the jump's page is
    /// read from them ([`Stream::near`]), through no reader.
    fn bring(&mut self, x: u64) -> Result<()> {
        #[cfg(test)]
        if self.fail_reads {
            anyhow::bail!("an injected read error");
        }
        let Backing::Stream(stream) = &mut self.backing else {
            return Ok(());
        };
        let total = stream.total;
        let Some(source) = stream.source.as_ref().filter(|_| total > 0) else {
            return Ok(());
        };
        let block = crate::stream::BLOCK;
        let grid = x.min(total - 1) / GRID * GRID;
        let (first, last) = (
            grid.saturating_sub(SPAN) / block,
            ((grid + SPAN).min(total) - 1) / block,
        );
        let (from, to) = (first * block, ((last + 1) * block).min(total));
        if let Some((lo, bytes)) = &stream.near {
            if *lo <= from && to <= lo + bytes.len() as u64 {
                return Ok(());
            }
        }
        while stream.pins.len() < (last - first + 1) as usize {
            stream
                .pins
                .push(Box::new(source.reader(ReaderKind::Interactive)));
        }
        let mut waits = false;
        for (pin, b) in stream.pins.iter_mut().zip(first..=last) {
            // Not the block's first byte: a reader's margin reaches a read's length back.
            match pin.try_read((b * block + 1).min(total - 1), 1) {
                Poll::Ready(Ok(_)) => {}
                Poll::Ready(Err(e)) => return Err(anyhow::Error::new(e)),
                Poll::Pending => waits = true,
            }
        }
        if waits {
            return Err(anyhow::Error::new(Pending));
        }
        let quiet = stream
            .quiet
            .get_or_insert_with(|| Box::new(source.reader(ReaderKind::Bulk)));
        match quiet.try_read(from, (to - from) as usize) {
            Poll::Ready(Ok(bytes)) => {
                stream.near = Some((from, bytes));
                Ok(())
            }
            Poll::Ready(Err(e)) => Err(anyhow::Error::new(e)),
            Poll::Pending => Err(anyhow::Error::new(Pending)),
        }
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
#[derive(Debug, Clone)]
struct Scan {
    /// Bytes read so far, from byte 0 of the file (a BOM skipped is among them).
    at: u64,
    line: u64,
    /// Bytes of the line so far, in whole units (a unit cut by the end of a read is not
    /// counted yet).
    line_len: u64,
    first_long: Option<u64>,
    /// The last unit of a line read: `Some(true)` when it was the LF unit and nothing came
    /// after it, `Some(false)` when something did; `None` before any.
    last: Option<bool>,
    /// The encoding it reads in, and the one chosen for this opening, if any (else the
    /// default its first bytes pick).
    enc: Encoding,
    chosen: Option<Encoding>,
    /// The default, once the first bytes were read, and the file's first four of them.
    pick: Option<Pick>,
    head: Vec<u8>,
    /// The unit being read: its bytes so far (a read can end inside it).
    unit: [u8; 4],
    marks: Vec<Mark>,
    /// Lines between marks now: [`BLOCK_LINES`] (0 stands for it) until the marks were
    /// thinned, doubled each time they were.
    spacing: u64,
    /// The most marks before they are thinned ([`MARKS_MAX`]; 0 stands for it).
    limit: usize,
    /// A mark due at the very end of what was read: it is the index's once a byte follows it.
    held: Option<Mark>,
    /// Tests: the most marks the index ever had room allocated for, and how often it was
    /// thinned.
    #[cfg(test)]
    peak: usize,
    #[cfg(test)]
    thinned: usize,
    /// Tests: where the marks were, and the room they had, as the last thinning left them.
    #[cfg(test)]
    after_thin: (usize, usize),
    /// The pass stopped at its cap, short of the file as opened.
    capped: bool,
    /// The hash of every byte read, for a pass that may stop short (a preview's): what its
    /// resume checks the bytes before the cut against.
    hasher: Option<sha2::Sha256>,
}

/// Bytes an index pass reads at a time (the cancel and the cap are looked at between reads).
const PASS_READ: usize = 1 << 20;

impl Default for Scan {
    fn default() -> Self {
        Self {
            at: 0,
            line: 0,
            line_len: 0,
            first_long: None,
            last: None,
            enc: Encoding::Utf8,
            chosen: None,
            pick: None,
            head: Vec::new(),
            unit: [0; 4],
            marks: Vec::new(),
            spacing: 0,
            limit: 0,
            held: None,
            #[cfg(test)]
            peak: 0,
            #[cfg(test)]
            thinned: 0,
            #[cfg(test)]
            after_thin: (0, 0),
            capped: false,
            hasher: None,
        }
    }
}

impl Scan {
    /// A pass that reads in `chosen`, or — `None` — in the default its first bytes pick.
    fn reading(chosen: Option<Encoding>) -> Self {
        Self {
            enc: chosen.unwrap_or(Encoding::Utf8),
            chosen,
            ..Self::default()
        }
    }

    /// Reads in `enc` from now on, chosen for this opening — one with the same LF unit (D22),
    /// or before the pass has begun; line 1 starts at `first` (the BOM rule).
    fn choose(&mut self, enc: Encoding, first: u64) {
        self.enc = enc;
        self.chosen = Some(enc);
        if let Some(mark) = self.marks.first_mut() {
            mark.offset = first;
        }
        if self.line == 0 && self.at > 0 {
            // Line 1 is the line being read: its bytes so far count from its new start.
            let unit = enc.unit() as u64;
            self.line_len = (self.at - self.at % unit).saturating_sub(first);
            self.last = (self.at > first).then_some(false);
        }
    }

    /// The next bytes of the file (`bytes`, at where the pass is), of a file of `size` bytes.
    /// The first ones (from byte 0) pick the default (08 §7.2); a BOM skipped under the
    /// encoding is no line's.
    fn feed(&mut self, bytes: &[u8], size: u64) {
        if bytes.is_empty() {
            return;
        }
        if let Some(mark) = self.held.take() {
            self.push(mark);
        }
        let mut bytes = bytes;
        if self.at == 0 && self.pick.is_none() {
            let pick = crate::encoding::pick(bytes, size > bytes.len() as u64);
            self.pick = Some(pick);
            self.enc = self.chosen.unwrap_or(pick.encoding);
            self.head = bytes[..bytes.len().min(4)].to_vec();
            let bom = self.enc.bom_len(&self.head);
            if let Some(mark) = self.marks.first_mut() {
                mark.offset = bom as u64;
            }
            self.at = bom as u64;
            bytes = &bytes[bom..];
            if bytes.is_empty() {
                return;
            }
        }
        let end = self.at + bytes.len() as u64;
        let lf = self.enc.lf();
        let n = lf.len();
        if n == 1 {
            // A one-byte unit: the byte 0x0A (UTF-8, ASCII, Latin-1, CP437, CP949).
            for (i, &b) in bytes.iter().enumerate() {
                if b != b'\n' {
                    self.line_len += 1;
                    continue;
                }
                self.newline(self.at + i as u64 + 1, end, size);
            }
            self.last = Some(bytes.last() == Some(&b'\n'));
        } else {
            // Units of 2 or 4 bytes, aligned from byte 0: one cut by the end of the last read
            // is finished by this one's first bytes, never a 0x0A byte inside a unit.
            let from = self.at;
            let mut ended = None;
            for (i, &b) in bytes.iter().enumerate() {
                let at = from + i as u64;
                let k = (at % n as u64) as usize;
                self.unit[k] = b;
                if k + 1 < n {
                    continue;
                }
                let is_lf = self.unit[..n] == *lf;
                ended = Some(is_lf);
                if is_lf {
                    self.newline(at + 1, end, size);
                } else {
                    self.line_len += n as u64;
                }
            }
            // Ended on the LF unit, or on something of a line: a unit, or part of one.
            let partial = !end.is_multiple_of(n as u64);
            self.last = Some(!partial && ended == Some(true));
        }
        self.at = end;
    }

    /// A line ended with its LF unit; the next starts at `start` (in a read ending at `end`).
    fn newline(&mut self, start: u64, end: u64, size: u64) {
        if self.line_len > LINE_MAX as u64 && self.first_long.is_none() {
            self.first_long = Some(self.line);
        }
        self.line_len = 0;
        self.line += 1;
        self.line_start(start, end, size);
    }

    /// Line `self.line` starts at byte `start`, of a file of `size` bytes read as far as `end`:
    /// a mark, if one is due there.
    fn line_start(&mut self, start: u64, end: u64, size: u64) {
        let last = self.marks[self.marks.len() - 1];
        if start >= size || !self.due(last, self.line, start) {
            return;
        }
        let mark = Mark {
            line: self.line,
            offset: start,
        };
        if start >= end {
            // No byte of this line is read yet: the index is of the bytes read, and has no
            // mark at their end.
            self.held = Some(mark);
        } else {
            self.push(mark);
        }
    }

    /// A line start `line`, at `offset`, is a mark after `last`: [`Scan::spacing`] lines on,
    /// or a span on.
    fn due(&self, last: Mark, line: u64, offset: u64) -> bool {
        line - last.line >= self.spacing() || offset - last.offset >= SPAN
    }

    fn spacing(&self) -> u64 {
        self.spacing.max(BLOCK_LINES)
    }

    fn limit(&self) -> usize {
        match self.limit {
            0 => MARKS_MAX,
            limit => limit,
        }
    }

    /// Adds a mark; the index is thinned first when it is full (06 §7.2: its memory is
    /// bounded).
    fn push(&mut self, mark: Mark) {
        while self.marks.len() >= self.limit() && self.spacing() < SPACING_MAX {
            self.thin();
        }
        let len = self.marks.len();
        if len == self.marks.capacity() {
            // Room is made by the index itself, never by `Vec`'s doubling: up to the bound it
            // doubles and stops there exactly; past it — only where no mark can be dropped
            // any more — it grows a sixteenth at a time.
            let more = match self.limit().checked_sub(len) {
                Some(left) if left > 0 => len.max(64).min(left),
                _ => (len / 16).max(4096),
            };
            self.marks.reserve_exact(more);
        }
        self.marks.push(mark);
        #[cfg(test)]
        {
            self.peak = self.peak.max(self.marks.capacity());
        }
    }

    /// Doubles the line spacing and drops the marks that are no longer due — but never one
    /// whose going would leave a line start a span or more past the mark before it: every line
    /// that does not start at a mark still starts less than a span past one, so finding a line
    /// still reads at most a span. What stays is at most a mark each [`Scan::spacing`] lines
    /// and two each 64 KiB.
    fn thin(&mut self) {
        #[cfg(test)]
        {
            self.thinned += 1;
        }
        self.spacing = self.spacing() * 2;
        // Where they are: no second set of marks is allocated beside the first, and the room
        // stays as it is (the index fills it again).
        let len = self.marks.len();
        let mut kept = 0usize;
        for i in 0..len {
            let mark = self.marks[i];
            // The line starts after this mark lie before the next one (the last mark's: not
            // known, so it stays). The first mark stays.
            let stays = match (kept.checked_sub(1), self.marks.get(i + 1)) {
                (None, _) | (_, None) => true,
                (Some(last), Some(next)) => {
                    let last = self.marks[last];
                    mark.line - last.line >= self.spacing() || next.offset - last.offset > SPAN
                }
            };
            if stays {
                self.marks[kept] = mark;
                kept += 1;
            }
        }
        self.marks.truncate(kept);
        #[cfg(test)]
        {
            self.after_thin = (self.marks.as_ptr() as usize, self.marks.capacity());
        }
    }

    /// Tests: `count` lines of `width` bytes each (their newline counted) follow, of a file of
    /// `size` bytes — the marks they give, as [`Scan::feed`] gives them, without their bytes.
    #[cfg(test)]
    fn lines_of(&mut self, count: u64, width: u64, size: u64) {
        assert!(width > 0 && self.line_len == 0);
        let (first, from) = (self.line, self.at);
        let end = from + count * width;
        if count > 0 {
            if let Some(mark) = self.held.take() {
                self.push(mark);
            }
        }
        let mut k = 0u64;
        while k < count {
            // The next line start that is a mark: lines on from the last one.
            let last = self.held.unwrap_or(self.marks[self.marks.len() - 1]);
            let by_lines = last.line + self.spacing() - (first + k);
            let by_bytes = (last.offset + SPAN).saturating_sub(from + k * width);
            let step = by_lines.min(by_bytes.div_ceil(width)).max(1);
            k += step;
            if k > count {
                break;
            }
            self.line = first + k;
            self.line_start(from + k * width, end, size);
        }
        self.line = first + count;
        self.at = end;
        self.last = Some(true);
    }

    /// The pass over `file` (opened at `path`, named `shown` in what is said, `size` bytes as opened) from where it is, to the
    /// end of those bytes or to `cap()` (asked before every read); `check()` after every read.
    /// Stops early, not capped, where the file ends sooner (it shrank meanwhile: the change is
    /// noticed later).
    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        file: &File,
        path: &Path,
        shown: &str,
        size: u64,
        cap: &dyn Fn() -> Option<u64>,
        check: &dyn Fn() -> Result<()>,
        on_chunk: &mut dyn FnMut(u64),
    ) -> Result<()> {
        // Only the tests' read log and their failing reads name the path.
        #[cfg(not(test))]
        let _ = path;
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
            #[cfg(test)]
            let failing = fails_index_reads(path);
            #[cfg(not(test))]
            let failing = false;
            let read = if failing {
                Err(std::io::Error::other("the read was made to fail"))
            } else {
                file.read_at(&mut buf[..want], self.at)
            };
            let n = read.with_context(|| format!("cannot read {shown}"))?;
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

/// Tests: the files whose index pass fails its next read (a disk that gives out mid-way).
#[cfg(test)]
static FAILING_INDEX_READS: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn fail_index_reads(path: &Path) {
    FAILING_INDEX_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(path.to_path_buf());
}

#[cfg(test)]
fn fails_index_reads(path: &Path) -> bool {
    FAILING_INDEX_READS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .any(|p| p == path)
}

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

/// Where a line is, by bytes (06 §7.2): its first byte in the file and how many bytes it shows
/// (its LF unit, and a CR unit before it, not counted). Past the index a line whose start is not
/// within reach is given in pieces from the 16 KiB grid (`loose`), and a long one is cut at a
/// grid point (`ends` is false: what follows is the same line's next piece).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    /// The line's number, where the index reaches it.
    line: Option<u64>,
    start: u64,
    len: u64,
    /// Where what follows it starts (the file's size: nothing does).
    next: u64,
    /// Its start is not known to be a line's.
    loose: bool,
    /// It ends where its line does.
    ends: bool,
    /// Its last unit was not looked at and may be the CR unit before its LF: the row that
    /// holds its end takes it and does not show it.
    cr: bool,
    /// A piece that begins on the grid where no character start is proven (CP949, D28): its
    /// bytes show as placeholder marks up to the first start a rule proves.
    blind: bool,
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

    /// [`Window_::at`] ending on a boundary of the units of `doc`'s encoding (counted from
    /// byte 0), so an LF unit is in it whole or not at all — unless less than a unit is left
    /// (the end of the file). Read from a unit boundary `pos`, as the line searches do.
    fn units(&mut self, doc: &mut TextDoc, pos: u64, want: usize) -> Result<&[u8]> {
        let n = doc.enc.unit() as u64;
        let len = self.at(doc, pos, want)?.len() as u64;
        let from = (pos - self.start) as usize;
        let over = (pos + len) % n;
        if over == 0 || len == 0 {
            return Ok(&self.bytes[from..]);
        }
        if len > over {
            return Ok(&self.bytes[from..from + (len - over) as usize]);
        }
        // Less than a unit is left of the last read: read again from here, a unit at least
        // (into a buffer of its own, as `at` does).
        let mut bytes = vec![0; want.max(n as usize).clamp(1, CHUNK)];
        let got = doc.read_at(&mut bytes, pos)?;
        bytes.truncate(got);
        self.bytes = bytes;
        self.start = pos;
        let len = got as u64;
        let over = (pos + len) % n;
        let keep = if len > over { len - over } else { len };
        Ok(&self.bytes[..keep as usize])
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

/// A line's shown bytes (from offset `at`, its start) as text in `enc`: a trailing CR unit
/// dropped; when `cut`, a character cut in two at the end left out; anything else that is not
/// a character shown as U+FFFD (08 §7.1) — by the one decoder the rows use too.
fn shown(enc: Encoding, at: u64, mut bytes: Vec<u8>, cut: bool) -> Line {
    if !cut && enc.ends_with_cr(&bytes, at) {
        bytes.truncate(bytes.len() - enc.unit());
    }
    let (cells, _) = enc.decode(&bytes, !cut);
    Line {
        text: cells.iter().map(|c| c.ch).collect(),
        cut,
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_nonewlines)
}

/// The syntaxes `syntax` is parsed with, and its own among them: a Makefile with
/// [`makefile_syntaxes`], when they could be built; anything else with the bundled ones.
fn parsing(syntax: &'static SyntaxReference) -> (&'static SyntaxSet, &'static SyntaxReference) {
    if syntax.name == "Makefile" {
        if let Some(set) = makefile_syntaxes() {
            if let Some(own) = set.find_syntax_by_name(&syntax.name) {
                return (set, own);
            }
        }
    }
    (syntaxes(), syntax)
}

/// The bundled Makefile grammar's lookaheads for a rule (in `main`) and for a variable (in
/// `variable-definitions`), by the length and FNV-1a hash of their source: only these are
/// rewritten ([`balanced_once`]), and only both together.
const MAKEFILE_LOOKAHEADS: [(usize, u64); 2] = [
    (3728, 0x5bc9_6045_23ad_8dd6),
    (13089, 0xdd3d_1fae_4ec9_06ec),
];

/// FNV-1a, 64 bits: the same on every build (std's hasher may change between them).
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The bundled syntaxes with Makefile's lookaheads rewritten ([`balanced_once`]); `None` when
/// they are not the ones the rewrite is for, and the grammar is parsed as bundled. Built once, by
/// the highlighting worker that first parses a Makefile: every grammar is read and built again
/// (75 ms; 13.4 MB more heap at its peak while it is built). From then on both sets stay for the
/// process's life, the bundled one for every other syntax: this one holds 4.5 MB more heap once
/// a Makefile is parsed with it (the bundled set, with a Rust and a Makefile line parsed, 9 MB).
fn makefile_syntaxes() -> Option<&'static SyntaxSet> {
    #[cfg(test)]
    MAKEFILE_SYNTAXES_ASKED.with(|n| n.set(n.get() + 1));
    static SET: OnceLock<Option<SyntaxSet>> = OnceLock::new();
    SET.get_or_init(|| with_makefile_rewritten(SyntaxSet::load_defaults_nonewlines()))
        .as_ref()
}

#[cfg(test)]
thread_local! {
    /// How often this thread asked for [`makefile_syntaxes`] (tests: never by a frame).
    pub(crate) static MAKEFILE_SYNTAXES_ASKED: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// `set` with the Makefile grammar's [`MAKEFILE_LOOKAHEADS`] rewritten, every grammar in its place
/// (a grammar's contexts name others' by place); `None` unless each of them is found.
fn with_makefile_rewritten(set: SyntaxSet) -> Option<SyntaxSet> {
    let bundled = set.into_builder();
    let mut rewritten = SyntaxSetBuilder::new();
    // Which of the lookaheads were found.
    let mut found = [false; MAKEFILE_LOOKAHEADS.len()];
    for syntax in bundled.syntaxes() {
        let mut syntax = syntax.clone();
        if syntax.name == "Makefile" {
            for context in syntax.contexts.values_mut() {
                for pattern in &mut context.patterns {
                    if let Pattern::Match(matched) = pattern {
                        let source = matched.regex.regex_str();
                        let known = (source.len(), fnv1a(source.as_bytes()));
                        if let Some(i) = MAKEFILE_LOOKAHEADS.iter().position(|&k| k == known) {
                            matched.regex = Regex::new(balanced_once(source));
                            found[i] = true;
                        }
                    }
                }
            }
        }
        rewritten.add(syntax);
    }
    found.iter().all(|&f| f).then(|| rewritten.build())
}

/// A Makefile lookahead with no two runs of characters side by side. It walks balanced
/// parentheses written out four levels deep, `[^()]* (?:\( … \))? [^()]*` at each: with the group
/// skipped, the two runs are one stretch shared every way there is, and nested that costs a
/// power of a line's length (an 86-character line 33 ms; 100 `x`s past the backtrack limit). Each
/// run after a group moves into it: `(?:\( … \) [^()]*)?`. What matches is the same — with the
/// group, the same pieces in the same order; without it, one `[^()]*` matches what two did — and
/// so is the order its ends are tried in (with the group first, then the stretch before it, each
/// longest first), so the lookahead holds at the same places and takes its captures from the
/// same match; a stretch is read once. The source is `(?x)`, a piece a line, so the move is a
/// line's: a line that closes a group (`\))?`) followed by one that is a run (`[^()]*`).
fn balanced_once(source: &str) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut out: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
    for i in 1..lines.len() {
        if lines[i - 1].trim_start().starts_with("\\))?")
            && lines[i].trim_start().starts_with("[^()]*")
        {
            out[i - 1] = out[i - 1].replacen("\\))?", "\\)", 1);
            out[i] = out[i].replacen("[^()]*", "[^()]*)?", 1);
        }
    }
    out.join("\n")
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

/// A character of a word run: what the grammars' identifier, number and keyword patterns are
/// made of, and never a delimiter (a quote, a bracket, a comment's start).
fn word(c: char) -> bool {
    c == '_' || c.is_ascii_digit() || c.is_alphabetic()
}

/// What a chain joins its word runs with, one of them, the same each time: nothing a bundled
/// grammar nests, and a chain is shortened by an even number of them, so `$`, which turns TeX's
/// math on and off, still leaves it as it was.
const CHAIN_SEPARATORS: [char; 4] = ['.', ',', '-', '$'];

/// What ends a stretch of a YAML line: what opens or ends something within one — a key's `:`, a
/// comment's `#`, a flow collection, a quote, an escape.
const YAML_STOPS: [char; 9] = [':', '#', '[', ']', '{', '}', '"', '\'', '\\'];

/// What of a syntax's lines its parser is handed shortened ([`Shortened`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Long runs of word characters, and long chains of short ones.
    Code,
    /// YAML: also a line's long stretches without its stops. YAML looks for a second key on
    /// every line, `(?=… ( [^\s:] | :\S | \s+(?![#\s]) )* \s*:(\s|$))` scanning the rest of the
    /// line from each place a key could start, so a long value — prose, quoted, in a list —
    /// costs the square of its length (5–7 ms at 500 characters).
    Yaml,
}

impl Shape {
    fn of(syntax: &SyntaxReference) -> Self {
        if syntax.name == "YAML" {
            Shape::Yaml
        } else {
            Shape::Code
        }
    }
}

/// The runs of characters `part` takes in `line[within]`, each as long as it goes: where it is
/// in the line, and how many characters it has.
fn runs_of(
    line: &str,
    within: std::ops::Range<usize>,
    part: impl Fn(char) -> bool,
) -> Vec<(std::ops::Range<usize>, usize)> {
    let mut runs = Vec::new();
    let (mut start, mut chars) = (within.start, 0);
    for (at, c) in line[within.clone()].char_indices() {
        let at = within.start + at;
        if part(c) {
            if chars == 0 {
                start = at;
            }
            chars += 1;
        } else if chars > 0 {
            runs.push((start..at, chars));
            chars = 0;
        }
    }
    if chars > 0 {
        runs.push((start..within.end, chars));
    }
    runs
}

/// Whether code's shortening finds anything in `line[within]` ([`Shortened::code`]): a word run
/// over [`RUN_MAX`], or names joined into a chain, between its numbers, longer than that. One
/// pass and no allocation, so an ordinary line costs nothing more.
fn code_shortens(line: &str, within: std::ops::Range<usize>) -> bool {
    // The run being read: its characters, and whether it is a number.
    let (mut run, mut number) = (0, false);
    // Since the last run: whether there was one, how many characters, the first of them.
    let (mut after_run, mut gap, mut first) = (false, 0, None::<char>);
    // The chain: its separator, and the part of it being read (since its last number) — its
    // names, and its characters from its first name on.
    let (mut separator, mut names, mut chars) = (None::<char>, 0, 0);
    // A space after the last character ends a run the line ends in.
    for c in line[within].chars().chain([' ']) {
        if word(c) {
            if run == 0 {
                number = c.is_ascii_digit();
                let joined = after_run
                    && gap == 1
                    && first.is_some_and(|s| {
                        CHAIN_SEPARATORS.contains(&s) && separator.is_none_or(|t| t == s)
                    });
                if joined {
                    separator = first;
                } else {
                    (separator, names, chars) = (None, 0, 0);
                }
            }
            run += 1;
            continue;
        }
        if run > 0 {
            if run > RUN_MAX {
                return true;
            }
            if number {
                (names, chars) = (0, 0);
            } else if names == 0 {
                (names, chars) = (1, run);
            } else {
                (names, chars) = (names + 1, chars + 1 + run);
                if chars > RUN_MAX {
                    return true;
                }
            }
            (run, after_run, gap, first) = (0, true, 0, None);
        }
        if gap == 0 {
            first = Some(c);
        }
        gap += 1;
    }
    false
}

/// Whether YAML's shortening finds anything in `line` ([`Shortened::of`]): in a stretch without
/// its stops, units (what spaces part) longer than [`RUN_MAX`] together, between its units over
/// that, or one of those in which code's shortening finds something. One pass and no allocation.
fn yaml_shortens(line: &str) -> bool {
    // The unit being read: its characters, and where it starts.
    let (mut unit, mut start) = (0, 0);
    // The part of the stretch being read: its units, its characters from its first unit on, and
    // the spaces since its last.
    let (mut units, mut chars, mut gap) = (0, 0, 0);
    // A stop after the last character ends the line's last stretch.
    for (at, c) in line.char_indices().chain([(line.len(), ':')]) {
        let stop = YAML_STOPS.contains(&c);
        if !stop && !c.is_whitespace() {
            if unit == 0 {
                start = at;
            }
            unit += 1;
            continue;
        }
        if unit > 0 {
            if unit > RUN_MAX {
                if code_shortens(line, start..at) {
                    return true;
                }
                (units, chars) = (0, 0);
            } else if units == 0 {
                (units, chars) = (1, unit);
            } else {
                (units, chars) = (units + 1, chars + gap + unit);
                if chars > RUN_MAX {
                    return true;
                }
            }
            (unit, gap) = (0, 0);
        }
        if stop {
            (units, chars, gap) = (0, 0, 0);
        } else {
            gap += 1;
        }
    }
    false
}

/// Where the chain from `words[i]` ends: the word runs after it, each one separator after the
/// one before, the same separator each time.
fn chain_end(line: &str, words: &[(std::ops::Range<usize>, usize)], i: usize) -> usize {
    let mut separator = None;
    let mut end = i + 1;
    while end < words.len() {
        let mut gap = line[words[end - 1].0.end..words[end].0.start].chars();
        match (gap.next(), gap.next()) {
            (Some(c), None)
                if CHAIN_SEPARATORS.contains(&c) && separator.is_none_or(|s| s == c) =>
            {
                separator = Some(c);
                end += 1;
            }
            _ => break,
        }
    }
    end
}

/// A line as the parser is handed it: each run of more than [`RUN_MAX`] word characters
/// shortened, each chain of names longer than that, and in YAML ([`Shape`]) each stretch longer
/// than that; the rest as it is. A grammar pattern that fancy-regex runs on its own engine (one
/// with a lookaround) costs the square of what it walks, until the backtrack limit, 1,000,000
/// steps, ends the search as no match: 1,000 `x`s in a Rust comment cost 20 ms, ordinary code
/// 1.5 µs a byte. What it walks is a run, or a chain — names joined by one separator, the same
/// each time ([`CHAIN_SEPARATORS`]): a dotted name in Python, Java or JavaScript, an argument
/// list in Go, an identifier with `$` in Java (1,000 characters of one name: 10–61 ms; of
/// varied names: up to 21 ms) —, or in YAML the rest of the line.
///
/// A shortened chain, or YAML stretch, is whole units of it — a name, or what spaces part, with
/// what parts it from the one before — from its start and its end, an even number of them left
/// out between, and longer than [`RUN_MAX`] (by at most three units): every two characters
/// side by side in it are side by side in the chain. A run over [`RUN_MAX`] parts a chain, and
/// so does a number: that run is shortened as alone, the names on each side as chains of their
/// own, and a list of numbers is no chain (it costs its length, and is coloured number by
/// number). A YAML unit over [`RUN_MAX`] parts its stretch the same way, shortened as code is.
///
/// A shortened run is pieces of the run itself, in order, nothing added: its first and last
/// [`RUN_END`] characters and, between them, the middle's stretches of one character — one
/// that stands alone kept alone (an exponent's `e`), a repeated one kept repeated, its length
/// modulo [`RUN_STRETCH`] — grown, while all of them fit, until the run is longer than
/// [`RUN_MAX`]; a middle of more stretches than fit (a hex blob) keeps its first ones, up to a
/// point a hash of the run picks, then each character the rest holds that they do not, once,
/// up to [`RUN_MIDDLE`] different ones. Every two characters side by side in it are side by
/// side in the run, but where such a middle is cut. So the parser sees the run's ends, its
/// characters, their order and everything around it, and the scopes it finds, put back on the
/// line ([`Self::place`]), are those of the line itself — on a run long enough for the limit,
/// those its patterns mean rather than no match. The differences known, all of runs, chains or
/// stretches over [`RUN_MAX`]:
/// - The part a shortened form leaves out takes the colour of the character kept before it, so
///   where a grammar colours it otherwise, not the colour it has: inside a varied run where a
///   grammar colours inside a word (CSS colours digits in one); in a chain whose names and what
///   joins them a grammar colours apart (SQL's `a.b.c`, TeX's `$`, JavaScript's
///   `a.b.c.f = function`, PHP's constants `A-B-C`, `true,false,null` in JSON, JavaScript or
///   Python); in a YAML flow collection (`[a, 1, true]`).
/// - Where a run's middle of many stretches is cut, two characters that are not neighbours in
///   the run meet, and a pattern that reads them together may read the kept ones otherwise.
/// - A run's middle with more than [`RUN_MIDDLE`] different characters keeps the first of them
///   only, so a test over the whole run (all of one script) may answer otherwise.
/// - A pattern that reads a whole chain sees its kept units only: one among those left out that
///   it would treat apart (JavaScript's `prototype` mid-chain) is not seen.
/// - A back-reference (a heredoc's delimiter) compares shortened text: two runs that agree in
///   their ends and in the stretches of their middle, each the same length modulo
///   [`RUN_STRETCH`] (200 and 216 `x`s), or in a middle of many stretches up to where the hash
///   cuts it and in what its rest holds, and two chains that agree in the units they keep, read
///   as the same.
struct Shortened {
    text: String,
    /// Where each piece of `text` starts: in `text`, and in the line it was copied from.
    pieces: Vec<(usize, usize)>,
}

impl Shortened {
    /// `None` when the line has nothing that long (it is parsed as it is): found without
    /// allocating, as the ordinary line has nothing.
    fn of(line: &str, shape: Shape) -> Option<Self> {
        let shortens = match shape {
            Shape::Code => code_shortens(line, 0..line.len()),
            Shape::Yaml => yaml_shortens(line),
        };
        if shortens {
            Self::made(line, shape)
        } else {
            None
        }
    }

    /// The line shortened, its runs, chains and stretches found and kept as they are copied.
    fn made(line: &str, shape: Shape) -> Option<Self> {
        let mut short = Self {
            text: String::new(),
            pieces: Vec::new(),
        };
        let copied = match shape {
            Shape::Code => short.code(line, 0, 0..line.len()),
            Shape::Yaml => {
                // The line is copied up to here.
                let mut copied = 0;
                let stretches = runs_of(line, 0..line.len(), |c| !YAML_STOPS.contains(&c));
                for (stretch, chars) in stretches {
                    if chars <= RUN_MAX {
                        continue;
                    }
                    // Its units are what spaces part: whole ones are left out, as a chain's are,
                    // so no token is cut in two and the spaces before its first (a line's
                    // indentation, YAML's structure) are kept. A long unit parts them, and is
                    // shortened as code is.
                    let units = runs_of(line, stretch, |c| !c.is_whitespace());
                    copied = short.parted(
                        line,
                        copied,
                        &units,
                        |_| false,
                        |short, copied, unit| short.code(line, copied, unit),
                    );
                }
                copied
            }
        };
        if short.pieces.is_empty() {
            return None;
        }
        short.copy(line, copied..line.len());
        Some(short)
    }

    /// Shortens `line[within]` as code is, the line copied up to `copied`; says where it is
    /// copied up to now. Its long word runs are shortened, and its long chains of names: names
    /// only — the patterns that cost a chain's square walk names, and a list of numbers
    /// (`1,2,3`) costs its length and is coloured number by number — so a number parts a chain,
    /// as a long run does (shortened as alone).
    fn code(&mut self, line: &str, mut copied: usize, within: std::ops::Range<usize>) -> usize {
        let words = runs_of(line, within, word);
        let number = |w: &(std::ops::Range<usize>, usize)| {
            line[w.0.clone()].starts_with(|c: char| c.is_ascii_digit())
        };
        let mut i = 0;
        while i < words.len() {
            let end = chain_end(line, &words, i);
            copied = self.parted(
                line,
                copied,
                &words[i..end],
                number,
                |short, copied, run| short.shorten(line, copied, run),
            );
            i = end;
        }
        copied
    }

    /// Shortens `units` — runs, each parted from the one before by what joins them — the line
    /// copied up to `copied`; says where it is copied up to now. A unit over [`RUN_MAX`] or one
    /// `apart` names parts them and is shortened by `long` (when long); the units between, more
    /// than one and longer than [`RUN_MAX`] together, are shortened as a chain.
    fn parted(
        &mut self,
        line: &str,
        mut copied: usize,
        units: &[(std::ops::Range<usize>, usize)],
        apart: impl Fn(&(std::ops::Range<usize>, usize)) -> bool,
        long: impl Fn(&mut Self, usize, std::ops::Range<usize>) -> usize,
    ) -> usize {
        let mut start = 0;
        for at in 0..=units.len() {
            let unit = units.get(at);
            if unit.is_some_and(|u| u.1 <= RUN_MAX && !apart(u)) {
                continue;
            }
            let joined = &units[start..at];
            if let (Some(first), Some(last)) = (joined.first(), joined.last()) {
                if joined.len() > 1 && line[first.0.start..last.0.end].chars().count() > RUN_MAX {
                    copied = self.chain(line, copied, joined);
                }
            }
            if let Some(unit) = unit.filter(|u| u.1 > RUN_MAX) {
                copied = long(self, copied, unit.0.clone());
            }
            start = at + 1;
        }
        copied
    }

    /// Copies the line from `from` to the end of the chain's units it keeps from its start, and
    /// says where those it keeps at its end start (copied with what follows them). A unit is a
    /// run with what parts it from the run before (the first, the run alone): the chain keeps its
    /// units to [`RUN_END`] characters from each end, then more of those after its start until
    /// it is longer than [`RUN_MAX`] and leaves out an even number of them.
    fn chain(
        &mut self,
        line: &str,
        from: usize,
        words: &[(std::ops::Range<usize>, usize)],
    ) -> usize {
        // Where unit `i` starts (at the end of the run before it), and its characters.
        let start = |i: usize| {
            if i == 0 {
                words[0].0.start
            } else {
                words[i - 1].0.end
            }
        };
        let chars = |i: usize| words[i].1 + line[start(i)..words[i].0.start].chars().count();
        let (mut head, mut kept) = (0, 0);
        while kept < RUN_END {
            kept += chars(head);
            head += 1;
        }
        let (mut tail, mut end) = (words.len(), 0);
        while end < RUN_END && tail > head {
            tail -= 1;
            end += chars(tail);
        }
        kept += end;
        while head < tail && (kept <= RUN_MAX || (tail - head) % 2 == 1) {
            kept += chars(head);
            head += 1;
        }
        self.copy(line, from..start(head));
        start(tail)
    }

    /// Copies the line from `from` to the end of the run's head, then the run's middle
    /// shortened, and says where its tail starts (copied with what follows it).
    fn shorten(&mut self, line: &str, from: usize, run: std::ops::Range<usize>) -> usize {
        let chars: Vec<(usize, char)> = line[run.clone()]
            .char_indices()
            .map(|(i, c)| (run.start + i, c))
            .collect();
        let tail = chars.len() - RUN_END;
        let middle = &chars[RUN_END..tail];
        // Where the middle's `i`th character starts in the line (the tail's start past its end).
        let at = |i: usize| middle.get(i).map_or(chars[tail].0, |c| c.0);
        // The run's FNV-1a hash: the same on every build (std's hasher may change between them).
        let hash = fnv1a(line[run].as_bytes());
        let cut = RUN_MIDDLE + (hash % RUN_STRETCH as u64) as usize;
        // The middle's stretches of one character, in order, while they fit: where each starts,
        // how long it is and how much of it is kept.
        let mut stretches: Vec<(usize, usize, usize)> = Vec::new();
        let (mut next, mut kept) = (0, 0);
        while next < middle.len() && kept <= cut {
            let c = middle[next].1;
            let len = middle[next..].iter().take_while(|m| m.1 == c).count();
            let keep = if len == 1 {
                1
            } else {
                2 + (len - 2) % RUN_STRETCH
            };
            stretches.push((next, len, keep));
            kept += keep;
            next += len;
        }
        // All of them fit: they grow, a whole number of times RUN_STRETCH, no longer than they are
        // (their kept length is theirs modulo it), until the middle is long enough.
        for (_, len, keep) in &mut stretches {
            while kept <= RUN_MIDDLE && *keep < *len {
                *keep += RUN_STRETCH;
                kept += RUN_STRETCH;
            }
        }
        self.copy(line, from..chars[RUN_END].0);
        let mut seen: Vec<char> = Vec::with_capacity(RUN_MIDDLE);
        for &(start, _, keep) in &stretches {
            self.copy(line, at(start)..at(start + keep));
            if !seen.contains(&middle[start].1) {
                seen.push(middle[start].1);
            }
        }
        // The rest of a middle of many stretches: each character not kept yet, once, in order.
        for &(place, c) in &middle[next..] {
            if seen.len() >= RUN_MIDDLE {
                break;
            }
            if !seen.contains(&c) {
                seen.push(c);
                self.copy(line, place..place + c.len_utf8());
            }
        }
        chars[tail].0
    }

    fn copy(&mut self, line: &str, part: std::ops::Range<usize>) {
        self.pieces.push((self.text.len(), part.start));
        self.text.push_str(&line[part]);
    }

    /// Where byte `at` of the text (one of its character starts, or its end) is in the line.
    fn place(&self, at: usize) -> usize {
        let i = self.pieces.partition_point(|&(text, _)| text <= at) - 1;
        let (text, line) = self.pieces[i];
        line + (at - text)
    }
}

/// Whether highlighting hands `line` to the parser shortened ([`Shortened`]): as YAML's line
/// when `yaml`, else as code's. For `tests/highlight_alloc.rs`, which counts its allocations
/// with an allocator of its own — in a test binary of its own, as that allocator slows every
/// allocation it sees.
#[doc(hidden)]
pub fn shortens(line: &str, yaml: bool) -> bool {
    let shape = if yaml { Shape::Yaml } else { Shape::Code };
    Shortened::of(line, shape).is_some()
}

/// Parser and highlighter state after some line.
type State = (ParseState, HighlightState);

/// Called with each line as the parser is handed it, before it is parsed (tests: a parser that
/// blocks on a line).
#[cfg(test)]
pub(crate) type ParseHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Highlighting state at each mark, computed in order from the top of the file — the worker's.
struct Highlight {
    highlighter: Highlighter<'static>,
    /// The syntax the file's name or first line picked.
    syntax: &'static SyntaxReference,
    /// The syntaxes it is parsed with ([`parsing`]), taken on the worker at its first page.
    set: Option<&'static SyntaxSet>,
    /// The state at mark `i`'s line, for every `i < states.len()` (none before the first page).
    states: Vec<State>,
    /// Lines from here on are plain (the first line cut for display, see the module doc).
    stop: u64,
    plain: Color32,
    /// Lines parsed so far (tests).
    parsed: Arc<AtomicU64>,
    /// What of its lines the parser is handed shortened.
    shape: Shape,
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
        Some(Self {
            highlighter,
            syntax,
            set: None,
            states: Vec::new(),
            stop,
            plain,
            parsed: Arc::default(),
            shape: Shape::of(syntax),
            #[cfg(test)]
            hook: None,
            #[cfg(test)]
            after_wake: None,
        })
    }

    fn run(&self, state: &mut State, line: &str) -> Runs {
        let short = Shortened::of(line, self.shape);
        let parsed = short.as_ref().map_or(line, |s| s.text.as_str());
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            hook(parsed);
        }
        self.parsed.fetch_add(1, Ordering::Relaxed);
        let set = self.set.unwrap_or_else(syntaxes);
        let Ok(mut ops) = state.0.parse_line(parsed, set) else {
            return vec![(self.plain, line.to_string())];
        };
        if let Some(short) = &short {
            for op in &mut ops {
                op.0 = short.place(op.0);
            }
        }
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
            if self.states.is_empty() {
                // The first page: the syntaxes it is parsed with (a grammar rewritten for it is
                // built here, on the worker, never on a frame).
                let (set, syntax) = parsing(self.syntax);
                self.set = Some(set);
                let start = HighlightState::new(&self.highlighter, ScopeStack::new());
                self.states.push((ParseState::new(syntax), start));
            }
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

/// The page on screen: where it starts in the file, and its rows.
struct Page {
    /// Where its first line starts in the file — a piece of a line, when `top_loose` — and
    /// that line's number where the index reaches it.
    top: u64,
    top_line: Option<u64>,
    top_loose: bool,
    /// Lines on it, from `top_line` (asked of the highlighter; 0 when their numbers are not
    /// known).
    count: usize,
    /// Rows it was read for.
    visible: usize,
    highlighted: bool,
    /// The generation its highlighting was asked under.
    gen: u64,
    /// Read wrapped: several rows to a line. Else one row to a line, from byte `left` of it.
    wrapped: bool,
    /// Wrapped: the byte of the top line its first row starts at.
    top_off: u64,
    left: u64,
    /// The top line's length (the status row).
    top_len: u64,
    /// The width its rows were laid out for, the rows, whether they reach the end of the
    /// file, and each line's colours by character once highlighted.
    width: f32,
    rows: Vec<wrap::Row>,
    at_end: bool,
    colors: Vec<Vec<Color32>>,
}

impl Page {
    /// A page with no row on it, at `top`.
    fn blank(top: u64, visible: usize, wrapped: bool) -> Self {
        Self {
            top,
            top_line: None,
            top_loose: false,
            count: 0,
            visible,
            highlighted: false,
            gen: 0,
            wrapped,
            top_off: 0,
            left: 0,
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
        self.rows.is_empty()
    }
}

/// Where the keys asked the view to go (applied when the next page is read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jump {
    /// Home: the start of the line at the top (its first row; not wrapped, its first byte), or
    /// the start of the file when already there.
    Home,
    /// End: the end of the line at the top when it runs past the page (its last row at the
    /// bottom; not wrapped, its last bytes at the right edge), else the end of the file.
    End,
    FileStart,
    FileEnd,
    /// The row byte `n` of the file is in (`SOPEN_STATE` `at=`, Go to `0x…`, the scroll bar).
    Byte(u64),
    /// Line `n` of the index at the top, from the row its byte `off` is in (Go to; a file
    /// indexed again: the place it was at).
    Line(u64, u64),
}

/// The Go to field's id (it has the keyboard while it is open).
fn goto_id() -> egui::Id {
    egui::Id::unique("sopen-text-goto")
}

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
    /// The place on screen, by bytes (06 §7.2): where the first line on screen starts in the
    /// file — `top_loose`: a piece from the grid of a line whose start is not known — and
    /// that line's number where the index reaches it. And how many rows fit (the last pass's).
    top: u64,
    top_loose: bool,
    line: Option<u64>,
    visible: u64,
    /// Not wrapped: the byte of each line its row starts at (→ and ← move it).
    left: u64,
    /// Lines wrap (the default; W or F2 switch it).
    wrap: bool,
    /// Wrapped: the byte of the top line the top row starts at.
    top_off: u64,
    /// Where the keys asked to go, and how many rows (not wrapped: lines) the keys and the
    /// wheel moved by — applied before the next page is read.
    jump: Option<Jump>,
    moved: i64,
    /// The top is put back on a row start before the next page (the width, the index or the
    /// mode changed).
    resnap: bool,
    /// The jump waiting is one that puts the view back where it was (a file read again, a
    /// stream reloaded), not one the owner made: it keeps the place sideways too.
    restoring: bool,
    /// The Go to field (Cmd/Ctrl+G), while it is open: what was typed.
    goto: Option<String>,
    /// Go to asked for a line past the lines known (0-based): the index pass runs on to it,
    /// the status row counts, and Esc stops it (06 §7.2).
    seeking: Option<u64>,
    /// A jump was marked on the stats line (`input`, 06 §11): its page is a `ready`.
    asked: bool,
    /// Lines the highlighter's copy of the index has: a page past them is shown plain.
    hl_lines: u64,
    /// The scroll bar's thumb is being dragged: where to (0 to 1). The jump is made when it
    /// is let go (a place the thumb only passes is not fetched).
    drag: Option<f32>,
    /// `SOPEN_STATE` `loading=1` (06 §11): a streamed window is ready to be shot at its first
    /// frame drawn with rows from the stream, what is still on its way shot as it is.
    shoot_loading: bool,
    /// Wrapped: what wrapping found so far (for this index and width).
    memo: wrap::Memo,
    /// The width rows are laid out in (the last pass's).
    room: f32,
    /// A plain row was laid out wider than its room (the fonts changed since it was wrapped): the
    /// page was wrapped again, once.
    rewrapped: bool,
    wheel_rest: f32,
    /// Not wrapped: the part of a byte the wheel sideways has moved by and not yet moved.
    wheel_across: f32,
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
    /// How much of a stream its index pass reads while the window is a preview: 02 §4.3's
    /// budget for text, given by the route that opened it ([`embed_source`]). `None`: no
    /// budget was given, and a preview's pass goes as far as a viewer's.
    preview_index: Option<u64>,
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
    /// `SOPEN_STATE` `enc=` (08 D34): the encoding chosen for this opening, applied on the
    /// first pass ([`TextWindow::choose`]).
    enc_wanted: Option<Encoding>,
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
    /// The file indexed again in another encoding whose LF unit is not the one read in now
    /// (D22), not a re-read: what it is, with its progress; Esc stops it.
    rebuild: Option<Rebuild>,
    /// The worker's thread ended (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    exited: Arc<AtomicBool>,
}

/// A local file indexed again in `enc` (D22): the bytes indexed so far and the size as
/// opened, for the status row, and the byte the view was at, put back at the top once it lands
/// (line numbers differ between LF units; a byte does not).
struct Rebuild {
    enc: Encoding,
    done: Arc<AtomicU64>,
    total: u64,
    place: Option<u64>,
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
            preview_index: None,
            enc_wanted: None,
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
            top_loose: false,
            line: None,
            visible: 1,
            left: 0,
            wrap: true,
            top_off: 0,
            jump: None,
            moved: 0,
            resnap: false,
            restoring: false,
            goto: None,
            seeking: None,
            asked: false,
            hl_lines: 0,
            drag: None,
            shoot_loading: false,
            memo: wrap::Memo::default(),
            room: 0.0,
            rewrapped: false,
            wheel_rest: 0.0,
            wheel_across: 0.0,
            page: None,
            stale_page: false,
            notices,
            status,
            painted: false,
        }
    }

    /// How far a stream's index pass goes: a preview's budget (the one its route gave it), else
    /// 16 MiB.
    fn pass_cap(&self) -> u64 {
        match self.preview_index {
            Some(budget) if self.preview => budget.min(HIGHLIGHT_MAX),
            _ => HIGHLIGHT_MAX,
        }
    }

    /// A jump to `jump` (the keys', Go to's, the scroll bar's): where the next page is read,
    /// and — on a stream — an `input` on the stats line with its place (06 §11).
    fn jump_to(&mut self, jump: Jump) {
        if self.seeking.is_some() {
            // Another place was asked for: Go to's pass is not waited for.
            self.stop_seek();
        }
        self.restoring = false;
        self.jump = Some(jump);
        let place = match jump {
            Jump::FileStart | Jump::Home | Jump::End => return,
            Jump::FileEnd => self.doc.total(),
            Jump::Byte(at) => at,
            Jump::Line(line, off) => {
                let mark = self.doc.marks[self.doc.mark_before(line)];
                mark.offset + off
            }
        };
        if let Some(source) = self.doc.source() {
            source.mark(crate::stream::Mark::Input(place));
            self.asked = true;
        }
    }

    /// Go to (06 §7.2): a line number, or `0x…` for a byte offset. A line the index has, and
    /// any offset, is a jump. A line past the lines known runs the index pass on to it.
    fn go(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let total = self.doc.total();
        let hex = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"));
        let wanted = match hex {
            Some(hex) => u64::from_str_radix(hex, 16).ok().map(Jump::Byte),
            None => match text.replace(',', "").parse::<u64>() {
                Ok(n) if n > 0 => Some(Jump::Line(n - 1, 0)),
                _ => None,
            },
        };
        self.status = Status::default();
        match wanted {
            None => {
                self.status = Status::error(format!(
                    "Go to: '{text}' is not a line number or a 0x… offset"
                ));
            }
            Some(_) if total == 0 => {}
            Some(Jump::Line(line, _)) => {
                if line < self.doc.line_count() || self.doc.whole() {
                    self.go_to_line(line, true);
                } else {
                    // Past the lines known: the pass goes on from where it stopped.
                    self.seeking = Some(line);
                    if let Some(source) = self.doc.source() {
                        source.mark(crate::stream::Mark::Input(self.doc.size()));
                        self.asked = true;
                    }
                }
            }
            Some(Jump::Byte(at)) => self.jump_to(Jump::Byte(at.min(total - 1))),
            Some(_) => {}
        }
        self.moved = 0;
    }

    /// Line `line` (0-based), which the index has, at the top — the file's last line when it
    /// has fewer. The last line known may have no end yet: its start is known all the same.
    /// `input`: the jump is marked on the stats line (not again, when Go to's pass marked it).
    fn go_to_line(&mut self, line: u64, input: bool) {
        let (lines, end) = self.doc.known();
        let count = self.doc.line_count();
        if line >= count {
            self.status =
                Status::info(format!("the file has {} lines", super::hex::grouped(count)));
        }
        let line = line.min(count.saturating_sub(1));
        self.moved = 0;
        let jump = if line < lines {
            Jump::Line(line, 0)
        } else {
            Jump::Byte(end)
        };
        if input {
            self.jump_to(jump);
        } else {
            self.jump = Some(jump);
        }
    }

    /// Go to's pass is over: the pass asks for nothing more, and stops where the viewer's
    /// stops again.
    fn stop_seek(&mut self) {
        self.seeking = None;
        self.doc.stop_pass();
        let cap = self.pass_cap();
        self.doc.set_cap(cap);
    }

    /// Go to's pass (06 §7.2), after a step of it: the line it looks for is known — it is at
    /// the top from the next page — or the file ended before it.
    fn seek(&mut self, ctx: &egui::Context) {
        let Some(line) = self.seeking else {
            return;
        };
        if self.doc.line_count() > line || self.doc.whole() {
            // The line has begun (its end is not looked for), or the file ended before it.
            self.stop_seek();
            self.go_to_line(line, false);
        }
        ctx.request_repaint();
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
            self.jump = Some(match page.top_line {
                Some(line) if !page.wrapped => Jump::Line(line, 0),
                _ => Jump::Byte(page.top + page.top_off),
            });
            self.restoring = true;
        }
        (self.top, self.top_off, self.top_loose, self.line) = (0, 0, false, None);
        self.seeking = None;
        // In the encoding chosen for this opening, if one was (D21).
        self.doc = TextDoc::streamed_as(source, self.doc.chosen);
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
    /// arrives, then — once the first screen is up — what is here of the next megabyte, as far
    /// as the pass goes (16 MiB; the whole file while Go to looks for a line). Past that the
    /// index grows by what the screen read at its end. The page on screen is read again only
    /// if it shows the line the index ended in, or lines the index has only now.
    fn grow_index(&mut self, ctx: &egui::Context) {
        if !self.doc.is_stream() {
            return;
        }
        let cap = match self.seeking {
            Some(_) => self.doc.total(),
            None => self.pass_cap(),
        };
        self.doc.set_cap(cap);
        if self.frozen() {
            return;
        }
        if self.stream_down() && !self.input {
            // A failed stream is asked again by a key, a click or a scroll, not by a pass.
            return;
        }
        // The first screen first — unless it is to be a place the pass has to reach: one
        // inside what the pass reads (a place past that is a jump by bytes, and waits for no
        // pass).
        let reach = self.doc.total().min(cap);
        let waits = match self.jump {
            Some(Jump::FileEnd) => reach >= self.doc.total(),
            Some(Jump::Byte(at)) => at < reach,
            Some(Jump::Line(..)) => true,
            _ => false,
        };
        if self.doc.size() > 0 && !self.first && !waits && self.seeking.is_none() {
            return;
        }
        let long = self.doc.long_lines();
        let (_, known) = self.doc.known();
        let growth = match self.doc.grow(cap, PASS_READ as u64) {
            Ok(Some(growth)) => growth,
            Ok(None) => match self.doc.grow_from_screen() {
                Some(growth) => growth,
                None => return self.seek(ctx),
            },
            // On its way: the Source's waker brings the next pass.
            Err(e) if is_pending(&e) => return,
            Err(e) => {
                if self.seeking.is_some() {
                    self.stop_seek();
                }
                return self.read_failed(&e);
            }
        };
        self.failure = None;
        // The page is read again only if a line on it got longer — the line the index ended
        // in, with no line end yet — or it was short of the window and may gain rows; or it
        // shows lines by their bytes that the index has now (they get their numbers).
        let (_, now_known) = self.doc.known();
        let numbered = self.page.as_ref().is_some_and(|p| {
            p.rows
                .iter()
                .any(|r| r.line.is_none() && r.at < now_known && now_known > known)
        });
        if growth.open_line.is_some() || now_known > known {
            // The line the index ended in may go on now, and lines found by their bytes have
            // numbers now: what wrapping found is of the index before. (The page on screen is
            // not read again for that, below.)
            self.memo.reset(self.room);
        }
        let grew = self.page.as_ref().is_some_and(|p| {
            // (`at_end`: its last row ended the last line indexed.)
            p.rows.len() < p.visible || (p.at_end && growth.open_line.is_some())
        });
        if grew || numbered {
            self.stale_page = true;
            self.resnap |= numbered;
        }
        if !long && self.doc.long_lines() && !self.status.error {
            self.status = opening_status(self.syntax, &self.doc);
        }
        if growth.size <= HIGHLIGHT_MAX {
            // The highlighter's copy of the index is of the first 16 MiB (06 §7.2).
            if let Some(worker) = &self.worker {
                self.hl_lines = growth.lines;
                worker.grown(growth);
            }
        }
        self.seek(ctx);
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
            let total = self.doc.total();
            self.gutter = Some(if self.doc.whole() {
                digits(lines)
            } else if total <= HIGHLIGHT_MAX {
                digits(total).min(8)
            } else {
                // Past the lines known the gutter shows byte offsets (06 §7.2): `…0x` and the
                // hex digits of the file's last byte — never fewer than its lines need.
                3 + format!("{:X}", total - 1).len()
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
            Some(p) if p.wrapped == self.wrap => p.at_end,
            // Nothing wrapped on screen yet: no end seen to follow.
            _ if self.wrap => false,
            _ => self.line.unwrap_or(0).saturating_add(self.visible) >= self.doc.line_count(),
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
        // In the encoding chosen for this opening, if one was (D21).
        let chosen = self.doc.chosen;
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
            let result = TextDoc::open_in(&path, chosen, &stop, &mut on_chunk);
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
            rebuild: None,
            exited,
        });
    }

    /// The lines read in `enc` from now on, chosen for this opening (`SOPEN_STATE` `enc=`;
    /// 08 §7.5): in place when its LF unit is the one read in now — line 1 follows the BOM
    /// rule — else the file is indexed again (D22): a stream from its first block, a local
    /// file by a pass on a worker with its progress on the status row, Esc keeping the
    /// encoding it had. Wrap's memos and the highlighter's states go either way, and the place
    /// on screen is kept by its byte (§7.3's jump rule).
    fn choose(&mut self, ctx: &egui::Context, enc: Encoding) {
        if self.doc.chosen == Some(enc) {
            return;
        }
        let place = self
            .page
            .as_ref()
            .map(|p| p.top + if p.wrapped { p.top_off } else { 0 })
            .filter(|&at| at > 0);
        let fresh = self.doc.is_stream() && self.doc.size() == 0;
        if fresh || !self.doc.needs_index_for(enc) {
            self.doc.reencode(enc);
        } else if let Some(source) = self.doc.source().cloned() {
            // Its first 16 MiB are read again (from the block cache where they still are).
            (self.top, self.top_off, self.top_loose, self.line) = (0, 0, false, None);
            self.seeking = None;
            self.doc = TextDoc::streamed_as(source, Some(enc));
            self.regutter = true;
            self.failure = None;
            self.streamed = self.doc.source().map(Source::state);
        } else {
            return self.rebuild(ctx, enc, place);
        }
        self.reread(place);
        ctx.request_repaint();
    }

    /// The lines are read another way (another encoding): what wrapping found, the
    /// highlighting and a syntax the first line picked go, and the view goes back to `place`,
    /// by its byte, unless the keys asked for another.
    fn reread(&mut self, place: Option<u64>) {
        self.memo.reset(self.room);
        self.worker = None;
        (self.hl_tried, self.hl_retry) = (false, false);
        self.syntax_due = syntax_by_name(&self.name).is_none();
        if self.syntax_due {
            self.syntax = None;
        }
        self.stale_page = true;
        self.resnap = true;
        if self.jump.is_none() {
            if let Some(at) = place {
                (self.jump, self.restoring) = (Some(Jump::Byte(at)), true);
            }
        }
    }

    /// A local file indexed again in `enc` (D22), on a worker: the page on screen stays, the
    /// status row counts, and Esc stops it, the encoding left as it was.
    fn rebuild(&mut self, ctx: &egui::Context, enc: Encoding, place: Option<u64>) {
        let doc = match self.doc.share() {
            Ok(doc) => doc,
            Err(e) => {
                self.status = Status::error(format!("{e:#}"));
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicU64::new(0));
        let total = self.doc.opened.2;
        let (stop, ex, counted, waker) =
            (cancel.clone(), exited.clone(), done.clone(), ctx.clone());
        #[cfg(test)]
        let hook = self.reindex_hook.clone();
        std::thread::spawn(move || {
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            let result = doc.reencoded(enc, &stop, &mut |at| {
                counted.store(at, Ordering::Relaxed);
                waker.request_repaint();
            });
            let _ = tx.send(result);
            ex.store(true, Ordering::Relaxed);
            waker.request_repaint();
        });
        self.worker = None;
        self.status = Status::default();
        self.reindex = Some(Reindex {
            cancel,
            rx,
            at_end: false,
            resume: None,
            rebuild: Some(Rebuild {
                enc,
                done,
                total,
                place,
            }),
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
            rebuild: None,
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
        let rebuilt = reindex.rebuild.as_ref().map(|r| (r.enc, r.place));
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
                if let Some((enc, _)) = rebuilt {
                    // Read again in the encoding it was being indexed in.
                    self.doc.chosen = Some(enc);
                }
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
                // Read again through the new index: the new end, or the same line put back on
                // a row start (the line may have changed).
                if at_end {
                    (self.jump, self.restoring) = (Some(Jump::FileEnd), true);
                } else if let Some((_, place)) = rebuilt {
                    // Read in another encoding (D22): back at the byte it was at — its line
                    // numbers are not the old ones.
                    if self.jump.is_none() {
                        self.jump = place.map(Jump::Byte);
                        self.restoring = place.is_some();
                    }
                    self.resnap = true;
                } else if self.jump.is_none() {
                    let line = self.line.unwrap_or(0).min(lines.saturating_sub(1));
                    (self.jump, self.restoring) = (Some(Jump::Line(line, self.top_off)), true);
                }
                (self.top, self.top_loose) = (0, false);
                self.status = if resumed || rebuilt.is_some() {
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

    /// The window's keys. `typing`: the Go to field has this pass's typing.
    fn keys(&mut self, ctx: &egui::Context, typing: bool) {
        if self.preview || typing {
            return;
        }
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
        // Cmd on macOS (Ctrl elsewhere), or Ctrl.
        let command = |key: Key| {
            ctx.input_mut(|i| {
                i.consume_key(Modifiers::COMMAND, key) || i.consume_key(Modifiers::CTRL, key)
            })
        };
        if self.seeking.is_some() && pressed(Key::Escape) {
            // Esc stops Go to's pass (06 §7.2): nothing more is sent.
            self.stop_seek();
            self.asked = false;
        }
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
        if self.reindex.as_ref().is_some_and(|r| r.rebuild.is_some()) && pressed(Key::Escape) {
            // Esc stops indexing the file in another encoding (D22): the index and the
            // encoding stay as they were.
            self.reindex = None;
            self.status = Status::info(format!("kept {}", self.doc.encoding().label()));
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
        // By rows — not wrapped: by lines — from the rows read when the next page is (going
        // back reads only from the restart point before a row).
        let page = self.visible.saturating_sub(1).max(1) as i64;
        let (file_start, file_end) = (command(Key::Home), command(Key::End));
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
                if !self.wrap {
                    // (Not wrapped, a jump is from where it lands: the lines moved by before
                    // it are not counted on.)
                    self.moved = 0;
                }
                self.jump_to(jump);
            }
        }
        if !self.wrap {
            // The place within a line is a byte of it (06 §7.2).
            if pressed(Key::ArrowRight) {
                self.left += SIDEWAYS;
            }
            if pressed(Key::ArrowLeft) {
                self.left = self.left.saturating_sub(SIDEWAYS);
            }
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
                    && p.width == self.room
                    && (if p.wrapped {
                        p.top_off == self.top_off
                    } else {
                        p.left == self.left
                    })
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
                self.page = Some(Page::blank(top, count, self.wrap));
            }
            if let Some(page) = self.page.as_mut() {
                page.highlighted = true;
            }
            return;
        }
        if self.doc.is_stream() && self.doc.size() == 0 && self.doc.total() > 0 {
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
            let mut page = match self.read_page(ctx, plain, font, count) {
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
                    let mut page = Page::blank(self.top, count, self.wrap);
                    page.top_off = self.top_off;
                    page.left = self.left;
                    page.width = self.room;
                    page
                }
            };
            self.stale_page = false;
            (self.pending, self.failure, self.reask) = (false, None, false);
            self.gen += 1;
            page.gen = self.gen;
            self.page = Some(page);
            self.ask_highlight();
            // The status row above was drawn before this page was read: once more for it.
            ctx.request_repaint();
        } else {
            self.pending = false;
            if std::mem::take(&mut self.reask) {
                // A highlighter that started after this page was read.
                self.gen += 1;
                if let Some(page) = self.page.as_mut() {
                    page.gen = self.gen;
                }
                self.ask_highlight();
            }
        }
        self.worker_results();
        if (self.waiting || due) && self.reindex.is_none() && !self.preview {
            // A change (also one the highlighter found): the re-read starts, or is scheduled,
            // in this frame, not at the next input.
            self.start_reindex(ctx);
        }
    }

    /// The page on screen is asked of the highlighter, when its copy of the index has all of
    /// its lines (06 §7.2: plain past the first 16 MiB, and where lines have no number).
    fn ask_highlight(&mut self) {
        let Some(page) = self.page.as_mut() else {
            return;
        };
        let top = page
            .top_line
            .filter(|top| page.count > 0 && top + page.count as u64 <= self.hl_lines);
        match (&self.worker, top) {
            (Some(worker), Some(top)) => {
                page.highlighted = false;
                worker.ask(self.gen, top, page.count);
            }
            _ => page.highlighted = true,
        }
    }

    /// The page of `rows` rows from the top — wrapped, or one row to a line — after the jump
    /// and the moves the keys asked for, and the top it ends up at: never past the last page
    /// (the file's last row at the bottom).
    fn read_page(
        &mut self,
        ctx: &egui::Context,
        plain: Color32,
        font: &FontId,
        rows: usize,
    ) -> Result<Page> {
        use wrap::{Measure, Pos, Wrapper};
        let total = self.doc.total();
        // While the pass is on its way, a place it will reach waits for it (06 §7.2: the
        // lines there get their numbers); a place past where it stops is a jump by bytes.
        let passing = self.doc.passing();
        let pass_end = total.min(self.doc.cap());
        let whole = self.doc.whole();
        let (lines, known) = self.doc.known();
        let waits = || Err(anyhow::Error::new(Pending));
        if self.memo.rewidth(self.room) {
            self.resnap = true;
        }
        // A jump past the lines known brings the blocks around its place first, in one ask.
        match self.jump {
            Some(Jump::FileEnd) if !whole && total > 0 => {
                if passing && pass_end >= total {
                    return waits();
                }
                self.doc.bring(total - 1)?;
            }
            Some(Jump::Byte(at)) if !whole && at >= known => {
                if passing && at < pass_end {
                    return waits();
                }
                self.doc.bring(at)?;
            }
            Some(Jump::Line(line, _)) if line >= lines && passing => return waits(),
            _ => {}
        }
        let cell = ctx.fonts_mut(|f| f.glyph_width(font, '0'));
        let columns = (self.room / cell.max(1.0)).floor().max(1.0) as u64;
        let wrap = self.wrap;
        // A place inside a line is a multiple of the unit (08 §7.3 rule 4).
        let mut left = self.doc.encoding().align_down(self.left);
        let mut w = Wrapper {
            doc: &mut self.doc,
            memo: &mut self.memo,
            m: Measure::new(ctx, font.clone(), plain),
            width: self.room,
            read: wrap::read_cap(self.room, cell),
        };
        let mut pos = Pos {
            at: self.top,
            off: if wrap { self.top_off } else { 0 },
            loose: self.top_loose,
        };
        let any = total > 0 && w.doc.line_count() > 0;
        if !any {
            pos = Pos::default();
        }
        // Not wrapped, a place is a line's start.
        let line_start = |p: Pos| if wrap { p } else { Pos { off: 0, ..p } };
        if any {
            // (A line's start stays one, and a place the jump replaces is not looked for:
            // neither is read.)
            let kept = matches!(self.jump, None | Some(Jump::Home | Jump::End));
            if self.resnap && kept && (pos.off > 0 || pos.loose) {
                // The row that holds the place now (the width, the mode or the index changed:
                // a piece the index has grown over is in a line of its own now).
                pos = match w.holding(pos.at.saturating_add(pos.off))? {
                    Some(p) if wrap => w.snap(p)?,
                    Some(p) => line_start(p),
                    None => Pos::default(),
                };
            }
            if !wrap && !kept && !self.restoring {
                // A jump the owner made lands on a line from its start. (One that puts the
                // view back after a re-read keeps the place within the line.)
                left = 0;
            }
            let back = rows.saturating_sub(1) as u64;
            pos = match self.jump {
                None => pos,
                Some(Jump::FileStart) => Pos::default(),
                Some(Jump::FileEnd) if wrap => w.end(rows, false)?,
                Some(Jump::FileEnd) => w.end_lines(rows, false)?,
                Some(Jump::Home) if wrap && pos.off > 0 => Pos { off: 0, ..pos },
                Some(Jump::Home) if !wrap && left > 0 => {
                    left = 0;
                    pos
                }
                Some(Jump::Home) => Pos::default(),
                Some(Jump::End) => {
                    let span = w.span(pos)?;
                    // The index's last line may go on, and the file's end is not known yet.
                    if passing && span.line.is_some_and(|line| line + 1 >= lines) {
                        return waits();
                    }
                    let within = if wrap {
                        let line_end = w.last_of(span)?;
                        let top = w.back(line_end, back)?;
                        (top > pos).then_some(top)
                    } else {
                        // The line's last characters at the right edge, when they are not
                        // there already.
                        let end = w.end_left(span, columns as usize)?;
                        (end > left).then(|| {
                            left = end;
                            pos
                        })
                    };
                    match within {
                        Some(top) => top,
                        None => {
                            // Already at the line's end: the end of the file.
                            self.jump_to(Jump::FileEnd);
                            return self.read_page(ctx, plain, font, rows);
                        }
                    }
                }
                Some(Jump::Byte(at)) if wrap => w.at_byte(at)?,
                Some(Jump::Byte(at)) => w.holding(at)?.map(line_start).unwrap_or_default(),
                Some(Jump::Line(line, off)) => {
                    let span = w.line(line.min(lines.saturating_sub(1)))?;
                    let p = Pos {
                        off,
                        ..Pos::of(&span)
                    };
                    if wrap {
                        w.snap(p)?
                    } else {
                        line_start(p)
                    }
                }
            };
            let n = self.moved.unsigned_abs();
            pos = match (self.moved.signum(), wrap) {
                (1, true) => w.forward(pos, n)?,
                (1, false) => w.lines_on(pos, n)?,
                (-1, true) => w.back(pos, n)?,
                (-1, false) => w.lines_back(pos, n)?,
                _ => pos,
            };
        }
        let read = |w: &mut Wrapper, pos: Pos| {
            if wrap {
                w.page(pos, rows)
            } else {
                w.flat(pos, left, rows)
            }
        };
        let (mut got, mut at_end) = read(&mut w, pos)?;
        if got.len() < rows && pos != Pos::default() {
            // Short of the window: the last page instead, no blank rows under the end — the
            // end of the lines known, while the pass is on its way.
            pos = if wrap {
                w.end(rows, true)?
            } else {
                w.end_lines(rows, true)?
            };
            (got, at_end) = read(&mut w, pos)?;
        }
        let top = match any {
            true => Some(w.span(pos)?),
            false => None,
        };
        (self.jump, self.moved, self.resnap) = (None, 0, false);
        self.restoring = false;
        (self.top, self.top_off, self.top_loose) = (pos.at, pos.off, pos.loose);
        self.line = top.and_then(|span| span.line);
        self.left = left;
        // The lines on it the index numbers: all of them, or none is asked of the highlighter.
        let count = match (self.line, got.last().and_then(|r| r.line)) {
            (Some(first), Some(last)) => (last - first + 1) as usize,
            _ => 0,
        };
        let mut page = Page::blank(pos.at, rows, wrap);
        page.top_line = self.line;
        page.top_loose = pos.loose;
        page.count = count;
        page.top_off = pos.off;
        page.left = left;
        page.top_len = top.map_or(0, |span| span.len);
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
                    // Each character's colour: the rows take theirs by index.
                    page.colors = runs
                        .iter()
                        .map(|line| {
                            line.iter()
                                .flat_map(|(c, t)| std::iter::repeat_n(*c, t.chars().count()))
                                .collect()
                        })
                        .collect();
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
        let parts = if self.doc.is_stream() && !self.doc.whole() {
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
        // Its encoding beside its syntax (08 §7.5, D34): `UTF-16LE (BOM)`, `UTF-8 (guessed)`.
        let mut parts = parts;
        parts.extend(self.doc.encoding_label());
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

    /// Where the page on screen is: its lines by number where the index reaches all of them,
    /// else its bytes — and that a line's start is not known, when the top one's is not.
    fn place(&self, page: &Page) -> String {
        let last = page.rows.last();
        match (page.top_line, last.and_then(|r| r.line)) {
            (Some(top), Some(last)) => {
                let mut first = (top + 1).to_string();
                if page.wrapped && page.top_off > 0 && page.top_len > 0 {
                    first.push_str(&format!(" ({} % of it)", page.top_off * 100 / page.top_len));
                }
                // (A local file's lines, not wrapped, are `lines 1–1` too, as they were.)
                let one = last == top && (page.wrapped || self.doc.is_stream());
                let mut shown = if one {
                    format!("line {first}")
                } else {
                    format!("lines {first}–{}", last + 1)
                };
                if self.doc.whole() {
                    shown.push_str(&format!(" of {}", self.doc.line_count()));
                }
                shown
            }
            _ => {
                let from = page.top + if page.wrapped { page.top_off } else { 0 };
                let to = last.map_or(from, |r| r.at + r.start + r.cut.bytes as u64);
                let mut shown = format!("bytes 0x{from:X}–0x{to:X}");
                if page.top_loose {
                    shown.push_str(" · line start not known");
                }
                shown
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let lines = self.doc.line_count();
        // A stream describes the page on screen — never where the keys asked to go — and says
        // `of N` only once N is the file's (06 §7.2).
        let stream = self.doc.is_stream();
        let shown = match &self.page {
            Some(page) if !page.is_empty() && (stream || lines > 0) => {
                let mut shown = self.place(page);
                if stream && self.pending {
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
            // (Nothing indexed yet: a file of only a BOM has bytes and no line.)
            _ if stream && self.doc.total() > 0 && (self.doc.size() == 0 || self.pending) => {
                self.loading()
            }
            _ if lines == 0 => "empty".to_string(),
            _ => {
                let top = self.line.unwrap_or(0);
                let last = top.saturating_add(self.visible).min(lines);
                format!("lines {}–{last} of {lines}", top + 1)
            }
        };
        let rebuild = self.reindex.as_ref().and_then(|r| r.rebuild.as_ref());
        let marks = self
            .page
            .as_ref()
            .is_some_and(|p| p.rows.iter().any(|r| r.cut.dim > 0));
        let resume = self.reindex.as_ref().and_then(|r| r.resume.as_ref());
        let status = match (resume, rebuild) {
            (Some((done, total)), _) => Status::info(format!(
                "indexing the rest of the file… {} of {}",
                crate::human(done.load(Ordering::Relaxed)),
                crate::human(*total)
            )),
            // Indexed again in another encoding (D22): how far, and that Esc keeps this one.
            (None, Some(r)) => Status::info(format!(
                "reading the lines as {}… {} of {} · Esc keeps {}",
                r.enc.label(),
                crate::human(r.done.load(Ordering::Relaxed)),
                crate::human(r.total),
                self.doc.encoding().label()
            )),
            // A stream's own state first (06 §5), then why its last read failed.
            (None, None) => match self.streamed.as_ref().and_then(|s| s.status()) {
                Some(text) => Status::error(text),
                None => match &self.failure {
                    Some(text) => Status::error(text.clone()),
                    // Go to's pass (06 §7.2): how far the lines are known, and how to stop.
                    None if self.seeking.is_some() => Status::info(format!(
                        "indexing… {} of {} · Esc stops",
                        crate::human(self.doc.size()),
                        crate::human(self.doc.total())
                    )),
                    // Placeholder marks on screen (08 §7.4): why.
                    None if marks && self.status.text.is_empty() => Status::info(format!(
                        "{}: no character boundary known here — shown from the next space or \
                         punctuation",
                        self.doc.encoding().label()
                    )),
                    None => self.status.clone(),
                },
            },
        };
        let mut go = None;
        ui.horizontal(|ui| {
            let place = !shown.is_empty();
            if place {
                ui.monospace(shown);
            }
            if let Some(text) = self.goto.as_mut() {
                // Go to (Cmd/Ctrl+G), as the hex window's: Enter goes there, Esc leaves.
                if place {
                    ui.separator();
                }
                ui.label("Go to");
                let field = ui.add(
                    chrome::field(text)
                        .id(goto_id())
                        .hint_text("line, or 0x… offset")
                        .desired_width(150.0),
                );
                if field.lost_focus() {
                    if ui.input(|i| i.key_pressed(Key::Enter)) {
                        go = Some(text.clone());
                    }
                    self.goto = None;
                }
                return;
            }
            if !status.text.is_empty() {
                if place {
                    ui.separator();
                }
                status.show(ui);
            }
        });
        if let Some(text) = go {
            self.go(&text);
        }
    }

    /// Where the scroll bar's thumb is (0 at the top, 1 at the end; `None`: nothing to scroll).
    /// By bytes while the line count is not known — and wrapped, always: rows cannot be counted
    /// without reading every line — else by lines (06 §7.2).
    fn thumb(&self) -> Option<f32> {
        if let Some(at) = self.drag {
            return Some(at);
        }
        let whole = self.doc.whole();
        match &self.page {
            Some(p) if p.wrapped || !whole => {
                let at_start = p.top == 0 && p.top_off == 0;
                // The end of what is indexed is the file's end only once the index is whole.
                match (p.at_end && whole, at_start) {
                    (true, true) => None,
                    (true, false) => Some(1.0),
                    _ if p.is_empty() => None,
                    _ => Some((p.top + p.top_off) as f32 / self.doc.total().max(1) as f32),
                }
            }
            page => {
                let max_top = self.doc.line_count().saturating_sub(self.visible);
                let top = page.as_ref().and_then(|p| p.top_line).or(self.line);
                let top = top.unwrap_or(0);
                (max_top > 0).then(|| top.min(max_top) as f32 / max_top as f32)
            }
        }
    }

    /// The scroll bar was clicked, or its thumb let go, at `at` (0 to 1): a jump (06 §7.2) —
    /// by lines when the thumb is by lines, else to that share of the file's bytes.
    fn scroll_to(&mut self, at: f32) {
        let at = f64::from(at.clamp(0.0, 1.0));
        let total = self.doc.total();
        if total == 0 {
            return;
        }
        let jump = if self.doc.whole() && !self.wrap {
            let max_top = self.doc.line_count().saturating_sub(self.visible);
            Jump::Line((max_top as f64 * at).round() as u64, 0)
        } else if at >= 1.0 {
            Jump::FileEnd
        } else {
            Jump::Byte(((total as f64 * at) as u64).min(total - 1))
        };
        self.moved = 0;
        self.jump_to(jump);
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
        if let Some(enc) = self.enc_wanted.take() {
            self.choose(&ctx, enc);
        }
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
            || (self.doc.is_stream()
                && !self.doc.whole()
                && self.preview_index.is_some_and(|b| self.doc.size() >= b));
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
            let (mut rows, mut across) = (0f32, 0f32);
            ctx.input(|i| {
                for event in &i.events {
                    if let egui::Event::MouseWheel { unit, delta, .. } = event {
                        rows -= match unit {
                            egui::MouseWheelUnit::Line => delta.y * 3.0,
                            egui::MouseWheelUnit::Page => delta.y * self.visible as f32,
                            egui::MouseWheelUnit::Point => delta.y / row_h,
                        };
                        across -= match unit {
                            egui::MouseWheelUnit::Line => delta.x * 3.0,
                            egui::MouseWheelUnit::Page => delta.x * 16.0,
                            egui::MouseWheelUnit::Point => delta.x / cw,
                        };
                    }
                }
            });
            if !self.wrap && across != 0.0 {
                // Not wrapped, the wheel sideways moves the place within the lines (06 §7.2).
                // What is less than a byte is kept for the next frame, as the rows' is: a slow
                // swipe is many frames of less than one each.
                let across = across + self.wheel_across;
                let whole = across.trunc();
                self.wheel_across = across - whole;
                self.left = self.left.saturating_add_signed(whole as i64);
            }
            if rows != 0.0 {
                let rows = rows + self.wheel_rest;
                let whole = rows.trunc();
                self.wheel_rest = rows - whole;
                self.moved += whole as i64;
            }
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
            // (Its state runs from the top of the file, so it goes over the first 16 MiB and
            // no further, whatever the file is read through: its copy of the index is of that
            // much, 06 §7.2.)
            if !self.frozen() {
                let stop = self.doc.first_long.unwrap_or(u64::MAX);
                let hl = self.syntax.and_then(|s| Highlight::new(s, p.text, stop));
                #[cfg(test)]
                let hl = hl.map(|mut h| {
                    h.hook = self.hook.clone();
                    h.after_wake = self.after_wake.clone();
                    h
                });
                self.worker = match (hl, self.doc.share_first(HIGHLIGHT_MAX)) {
                    (Some(hl), Ok(doc)) => {
                        self.hl_lines = doc.line_count();
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
        let bar = Rect::from_min_max(
            Pos2::new(rect.right(), rect.top()),
            Pos2::new(rect.right() + chrome::CELL.x, rect.bottom()),
        );
        let track = chrome::scroll_bar(&ui.painter_at(outer), bar, self.thumb());
        if !self.preview && !self.waiting && self.reindex.is_none() && !self.stream_frozen() {
            // A click on it, or its thumb let go, is a jump (06 §7.2). While the thumb is
            // dragged only the thumb moves: a place it passes is not read.
            let id = ui.scope_id().with("sopen-text-scroll");
            let response = ui.interact(bar, id, egui::Sense::click_and_drag());
            let room = (track.height() - chrome::CELL.y).max(1.0);
            let at = response
                .interact_pointer_pos()
                .map(|pos| ((pos.y - track.top() - chrome::CELL.y / 2.0) / room).clamp(0.0, 1.0));
            if response.dragged() {
                self.drag = at.or(self.drag);
            }
            if response.clicked() || response.drag_stopped() {
                if let Some(at) = at.or(self.drag) {
                    self.scroll_to(at);
                }
                self.drag = None;
                ctx.request_repaint();
            }
        }
        let text_painter =
            ui.painter_at(Rect::from_min_max(Pos2::new(text_x, rect.top()), rect.max));
        // The gutter: a line's number where the index reaches it; past that its byte offset
        // (06 §7.2), with `…` before it when it is not known to be a line's start.
        let number = |k: usize, row: &wrap::Row| {
            let label = match row.line {
                Some(line) => (line + 1).to_string(),
                None if row.loose => format!("…0x{:X}", row.at),
                None => format!("0x{:X}", row.at),
            };
            painter.text(
                Pos2::new(rect.left() + gutter, rect.top() + k as f32 * row_h),
                egui::Align2::RIGHT_TOP,
                label,
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
                        number(k, row);
                    }
                    let colors = row.first_char.zip(row.line).and_then(|(first, n)| {
                        let line = page.colors.get(n.checked_sub(page.top_line?)? as usize)?;
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
                    // Placeholder marks and C1 controls' `·` are dim (08 §7.4, D32).
                    let marked = row.cut.dim > 0 || !row.cut.dots.is_empty();
                    let plain = || {
                        if rtl || marked {
                            let job = directed_job(row_chars(row, None, p.text, p.dim), &font);
                            ctx.fonts_mut(|f| f.layout_job(job))
                        } else {
                            measured()
                        }
                    };
                    let mut galley = match colors {
                        Some(colors) => {
                            let job =
                                directed_job(row_chars(row, Some(colors), p.text, p.dim), &font);
                            ctx.fonts_mut(|f| f.layout_job(job))
                        }
                        None => plain(),
                    };
                    // Sections are shaped apart and can come out a hair wider than the row was
                    // measured: plain then, and at worst as measured (never cut at the edge).
                    if galley.size().x > page.width + 0.5 && colors.is_some() {
                        galley = plain();
                    }
                    if galley.size().x > page.width + 0.5 && (rtl || marked) {
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
                // Only the columns that fit are laid out: a row is read wider than it shows.
                let columns = ((rect.right() - text_x) / cw).floor().max(0.0) as usize;
                for (k, row) in page.rows.iter().enumerate() {
                    number(k, row);
                    let colors = row.first_char.zip(row.line).and_then(|(first, n)| {
                        let line = page.colors.get(n.checked_sub(page.top_line?)? as usize)?;
                        Some((first, line.as_slice()))
                    });
                    let mut shown = row_chars(row, colors, p.text, p.dim);
                    shown.truncate(columns);
                    let galley = ctx.fonts_mut(|f| f.layout_job(directed_job(shown, &font)));
                    text_painter.galley(
                        Pos2::new(text_x, rect.top() + k as f32 * row_h),
                        galley,
                        p.text,
                    );
                }
                if self.doc.passing() {
                    // The rows below the last line indexed so far (a line that runs past
                    // what is indexed): their starts are not known yet. They fill in as the
                    // index pass reaches them (06 §7.2).
                    for k in page.rows.len()..self.visible as usize {
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
        if self.asked && !self.pending && self.jump.is_none() && self.seeking.is_none() {
            // The page of the last jump is drawn: `ready` on the stats line (06 §11).
            self.asked = false;
            if let Some(source) = self.doc.source() {
                source.mark(crate::stream::Mark::Ready);
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
/// `colors`, the line's colour per character with the row's first at index `first`, else plain;
/// its placeholder marks and C1 controls' `·` `dim` (08 §7.4, D32).
fn row_chars(
    row: &wrap::Row,
    colors: Option<(usize, &[Color32])>,
    plain: Color32,
    dim: Color32,
) -> Vec<(char, Color32)> {
    let mut out = Vec::with_capacity(row.cut.shown.len());
    for (i, &(c, n)) in row.cut.shown.iter().enumerate() {
        let color = if i < row.cut.dim || row.cut.dots.binary_search(&i).is_ok() {
            dim
        } else {
            colors
                .and_then(|(first, colors)| colors.get(first + i).copied())
                .unwrap_or(plain)
        };
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
        // Cmd/Ctrl+G opens the Go to field in the status row (06 §7.2), which then has the
        // typing: no key of the window acts from there.
        let ctx = ui.ctx().clone();
        let open = super::command_key(&ctx, Key::G, false);
        if open {
            self.goto = Some(String::new());
            ctx.memory_mut(|m| m.request_focus(goto_id()));
        }
        let typing = self.goto.is_some() && (open || ctx.memory(|m| m.has_focus(goto_id())));
        self.keys(&ctx, typing);
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
        if self.shoot_loading && self.doc.is_stream() {
            // `loading=1` (06 §11): the first frame drawn with rows from the stream, and every
            // one after it — a page read on its way, rows that say `loading…`, highlighting
            // not here yet and Go to's pass are shot as they are. Never before a row came.
            let rows = self.page.as_ref().is_some_and(|p| !p.is_empty());
            return self.painted && self.first && rows;
        }
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
        let still = !self.waiting && self.reindex.is_none() && !self.reloading;
        // (Go to's pass is a jump on its way.)
        self.painted && still && self.seeking.is_none() && page
    }

    /// A preview (02 §4.2): no bars, a single-lined frame, no keys or wheel, no following and no
    /// re-read. `false` (the hand-over) brings all of that back; an index the preview stopped
    /// short goes on from where it stopped on the next pass.
    fn set_preview(&mut self, on: bool) {
        self.preview = on;
    }

    /// Held without being drawn, a streamed viewer does not poll its file (06 §5).
    fn set_shown(&mut self, shown: bool) {
        if let Some(source) = self.doc.source() {
            source.set_visible(shown);
        }
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
        _ if doc.total() > HIGHLIGHT_MAX => Status::info(format!(
            "highlighted over its first {}",
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

#[cfg(test)]
thread_local! {
    static EMBEDDED_LOADING: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Tests: the `loading` the last viewer embedded on this thread was given ([`embed_scripted`]);
/// `None` when none was.
#[cfg(test)]
pub(crate) fn embedded_loading() -> Option<bool> {
    EMBEDDED_LOADING.with(std::cell::Cell::get)
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
    embed_scripted(doc, title, name, notices, live, at, false, None)
}

/// [`embed`] for the viewer a scripted run opens: `loading` is `SOPEN_STATE`'s `loading=1`
/// (06 §11), as the window that hosts the viewer parsed it — a viewer on a stream is then
/// ready to be shot at its first frame drawn with rows, what is still on its way shot as it is.
/// `enc`: its `enc=NAME` (08 D34), the encoding chosen for this opening — read in from the
/// first pass on, the file indexed again only when the LF unit changes (D22).
#[allow(clippy::too_many_arguments)]
pub fn embed_scripted(
    doc: TextDoc,
    title: String,
    name: &str,
    notices: Vec<String>,
    live: bool,
    at: Option<u64>,
    loading: bool,
    enc: Option<Encoding>,
) -> Embedded {
    let mut window = TextWindow::new(doc, title, name, notices);
    window.live = live;
    window.shoot_loading = loading;
    window.enc_wanted = enc;
    #[cfg(test)]
    EMBEDDED_LOADING.with(|l| l.set(Some(loading)));
    if let Some(at) = at {
        window.jump_to(Jump::Byte(at));
    }
    Embedded {
        window: Box::new(window),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

/// The text viewer on a remote file read through `source` (06 §7.2): the window opens at once,
/// shows the first block when it arrives and indexes on from there. `title`, `name`, `notices`
/// and `at` as [`embed`]'s; it is never followed at its end. `preview_index`: how much of the
/// file its index pass reads while the window is a preview (02 §4.3's budget for text, given
/// by the route: the one constant is the route's); the hand-over goes on from there.
pub fn embed_source(
    source: Source,
    title: String,
    name: &str,
    notices: Vec<String>,
    at: Option<u64>,
    preview_index: Option<u64>,
) -> Embedded {
    let mut window = TextWindow::new(TextDoc::streamed(source), title, name, notices);
    window.preview_index = preview_index;
    if let Some(at) = at {
        // A place opened at is a jump (06 §11: an `input`, and its page a `ready`).
        window.jump_to(Jump::Byte(at));
    }
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
