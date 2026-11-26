//! The hex window (spec §6a): any file as rows of 16 bytes — offset, hex, ASCII — read through a
//! small page cache, so only the rows on screen are ever read and a file of any size scrolls
//! the same. Editing overwrites bytes only (the size never changes); Undo/Redo; Save, Save copy
//! (a new sibling) and Save As (a new local file).
//!
//! - A streamed file (06 §7.1) saves by ED1's routes (07 §4): copied on the server, patched,
//!   renamed — the file's bytes never cross the network. A file with a local base keeps 01 §6a's
//!   Save ([`HexFile::save`]).
//! - Every edit is kept in a journal on the local disk as well as in memory (07 §3), at most
//!   250 ms after it, at once on focus loss, at close and before a Save. No edit is taken while a
//!   Save runs (07 §10).
//! - On open, what an earlier session left — its journal, a Save cut off, an undo file — is
//!   offered in the recovery rows: Restore, Load as edits, Later, Discard.
//!
//! [`HexDoc`] is the edit model (the bytes as opened plus a sparse map of changed bytes) and
//! [`HexFile`] the write side; both are window-free, so the tests and the `SOPEN_AUTOSAVE` hook
//! run the exact code the window's buttons run.
//!
//! The screen is cells (10 §4–§6, X3-hex): the toolbar with its Go to field and `[ buttons ]`,
//! the frame with the rows and its scroll bar, the status row and, on its own, the F-key row are
//! written into buffers of whole cells ([`super::cells`]) and painted a glyph per cell, so the
//! window and the terminal show the same cells (TS-4). In a terminal that draws a glyph of
//! ambiguous width in two cells ([`cells::ambiguous_wide`], which the terminal backend sets) the
//! ASCII column's CP437 glyphs of width A are 10 §10's stand-ins — `+ - = | # ^ v < >`, and `.`
//! for the letters the table leaves ([`column_glyph`]).
//!
//! The rows below the grid and the window's questions are cells too (X3-questions): the lines to
//! read whole, the read-only window's keep row with its path field, a refusal's offer, the live
//! question about a temp and the recovery rows, each text wrapped by cells and each control a
//! `[ button ]` as the toolbar's; the close prompt, the first Save's question above 256 MiB,
//! Write in place and Save As are `ops::dialog` boxes between the window's top and its own
//! F-key row (hosted, the browser's title and status rows). Cells have no hover: what a hover
//! said is a row (where Discard deletes the kept edits, why a row's buttons wait). A row's
//! controls are reached by Tab past the ASCII column (Shift+Tab before the hex one), Tab and
//! Shift+Tab walking them; Enter or Space presses one, Esc gives the keyboard back to the grid.
//!
//! As the file browser's preview (02 §4.2, [`super::Window::set_preview`]) the window shows its
//! first rows without toolbar, status row or cursor, takes no input and never takes the keyboard.
//!
//! A file at a revision (03 §6.3, [`super::Window::set_read_only`]) is shown, moved through and
//! gone to as any other, but no byte is typed over and no Save, Save copy, Save As or Write in
//! place starts — each says [`AT_A_REVISION`] — and no journal is kept or looked for.

mod file;

pub use file::{Base, Checking, HexFile, Rebind, Repaint, Saved};

use super::browser::ops::dialog::{self, Button, Ink, Inputs, Part};
use super::cells::{self, Buffer, Cells, Grid, Pen, Position};
use super::chrome::{self, HEX_KEYS};
use super::image::{AT_A_REVISION, CANNOT_REPLACE};
use super::save_as::{self, SaveAs};
use super::{Hooks, Outcome, Status};
use crate::journal::{self, Journal, Keep, Schedule};
use crate::remote::{patch, Fingerprint, HASH_CHECK_MAX};
use crate::stream::{Bytes, Mark, ReadError, Reader, Source, Waiting};
use anyhow::{anyhow, bail, Result};
use eframe::egui::{self, Key, Rect, Sense, Vec2};
use file::{LeftTemp as LeftTempRef, Request, Row, Saving, Streamed, Way};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, Instant};
// What the tests beside this file name through `super::*`, as before the write side moved.
#[cfg(test)]
#[allow(unused_imports)]
use {
    super::Origin, crate::remote::WriteOutcome, anyhow::Context as _, eframe::egui::Color32,
    file::CHUNK, std::time::SystemTime,
};

/// Bytes per row.
pub const ROW: u64 = 16;
/// The page cache: pages of this many bytes, at most [`PAGES`] of them held.
const PAGE: u64 = 4096;
const PAGES: usize = 64;
/// Undo history bound (one step per typed digit or character).
const UNDO_STEPS: usize = 100_000;
/// Offsets one window's edits may change before a Save (07 §11: with the undo steps, at most
/// 16 MiB): a byte at a new offset past it is refused.
const EDITS: usize = 100_000;
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

/// The ASCII column's glyph for a byte (`None`: on its way, `·`): its code page 437 glyph
/// ([`ascii_char`]) — or, in a terminal that draws a glyph of ambiguous width in two cells
/// (`wide`), for one of width A 10 §10's stand-in: the table's (`+ - = |` for box lines, `#` for
/// blocks, `^ v < >` for arrows, `.` for `·` and the other symbols, [`super::term::narrow`]),
/// and `.` for what the table leaves — the Greek, maths and Latin-1 letters (its last row).
/// Substituted here, where the cell is known to be a CP437 glyph: the terminal's printer lays a
/// letter of width A in two cells, as it lays a name's, and would move the rest of the row.
pub fn column_glyph(b: Option<u8>, wide: bool) -> char {
    let glyph = b.map_or('·', chrome::cp437);
    if !wide || !super::term::ambiguous(glyph) {
        return glyph;
    }
    let mut utf8 = [0u8; 4];
    let stand_in = super::term::narrow(glyph.encode_utf8(&mut utf8));
    stand_in.and_then(|s| s.chars().next()).unwrap_or('.')
}

/// Where the hex column draws byte `i` of a row, in characters from the column's start.
fn hex_col(i: u64) -> u64 {
    3 * i + u64::from(i >= 8)
}

/// Whether a row of `per` bytes — the offset, the hex column, the ASCII column — fits in `cols`
/// cells.
fn row_fits(cols: u16, offset_chars: u16, per: u64) -> bool {
    u64::from(offset_chars) + 1 + per * 3 + 2 + per <= u64::from(cols)
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
    /// Offsets the edits may hold ([`EDITS`]).
    edit_cap: usize,
    /// The last undo or redo was refused: it would have passed `edit_cap` (taken by the window,
    /// which says so).
    over_cap: bool,
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
    /// The evidence of the edits (07 §3 `seen`): each edited 4 KiB page's SHA-256 as shown —
    /// the file's bytes there, before any edit — taken when the page was first edited.
    seen: BTreeMap<u64, journal::Sha>,
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
            edit_cap: EDITS,
            over_cap: false,
            open_step: None,
            rev: 0,
            saved: BTreeMap::new(),
            copied: None,
            counted: None,
            seen: BTreeMap::new(),
            generation: 0,
            answers: None,
        }
    }

    /// A smaller undo bound (tests reach it without a hundred thousand edits).
    #[cfg(test)]
    pub(crate) fn set_undo_cap(&mut self, cap: usize) {
        self.undo_cap = cap;
    }

    /// A smaller edit bound (tests reach it without a hundred thousand edits).
    #[cfg(test)]
    pub(crate) fn set_edit_cap(&mut self, cap: usize) {
        self.edit_cap = cap;
    }

    /// Whether an edit at `at` is past 07 §11's bound: a new offset while the edits hold
    /// [`EDITS`] of them already. A byte already edited can always change.
    pub fn full_at(&self, at: u64) -> bool {
        !self.patches.contains_key(&at) && self.patches.len() >= self.edit_cap
    }

    /// Whether putting `value` at `at` would pass the bound: it adds an entry (a new offset, and
    /// not one [`HexDoc::put`] leaves out) while the edits are full.
    fn over_cap_with(&self, at: u64, value: u8) -> bool {
        self.full_at(at) && !self.is_baseline(at, value)
    }

    /// `value` is the byte the edits are laid over at `at` (`replaced`: what the first edit
    /// there replaced, or what a streamed Save wrote there since), and the page of `at` is here
    /// to show it: an entry for it changes nothing shown, and a Save writes that byte there
    /// anyway. Where the page is not here (a stream's, dropped from the cache) the entry is what
    /// shows the byte without reading the file again (06 §7.1).
    fn is_baseline(&self, at: u64, value: u8) -> bool {
        let here = self.pages.in_place || self.pages.held.contains_key(&(at / PAGE));
        here && self.replaced.get(&at) == Some(&value)
    }

    /// `value` at `at`, as an entry of the edits — none where it is the file's own byte
    /// ([`HexDoc::is_baseline`]): such an entry would only count against the bound.
    fn put(&mut self, at: u64, value: u8) {
        if self.is_baseline(at, value) {
            self.patches.remove(&at);
        } else {
            self.patches.insert(at, value);
        }
    }

    /// Whether the last undo or redo was refused for the bound (once).
    pub fn take_over_cap(&mut self) -> bool {
        std::mem::take(&mut self.over_cap)
    }

    /// Undo steps held now.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Offsets whose file byte is kept, and edited pages whose evidence is (tests: the bound).
    #[cfg(test)]
    pub(crate) fn kept_len(&self) -> (usize, usize) {
        (self.replaced.len(), self.seen.len())
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

    /// The bytes that differ from the file's, offset → (byte wanted, byte replaced): what a Save
    /// writes and the journal keeps.
    ///
    /// The byte replaced is the one the file holds now: what the last Save in it wrote there
    /// (a file with a local base), else the file's byte as read.
    pub fn change_pairs(&self) -> BTreeMap<u64, (u8, u8)> {
        self.patches
            .iter()
            .filter_map(
                |(&at, &b)| match self.saved.get(&at).or_else(|| self.replaced.get(&at)) {
                    Some(&old) if old == b => None,
                    Some(&old) => Some((at, (b, old))),
                    None => None,
                },
            )
            .collect()
    }

    /// The bytes that differ from the file's, offset → byte wanted.
    pub fn changes(&self) -> BTreeMap<u64, u8> {
        self.change_pairs()
            .into_iter()
            .map(|(at, (b, _))| (at, b))
            .collect()
    }

    /// The evidence of `changes`: the `seen` hash of every page they touch.
    pub fn evidence(&self, changes: &BTreeMap<u64, u8>) -> BTreeMap<u64, journal::Sha> {
        let mut out = BTreeMap::new();
        for at in changes.keys() {
            let page = at / PAGE * PAGE;
            if let Some(sha) = self.seen.get(&page) {
                out.insert(page, *sha);
            }
        }
        out
    }

    /// Every page's evidence so far (07 §3 `seen`).
    pub fn seen(&self) -> &BTreeMap<u64, journal::Sha> {
        &self.seen
    }

    /// The page of `at` is being edited for the first time: its evidence is the SHA-256 of its
    /// bytes as shown — the file's, with what the last Save in it wrote over the bytes read.
    fn see(&mut self, at: u64) {
        let page = at / PAGE;
        if self.seen.contains_key(&(page * PAGE)) {
            return;
        }
        // A local file read in place gives its page now; a stream's is here whenever an edit
        // can be made on it.
        if self.pages.in_place && !self.pages.held.contains_key(&page) {
            let _ = self.pages.cell(at);
        }
        let Some((bytes, _)) = self.pages.held.get(&page) else {
            return;
        };
        let mut shown = bytes.clone();
        let start = page * PAGE;
        for (&off, &b) in self.saved.range(start..start + shown.len() as u64) {
            shown[(off - start) as usize] = b;
        }
        self.seen.insert(start, journal::sha256(&shown));
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
        self.seen.clear();
        self.open_step = None;
        self.bump();
    }

    /// A Save of a streamed file landed (07 §4.2 step 11): each byte it wrote is now the byte the
    /// file holds there, so the saved edits drop out without anything being read, and each page
    /// it wrote has the hash it was saved with as its evidence. Every undo and redo step stays (an
    /// undo now changes the saved file). What the last Save copy holds is forgotten, as after a
    /// rebase. The view reads the saved version from the next frame ([`HexDoc::refresh`]).
    pub(super) fn rebase_saved(
        &mut self,
        wrote: &BTreeMap<u64, u8>,
        seen: &BTreeMap<u64, journal::Sha>,
    ) {
        self.replaced.extend(wrote.iter().map(|(&at, &b)| (at, b)));
        let replaced = &self.replaced;
        self.patches
            .retain(|at, b| replaced.get(at).is_some_and(|old| old != b));
        self.seen.extend(seen.iter().map(|(&p, &sha)| (p, sha)));
        self.saved.clear();
        self.copied = None;
        self.prune();
        self.bump();
    }

    /// What the edits keep about offsets no edit, saved byte, copied byte or undo or redo step
    /// names any more is let go: 07 §11's bound holds across Saves, not only before the first.
    fn prune(&mut self) {
        let mut named: HashSet<u64> = self.patches.keys().copied().collect();
        named.extend(self.saved.keys());
        if let Some(copied) = &self.copied {
            named.extend(copied.keys());
        }
        named.extend(self.undo.iter().map(|s| s.at));
        named.extend(self.redo.iter().map(|s| s.at));
        self.replaced.retain(|at, _| named.contains(at));
        let pages: HashSet<u64> = named.iter().map(|at| at / PAGE * PAGE).collect();
        self.seen.retain(|page, _| pages.contains(page));
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
        if self.full_at(at) {
            return None;
        }
        let before = self.byte(at)?;
        if before == value {
            return Some(false);
        }
        // The first edit here: `before` is the file's byte, kept from now on, and its page as
        // shown is the edit's evidence.
        if !self.patches.contains_key(&at) {
            self.replaced.insert(at, before);
            self.see(at);
        }
        self.put(at, value);
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
        // An offset nothing names any more (its undo and redo steps gone, its entry back at the
        // file's byte) is let go once what is kept is twice what is named: 07 §11's bound holds
        // for the kept bytes and their evidence too, at a cost spread over the edits.
        if self.replaced.len() > 2 * self.named_len() + 64 {
            self.prune();
        }
        self.bump();
        Some(true)
    }

    /// How many offsets the edits, saved and copied bytes and undo and redo steps can name (at
    /// most; one offset may be named twice).
    fn named_len(&self) -> usize {
        self.patches.len()
            + self.saved.len()
            + self.copied.as_ref().map_or(0, BTreeMap::len)
            + self.undo.len()
            + self.redo.len()
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
        self.put(at, value);
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

    /// Undoes the last change; returns where the cursor goes. `None` also when it would pass
    /// 07 §11's bound ([`HexDoc::take_over_cap`]): the step stays where it is.
    pub fn undo(&mut self) -> Option<Cursor> {
        self.open_step = None;
        let step = *self.undo.back()?;
        if self.over_cap_with(step.at, step.before) {
            self.over_cap = true;
            return None;
        }
        self.undo.pop_back();
        // A page whose evidence went with a rebase (a Save in place) gets it again.
        self.see(step.at);
        self.put(step.at, step.before);
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
        let step = *self.redo.last()?;
        if self.over_cap_with(step.at, step.after) {
            self.over_cap = true;
            return None;
        }
        self.redo.pop();
        self.see(step.at);
        self.put(step.at, step.after);
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
        // The evidence is of the file read now: each page still edited, as it is in it.
        self.seen.clear();
        let edited: Vec<u64> = self.patches.keys().copied().collect();
        for at in edited {
            let _ = self.pages.cell(at);
            self.see(at);
        }
        self.prune();
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
    window.save_as_hook = save_as_hook();
    window.scripted = hooks.screenshot.is_some();
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
    window.save_as_hook = save_as_hook();
    window.scripted = hooks.screenshot.is_some();
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

/// The journal snapshot of `doc`'s edits against `baseline` (07 §3): the changed runs with the
/// bytes they replaced, and each edited page's evidence.
fn snapshot_of(doc: &HexDoc, baseline: Fingerprint) -> journal::Snapshot {
    let pairs = doc.change_pairs();
    let changes: BTreeMap<u64, u8> = pairs.iter().map(|(&at, &(b, _))| (at, b)).collect();
    journal::Snapshot {
        baseline,
        seen: journal::Seen::pages(doc.evidence(&changes)),
        edits: file::edits_text(&pairs),
        pastes: Vec::new(),
    }
}

/// `SOPEN_SAVE_AS=<folder>` (a receipt hook, read here, not in `State`): the hex window saves
/// its edits as `<folder>/<stem>.edited<ext>` — without a window under `SOPEN_AUTOSAVE`, else
/// through the Save As modal once the state is on screen.
fn save_as_hook() -> Option<PathBuf> {
    std::env::var_os("SOPEN_SAVE_AS")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `SOPEN_SAVE_AS_HOLD_MS=<ms>`: the hex window's Save As holds at its first progress at or
/// past 30 % for that long, so a screenshot shows it under way.
fn save_as_hold() -> Option<Duration> {
    std::env::var("SOPEN_SAVE_AS_HOLD_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_millis)
}

fn autosave(mut file: HexFile, doc: &mut HexDoc, hooks: &Hooks) -> Result<Outcome> {
    let mut outcome = Outcome::default();
    if let Some(folder) = save_as_hook() {
        return autosave_as(file, doc, &folder);
    }
    if !doc.dirty() {
        println!("no changes; {} untouched", file.origin.display());
        return Ok(outcome);
    }
    if let Some(delay) = hooks.save_delay {
        eprintln!("waiting {} ms before saving", delay.as_millis());
        std::thread::sleep(delay);
    }
    if file.stream().is_some() {
        let changes = doc.changes();
        let req = Request {
            way: Way::Save,
            snap: snapshot_of(doc, file.opened()),
            changes,
            session: 0,
        };
        let slot = Mutex::new(None);
        let saving = Saving::default();
        let (host, abs, _) = file.place();
        let result = file.save_streamed(&req, &slot, &saving, &|| {});
        // A scripted run's edits are its state's: nothing of them is kept — unless a request
        // got no reply, when the journal's record is what tells later whether it landed.
        let journal = slot.into_inner().unwrap_or_else(|p| p.into_inner());
        let unknown = matches!(&result, Err(e) if crate::remote::is_outcome_unknown(e));
        match journal {
            Some(journal) if unknown => {
                let kept = super::binary::tilde(journal.dir());
                return Err(result
                    .expect_err("checked above")
                    .context(format!("its record is kept at {kept}")));
            }
            Some(journal) => {
                if let Err(e) = journal.remove() {
                    eprintln!("sopen: the kept edits could not be removed: {e:#}");
                }
            }
            None => {}
        }
        match result? {
            Streamed::Saved { saved, notes } => {
                outcome.saved = true;
                println!("{}", saved_line(&saved, &host, &abs));
                for note in notes {
                    eprintln!("sopen: {note}");
                }
            }
            Streamed::Refused { why, notes, .. } => {
                outcome.conflict = true;
                let mut message = format!("not saved: {why}");
                for note in notes {
                    message.push_str(&format!("; {note}"));
                }
                outcome.refused = Some(message);
            }
            Streamed::Ask(left) => {
                outcome.conflict = true;
                outcome.refused = Some(format!(
                    "not saved: an earlier save was cut off — {}",
                    left.line
                ));
            }
            Streamed::Copied { .. } | Streamed::FoundLanded { .. } => {
                bail!("{} was not saved", file.origin.display())
            }
        }
        return Ok(outcome);
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

/// `SOPEN_AUTOSAVE` with `SOPEN_SAVE_AS`: the edits saved as a new local file in `folder`
/// without a window (07 §7), its progress on stderr, `saved <path>` on stdout.
fn autosave_as(file: HexFile, doc: &mut HexDoc, folder: &Path) -> Result<Outcome> {
    let size = file.size();
    let name = file.name();
    let baseline = file.opened();
    let changes = doc.changes();
    let seen = doc.evidence(&changes);
    let file = Arc::new(Mutex::new(file));
    let mut producer = file::HexProducer::new(file, changes, seen, baseline);
    producer.hold = save_as_hold();
    let (dest, _) = save_as::first_path(folder, &name);
    let decision = super::binary::Decision::new();
    let began = Instant::now();
    let last = Mutex::new(0u64);
    let report = |done: u64| {
        let mut last = last.lock().unwrap_or_else(|p| p.into_inner());
        if done == size || done >= *last + size / 20 {
            *last = done;
            eprintln!(
                "saving as {}: {}",
                dest.display(),
                save_as::progress_line(done, save_as::Size::Exact(size), began.elapsed(), None)
            );
        }
    };
    let ctl = save_as::Ctl::new(&decision, &report);
    let written = save_as::write(&dest, save_as::Size::Exact(size), &mut producer, &ctl);
    let mut outcome = Outcome::default();
    match written {
        save_as::Outcome::Saved { path, notes, .. } => {
            println!("saved {}", path.display());
            for note in notes {
                eprintln!("sopen: {note}");
            }
            outcome.saved = true;
        }
        other => {
            outcome.refused = Some(other.line());
        }
    }
    Ok(outcome)
}

/// `saved host:/abs · backup <undo file>`, and above 256 MiB what was checked.
fn saved_line(saved: &patch::Saved, host: &str, abs: &str) -> String {
    let mut line = saved.line(host, abs);
    if let Some(checked) = &saved.checked {
        line.push_str(&format!(" · {checked}"));
    }
    line
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
        Ok(JobDone::Streamed { line, done }) => match done {
            Streamed::Saved { notes, .. } => {
                outcome.saved = true;
                outcome.refused = None;
                println!("{line}");
                for note in notes {
                    eprintln!("sopen: {note}");
                }
                (Status::info(line.clone()), notes.clone())
            }
            Streamed::Copied { path, notes, .. } => {
                println!("saved a copy at {path}");
                for note in notes {
                    eprintln!("sopen: {note}");
                }
                (Status::info(line.clone()), notes.clone())
            }
            Streamed::Refused { why, notes, .. } => {
                outcome.conflict = true;
                outcome.refused = Some(why.clone());
                eprintln!("sopen: not saved: {why}");
                let mut shown = vec![format!("not saved: {why}")];
                shown.extend(notes.iter().cloned());
                (Status::error("not saved"), shown)
            }
            Streamed::FoundLanded { .. } => {
                outcome.saved = true;
                println!("{line}");
                (Status::info(line.clone()), Vec::new())
            }
            // The question is asked in its own row (`HexWindow::left_row`).
            Streamed::Ask(_) => {
                eprintln!("sopen: {line}");
                (Status::error(line.clone()), Vec::new())
            }
        },
        // A streamed Save stopped by Esc between its requests: its temp is gone (07 §4.2).
        Err(e) if stopped(e) => (Status::info(STOPPED), Vec::new()),
        Err(e) => {
            eprintln!("sopen: {e:#}");
            let status = if crate::remote::is_outcome_unknown(e) {
                "save cut off: outcome unknown"
            } else {
                "save failed"
            };
            (Status::error(status), vec![format!("{e:#}")])
        }
    }
}

/// A Save stopped before it changed the target wrote nothing there.
const STOPPED: &str = "stopped: nothing was written";

/// Whether a save ended because it was stopped (Esc).
fn stopped(e: &anyhow::Error) -> bool {
    crate::remote::is_cancelled(e)
}

/// The status row while a Save runs and an edit was tried (07 §10).
const EDITING_RESUMES: &str = "saving — editing resumes when the save is done";

/// The status row when an edit at a new offset is past 07 §11's bound.
const EDITS_FULL: &str = "too many changes for one save — save first";

/// A reload is on its way: nothing is edited or saved until it has landed.
const RELOADING: &str = "reloading…";
/// How long the scroll bar's thumb rests on a place before it is fetched, in seconds (06 §7.1).
const THUMB_REST: f64 = 0.1;

/// Closing was asked for while a save runs.
const CLOSES_AFTER_SAVE: &str = "the window closes when the save is done";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Save {
        close_after: bool,
    },
    SaveCopy {
        close_after: bool,
    },
    /// Write in place (07 §4.4): only from its dialog, after a refusal that offers it.
    InPlace,
}

/// A generation's write ([`HexWindow::generation`]): the journal it kept the edits in, `None`
/// when it found the journal's epoch moved and wrote nothing.
type GenerationWork = Box<dyn FnOnce() -> std::result::Result<Option<PathBuf>, String> + Send>;

/// A background generation's end: the revision it holds, and the journal it kept them in —
/// `Ok(None)` when it found the journal moved on (a Save started, or the journal retired) and
/// wrote nothing.
type Generated = (u64, std::result::Result<Option<PathBuf>, String>);

/// A removal the owner asked for: the journal whose question it answers, and how it went.
type Removed = (Option<PathBuf>, std::result::Result<String, String>);

/// What a recovery row's button asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RowAction {
    Restore,
    Later,
    Discard,
    /// Load as edits (an undo file).
    Load,
    /// Remove a file at an interrupted Save's temp path.
    Remove,
    /// Keep that file: the question ends.
    Keep,
}

pub(super) struct Job {
    kind: JobKind,
    /// The edits when the save began (absolute bytes, as the doc holds them).
    snapshot: BTreeMap<u64, u8>,
    /// The document revision a streamed Save's worker kept in the journal at its start.
    kept: Option<u64>,
    /// The bytes it writes, offset → byte wanted (what the file holds if it lands).
    changes: BTreeMap<u64, u8>,
    rx: Receiver<Result<JobDone>>,
}

enum JobDone {
    Saved(Saved),
    /// The copy's printed path.
    Copy(String),
    /// A streamed file's Save, Save copy or Write in place (07 §4), with its status line.
    Streamed {
        line: String,
        done: Streamed,
    },
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
    /// The write side. A Save's worker holds it for as long as it runs, so a frame never waits
    /// for it: what the frames need of the file is held here as well (`home`, `baseline`).
    file: Arc<Mutex<HexFile>>,
    /// Where this file's journals and undo files are: `(host, abs, cache)`.
    home: (String, String, PathBuf),
    /// The baseline the edits are against (07 §3 `head`): the version as opened, then each
    /// landed Save's next baseline.
    baseline: Fingerprint,
    /// Cmd/Ctrl+R reloads so far: a version the owner reloaded gets an undo file of its own.
    session: u64,
    /// A streamed file's baseline pass (07 §4.5), read without the file's lock.
    checking: Arc<Checking>,
    /// A streamed Save's progress and cancel, read without the file's lock.
    saving: Arc<Saving>,
    /// The session's journal (07 §3): made at the first generation, shared with a Save's worker,
    /// which records the save intent in it; retired once nothing is unsaved.
    journal: Arc<Mutex<Option<Journal>>>,
    /// Bumped when a Save starts and when the journal is retired: a generation begun before
    /// that finds it moved when it takes the journal, and writes nothing — it can never bring a
    /// retired journal back with edits a Save has written since.
    journal_epoch: Arc<AtomicU64>,
    /// Where the journals are written (the file's: tests count it).
    journal_fs: Arc<dyn journal::Fs>,
    /// Tests: a background generation waits here before it takes the journal; and what runs
    /// when a Save's start waits for one, or the window's end for an answer to its question.
    #[cfg(test)]
    pub(super) generation_gate: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(super) before_drain: Option<Box<dyn FnMut() + Send>>,
    /// When the next generation is due, and whether the latest edit is kept.
    schedule: Schedule,
    /// The document revision last given to the schedule.
    journal_rev: u64,
    /// A generation being written in the background: the revision it holds, and its result.
    writing: Option<Receiver<Generated>>,
    /// The journal's folder once a generation of it is kept (`kept at <journal>`).
    journal_dir: Option<PathBuf>,
    /// Discard was chosen: the journal goes with the window, unwritten.
    discard: bool,
    /// The window had the keyboard focus at the last pass (losing it writes a generation).
    focused: bool,
    /// The first Save of a file above 256 MiB asks once (07 §4.5): the Save waiting for the
    /// answer, and whether it was given.
    confirm: Option<JobKind>,
    confirmed: bool,
    /// An edit was tried while a Save ran: the status row says editing resumes after it.
    edit_blocked: bool,
    /// A refusal whose row offers Write in place… and Save As… (07 §4.1).
    offer: Option<String>,
    /// A cut-off Save of this session left a file at its temp path that sopen cannot prove its
    /// own (07 §3.1): the question the next Save found, until the owner answers it — and the
    /// answer on its way.
    left: Option<LeftTempRef>,
    answering: Option<Receiver<std::result::Result<String, String>>>,
    /// Write in place's dialog (07 §4.4) is up, with its words.
    in_place_ask: Option<String>,
    /// The Save As modal (07 §7): the modal, whether the window closes once it lands, and the
    /// edits it writes.
    save_as: Option<(SaveAs, bool, BTreeMap<u64, u8>)>,
    /// Where the Save As field starts: the download folder (tests and `SOPEN_SAVE_AS`: their
    /// own), and the system's save dialog.
    pub(super) save_as_folder: Option<PathBuf>,
    #[cfg(test)]
    pub(super) chooser: Option<Box<dyn Fn() -> Box<dyn super::binary::Chooser>>>,
    /// `SOPEN_SAVE_AS` (a receipt): Save As starts once the state is on screen; then whether
    /// its fill has reached its hold.
    save_as_hook: Option<PathBuf>,
    save_as_held: Arc<AtomicBool>,
    /// Tests: the Save As fill holds this long at 30 % (as `SOPEN_SAVE_AS_HOLD_MS` does), or
    /// until `test_release` is set; and the rate the modal says its time at.
    #[cfg(test)]
    pub(super) test_hold: Option<Duration>,
    #[cfg(test)]
    pub(super) test_release: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    pub(super) test_rate: Option<f64>,
    /// A Save of this session whose outcome was unknown: the bytes it was writing (found landed
    /// by the next Save's reconciliation, they are the file's).
    cut_off: Option<BTreeMap<u64, u8>>,
    /// The recovery rows (07 §3, §5.1), and their listing while it runs.
    rows: Vec<Row>,
    recovering: Option<Receiver<Vec<Row>>>,
    recovery_started: bool,
    /// The baseline pass read other bytes under an edited page than the window showed, as of
    /// this document revision.
    under_edits: bool,
    pass_rev: Option<u64>,
    /// A scripted run (`SOPEN_SCREENSHOT`): its edits are the state's, and its journal goes
    /// with the window.
    scripted: bool,
    /// A file at an interrupted Save's temp path being removed, after the owner's answer: the
    /// journal whose question it answers, and how it went.
    removing: Option<Receiver<Removed>>,
    /// A Restore whose bytes are landing: how many it puts back.
    restoring: Option<usize>,
    /// Tests: what a crash leaves (the journal is dropped unwritten, its lock freed).
    #[cfg(test)]
    pub(super) crashed: bool,
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
    /// A file at a revision ([`super::Window::set_read_only`]): a viewer only — nothing is
    /// edited or saved, no journal is written and none of an earlier session's is looked for
    /// (that look may reconcile a cut-off Save on the file).
    at_revision: bool,
    /// The Hex view of the file browser's viewer (08 D35, [`super::Window::set_in_viewer`]): it
    /// opens viewing — typed text is not read, so a view key is never a byte — and `editing`
    /// from Enter or Insert until Esc or F9 (the view keys' commands). A window on its own types
    /// at once, as ever.
    in_viewer: bool,
    editing: bool,
    /// Saves that wrote the file so far (a Save, Write in place, a cut-off Save found landed):
    /// the browser's view set tells its hidden views they are stale by it (08 D25), whatever
    /// the window does after.
    saves_landed: u64,
    /// Saves started so far (tests: a key starts exactly one).
    #[cfg(test)]
    pub(super) save_starts: usize,
    /// Where the window's questions stand (10 §6): its rect less its own F-key rows — hosted,
    /// the browser's area between its title and status rows. Set at each pass.
    area: Rect,
    /// A control of the rows below the grid had the keyboard at the end of the last pass: the
    /// view keys' Question, as a dialog's (every key is the rows').
    row_focus: bool,
    /// What the questions of the last pass covered, their boxes and shadows (points), and of
    /// this one so far: the window's cells under them are blanked before they are painted
    /// ([`HexWindow::blank_under`]).
    under: Vec<Rect>,
    covering: Vec<Rect>,
    /// The context of the last pass (the window's or the terminal's): what a background
    /// generation's end repaints.
    wake: Option<egui::Context>,
    /// Enter or Space that pressed a control of the rows below the grid, while it stays down
    /// ([`HexWindow::drop_held`]).
    held: Option<Key>,
}

impl Drop for HexWindow {
    fn drop(&mut self) {
        // A baseline pass still reading is of no use to anyone now.
        self.checking.cancel.store(true, Ordering::SeqCst);
        #[cfg(test)]
        if self.crashed {
            return;
        }
        let running = self.job.is_some();
        if let Some(job) = self.job.take() {
            *self.leftover.lock().unwrap() = Some(job);
        }
        // Closing (07 §3): the edits are written once more and kept, or — nothing unsaved, or
        // Discard — the journal goes. A Save still running holds the journal and settles it.
        if !running && !self.preview {
            self.settle_journal();
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

/// A control was pressed in this pass, by a click or by Enter or Space on it: those keys are
/// spent, so a question it opens in the same pass (Save As…) never takes them for its own.
fn spend_presses(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        i.events.retain(|e| {
            !matches!(
                e,
                egui::Event::Key {
                    key: Key::Enter | Key::Space,
                    pressed: true,
                    ..
                }
            ) && !matches!(e, egui::Event::Text(t) if t == " ")
        })
    });
}

fn grid_id() -> egui::Id {
    egui::Id::unique("sopen-hex-grid")
}

/// The Hex view's status row while it is viewing and while it is edited (08 §8, D35).
pub(super) const VIEWING: &str = "viewing — Enter edits";
pub(super) const EDITING: &str = "editing — Esc ends";

fn goto_id() -> egui::Id {
    egui::Id::unique("sopen-hex-goto")
}

/// The Go to field on the toolbar, in cells: where it starts (after ` Go to `), its width (the
/// egui field's 130 points), the fewest it gives up to on a narrow window, and its hint.
const GOTO_AT: u16 = 7;
const GOTO_CELLS: u16 = 16;
const GOTO_FEWEST: u16 = 8;
const GOTO_HINT: &str = "0x… or decimal";

/// Between two parts of the status row.
const RULE: &str = " │ ";

/// The one-cell spinner while a save runs (10 §6), a quarter turn every tenth of a second.
const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

/// What a toolbar button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Does {
    Undo,
    Redo,
    Save,
    SaveCopy,
    SaveAs,
}

impl Does {
    fn label(self) -> &'static str {
        match self {
            Does::Undo => "Undo",
            Does::Redo => "Redo",
            Does::Save => "Save",
            Does::SaveCopy => "Save copy",
            Does::SaveAs => "Save As…",
        }
    }
}

/// A toolbar button: what it does, whether it does it now, and what it says on hover then
/// (`tip`) and while it does not (`off`; empty: nothing).
struct Tool {
    does: Does,
    on: bool,
    tip: &'static str,
    off: &'static str,
}

impl Tool {
    /// Its cells: `[ label ]`.
    fn cells(&self) -> u16 {
        cells::width(self.does.label()) as u16 + 4
    }
}

fn tool_id(does: Does) -> egui::Id {
    egui::Id::unique("sopen-hex-tool").with(does)
}

/// A row one cell tall across `ui`, on the cell grid: its grid, and its cells in the body's
/// colours.
fn cell_row(ui: &mut egui::Ui) -> (Grid, Buffer) {
    let width = ui.available_width();
    let size = Vec2::new(width, chrome::CELL.y);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let grid = Grid::over(rect);
    let mut buf = Buffer::empty(grid.all());
    let p = chrome::PALETTE;
    cells::fill(&mut buf, grid.all(), Pen::new(p.text, p.back));
    (grid, buf)
}

/// F1's help over the window: the cells of `buf` (on `grid`) under its box blanked first — one
/// buffer of cells laid over another (cells.rs) — so a wide glyph half under its edge (a Hangul
/// name) goes as a terminal erases it.
fn blank_under_help(ctx: &egui::Context, grid: &Grid, buf: &mut Buffer) {
    let p = chrome::PALETTE;
    for under in super::help::covered(ctx, grid) {
        cells::fill(buf, under, Pen::new(p.text, p.back));
    }
}

/// The cells of `on` that `r` (points, on the same 8 × 16 lattice) stands on, cut to it.
fn cells_on(on: &Grid, r: Rect) -> Cells {
    let at = |v: f32, cell: f32, n: u16| (v / cell).round().clamp(0.0, f32::from(n)) as u16;
    let (from, to) = (r.min - on.origin, r.max - on.origin);
    let (x0, y0) = (
        at(from.x, chrome::CELL.x, on.cols),
        at(from.y, chrome::CELL.y, on.rows),
    );
    let (x1, y1) = (
        at(to.x, chrome::CELL.x, on.cols),
        at(to.y, chrome::CELL.y, on.rows),
    );
    Cells::new(x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
}

/// A stand-in run before each link of writing the list (tests).
#[cfg(test)]
type KeepLink = Box<dyn FnMut(super::binary::LinkStep, &Path) -> std::io::Result<()> + Send>;

/// The read-only window's "Write the list there" path field.
fn keep_id() -> egui::Id {
    egui::Id::unique("sopen-hex-keep-to")
}

/// The fewest cells the keep row's path field has.
const KEEP_FEWEST: u16 = 12;

/// The questions' nominal widths in cells, the egui dialogs' (420, 440 and 460 points).
const PROMPT_WIDE: usize = 52;
const CONFIRM_WIDE: usize = 55;
const IN_PLACE_WIDE: usize = 57;

fn prompt_id() -> egui::Id {
    egui::Id::unique("sopen-hex-unsaved")
}

fn confirm_id() -> egui::Id {
    egui::Id::unique("sopen-hex-confirm")
}

fn in_place_id() -> egui::Id {
    egui::Id::unique("sopen-hex-in-place")
}

/// What a control of the rows below the grid does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Press {
    /// "Keep edits…": the list of the edits among sopen's kept edits.
    KeepEdits,
    /// "Write the list there": the list at the keep row's path.
    WriteList,
    /// A refusal's Write in place… (its dialog) and Save As… (07 §4.1).
    InPlace,
    OfferSaveAs,
    /// The live question about a temp (07 §3.1): Remove it (`true`) or Keep it.
    Left(bool),
    /// A recovery row's button, the row by its identity ([`row_key`]), never its place: the row
    /// that moves up when the one above goes never takes that one's controls or keyboard.
    Row(u64, RowAction),
}

/// A control of the rows below the grid: its label, what it does, whether it does it now.
struct Control {
    label: &'static str,
    press: Press,
    on: bool,
}

/// One row of cells below the grid ([`HexWindow::below`]), laid out for its width: what it
/// draws is all of what acts there, and what acts is a Tab stop ([`HexWindow::below_controls`]).
enum Below {
    /// A text, wrapped already, in its pen.
    Text(String, Pen),
    /// Buttons at their first cells, and a `│` at its cell (between a question's answers and a
    /// recovery row's own buttons, on one row of cells).
    Buttons(Vec<(u16, Control)>, Option<u16>),
    /// The read-only window's: `[ Keep edits… ]` and its path field, `field` cells wide, then
    /// `[ Write the list there ]` — on this row (`write`), or alone on the next
    /// ([`Below::KeepWrite`]) where this one has no room for it. Where not even a cell of the
    /// field fits beside the button (`field` 0), the field is a row of its own first
    /// ([`Below::KeepField`]).
    Keep {
        field: u16,
        write: bool,
    },
    KeepField(u16),
    KeepWrite,
}

fn below_id(press: Press) -> egui::Id {
    egui::Id::unique("sopen-hex-below").with(press)
}

/// A button's cells: `[ label ]`.
fn button_cells(label: &str) -> u16 {
    cells::width(label) as u16 + 4
}

/// Whether a button at cell `x` of a row `cols` wide shows a cell (it is cut at the row's end):
/// only one that shows is drawn, acts, and is a Tab stop.
fn shows(x: u16, cols: u16) -> bool {
    x < cols
}

/// `controls` on rows `cols` cells wide, from cell 1, two blanks apart — a `│` with a blank
/// either side before the one at `rule` — and a button that does not fit after another on the
/// next row (a rule at a row's start is left out). One wider than the row stands alone, cut.
fn placed(controls: Vec<Control>, rule: Option<usize>, cols: u16) -> Vec<Below> {
    let (mut out, mut row, mut bar, mut x) = (vec![], vec![], None, 1u16);
    for (k, c) in controls.into_iter().enumerate() {
        let w = button_cells(c.label);
        let ruled = Some(k) == rule;
        let need = w + if ruled { 3 } else { 0 };
        if !row.is_empty() && x.saturating_add(need) > cols {
            out.push(Below::Buttons(std::mem::take(&mut row), bar.take()));
            x = 1;
        }
        if ruled && !row.is_empty() {
            bar = Some(x);
            x += 3;
        }
        row.push((x, c));
        x = x.saturating_add(w + 2);
    }
    if !row.is_empty() {
        out.push(Below::Buttons(row, bar));
    }
    out
}

/// A recovery row's identity: its journal's folder, its temp's path, its undo file's or its
/// note's line — what its controls are keyed on ([`Press::Row`]).
fn row_key(row: &Row) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match row {
        Row::Edits { orphan, .. } | Row::Kept { orphan, .. } => {
            ("journal", orphan.dir()).hash(&mut h)
        }
        Row::Undo { line, .. } => ("undo", line).hash(&mut h),
        Row::Temp { path, .. } => ("temp", path).hash(&mut h),
        Row::Note(line) => ("note", line).hash(&mut h),
    }
    h.finish()
}

/// The recovery rows as their controls see them: each one's identity and whether it asks about
/// a temp. A press that changes it gives the keyboard back to the grid.
fn rows_seen(rows: &[Row]) -> Vec<(u64, bool)> {
    let asks = |row: &Row| match row {
        Row::Edits { temp, .. } | Row::Kept { temp, .. } => temp.is_some(),
        _ => false,
    };
    rows.iter().map(|r| (row_key(r), asks(r))).collect()
}

/// Whether a row below the grid holds `[ Write the list there ]`: the keep row where it has room,
/// or the row of its own after it. `None`: not the keep row's.
fn row_write(row: &Below) -> Option<bool> {
    match row {
        Below::Keep { write, .. } => Some(*write),
        Below::KeepWrite => Some(true),
        _ => None,
    }
}

/// A journal's question about a temp, and a temp's row: Remove it, Keep it.
const ASK: [(&str, RowAction); 2] = [
    ("Remove it", RowAction::Remove),
    ("Keep it", RowAction::Keep),
];

/// A recovery row's line, its journal's question about a temp, and its own buttons.
fn recovery_row(
    row: &Row,
) -> (
    &str,
    Option<&LeftTempRef>,
    &'static [(&'static str, RowAction)],
) {
    match row {
        Row::Edits {
            line, check, temp, ..
        } => (
            line,
            temp.as_ref(),
            match check {
                journal::Restore::Apply { .. } => &[
                    ("Restore", RowAction::Restore),
                    ("Later", RowAction::Later),
                    ("Discard", RowAction::Discard),
                ],
                journal::Restore::Ask => &[
                    ("Restore anyway", RowAction::Restore),
                    ("Later", RowAction::Later),
                    ("Discard", RowAction::Discard),
                ],
                _ => &[("Later", RowAction::Later), ("Discard", RowAction::Discard)],
            },
        ),
        Row::Kept { line, temp, .. } => (
            line,
            temp.as_ref(),
            &[("Later", RowAction::Later), ("Discard", RowAction::Discard)],
        ),
        Row::Undo { line, .. } => (
            line,
            None,
            &[
                ("Load as edits", RowAction::Load),
                ("Later", RowAction::Later),
            ],
        ),
        Row::Temp { line, .. } => (line, None, &ASK),
        Row::Note(line) => (line, None, &[("OK", RowAction::Later)]),
    }
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
        let (home, baseline, checking) = (file.place(), file.opened(), file.checking.clone());
        let file_journal_fs = file.journal_fs.clone();
        Self {
            title: file.origin.display(),
            can_replace,
            doc,
            file: Arc::new(Mutex::new(file)),
            home,
            baseline,
            session: 0,
            checking,
            saving: Arc::default(),
            journal: Arc::default(),
            journal_epoch: Arc::default(),
            journal_fs: file_journal_fs,
            #[cfg(test)]
            generation_gate: None,
            #[cfg(test)]
            before_drain: None,
            schedule: Schedule::default(),
            journal_rev: 0,
            writing: None,
            journal_dir: None,
            discard: false,
            focused: true,
            confirm: None,
            confirmed: false,
            edit_blocked: false,
            offer: None,
            left: None,
            answering: None,
            in_place_ask: None,
            save_as: None,
            save_as_folder: None,
            #[cfg(test)]
            chooser: None,
            save_as_hook: None,
            save_as_held: Arc::default(),
            #[cfg(test)]
            test_hold: None,
            #[cfg(test)]
            test_release: None,
            #[cfg(test)]
            test_rate: None,
            cut_off: None,
            rows: Vec::new(),
            recovering: None,
            recovery_started: false,
            under_edits: false,
            pass_rev: None,
            scripted: false,
            removing: None,
            restoring: None,
            #[cfg(test)]
            crashed: false,
            cursor,
            in_viewer: false,
            editing: false,
            saves_landed: 0,
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
            at_revision: false,
            #[cfg(test)]
            save_starts: 0,
            area: Rect::NOTHING,
            row_focus: false,
            under: Vec::new(),
            covering: Vec::new(),
            wake: None,
            held: None,
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
    /// size). Edits and steps over the one before are gone — none was unsaved — and the
    /// baseline is the version open now: its own pass, its own undo file.
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
                if let Some(source) = &self.stream {
                    self.baseline = source.opened();
                }
                self.session += 1;
                self.under_edits = false;
                self.pass_rev = None;
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
                if self.doc.full_at(at) {
                    // Past 07 §11's bound: the rest are not put in, and the window says so.
                    let left = self.state_patches.len();
                    self.state_patches.clear();
                    self.restoring = None;
                    self.status = Status::error(format!(
                        "{EDITS_FULL}; {} not put in",
                        plural_bytes(left as u64)
                    ));
                    break;
                }
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

    /// No edit of a file at a revision (03 §6.3): the status row says why.
    fn no_edit_here(&mut self) -> bool {
        if self.at_revision {
            self.status = Status::error(format!("cannot edit: {AT_A_REVISION}"));
        }
        self.at_revision
    }

    fn undo(&mut self) {
        if self.no_edit_here() {
            return;
        }
        if let Some(cursor) = self.doc.undo() {
            self.cursor = cursor;
            self.follow = true;
            self.status = Status::info("undone");
        } else if self.doc.take_over_cap() {
            self.status = Status::error(EDITS_FULL);
        }
    }

    fn redo(&mut self) {
        if self.no_edit_here() {
            return;
        }
        if let Some(cursor) = self.doc.redo() {
            self.cursor = cursor;
            self.follow = true;
            self.status = Status::info("redone");
        } else if self.doc.take_over_cap() {
            self.status = Status::error(EDITS_FULL);
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
        if self.at_revision {
            return self.status = Status::error(format!("cannot save: {AT_A_REVISION}"));
        }
        if let Some(why) = &self.read_only {
            return self.status = Status::error(why.clone());
        }
        if self.reloading.is_some() {
            return self.status = Status::info(RELOADING);
        }
        // Restored bytes still landing are not all in yet.
        if !self.state_patches.is_empty() {
            return self.status = Status::info("your edits are still being put back");
        }
        // An earlier save's question (07 §3.1) is answered first.
        if self.left.is_some() || self.answering.is_some() {
            return self.status = Status::error(
                "first answer the question about the interrupted save's file: remove it or keep it",
            );
        }
        if matches!(kind, JobKind::Save { .. }) {
            if !self.can_replace {
                return self.status = Status::error(CANNOT_REPLACE);
            }
            if !self.doc.dirty() {
                return self.status = Status::info("no changes to save");
            }
        }
        let streamed = self.stream.is_some();
        let changes = self.doc.changes();
        if streamed && changes.is_empty() {
            return self.status = Status::info("no changes to save");
        }
        // The first Save of a file above 256 MiB asks once what its checks cannot see (07 §4.5).
        if streamed
            && matches!(kind, JobKind::Save { .. })
            && self.doc.size() > HASH_CHECK_MAX
            && !self.confirmed
        {
            self.confirm = Some(kind);
            return;
        }
        #[cfg(test)]
        {
            self.save_starts += 1;
        }
        // A generation on its way is waited for by the Save's worker before it takes the journal
        // — never by the frame, which goes on drawing whatever the journal's disk does — so the
        // Save cannot overtake it. None begun before now writes from here on: it cannot bring a
        // journal the Save retires back. (What it held is in the Save's own first write, which
        // says what is kept.)
        let behind = self.writing.take();
        #[cfg(test)]
        let mut hook = behind.as_ref().and_then(|_| self.before_drain.take());
        // Returns false when the Save was stopped (Esc) while it waited: it then sends nothing.
        let settle = move |cancel: &AtomicBool| -> bool {
            if let Some(rx) = behind {
                #[cfg(test)]
                if let Some(hook) = hook.as_mut() {
                    hook();
                }
                loop {
                    match rx.recv_timeout(Duration::from_millis(50)) {
                        Err(mpsc::RecvTimeoutError::Timeout) if cancel.load(Ordering::SeqCst) => {
                            return false
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            }
            true
        };
        self.journal_epoch.fetch_add(1, Ordering::SeqCst);
        *self
            .saving
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        self.saving.cancel.store(false, Ordering::SeqCst);
        self.saving.kept.store(false, Ordering::SeqCst);
        *self
            .saving
            .kept_why
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        self.offer = None;
        self.edit_blocked = false;
        let patches = self.doc.patches().clone();
        let snapshot = patches.clone();
        let file = self.file.clone();
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        // Before any Save starts the edits are kept (07 §3), by its worker, first: a streamed
        // Save's own worker writes them with its record before anything is sent; another's
        // writes this generation.
        let keep = (!streamed && self.wants_generation()).then(|| self.generation());
        let kept = match &keep {
            Some((rev, _)) => Some(*rev),
            None => streamed.then(|| self.schedule.taking()),
        };
        if streamed {
            let way = match kind {
                JobKind::Save { .. } => Way::Save,
                JobKind::SaveCopy { .. } => Way::Copy,
                JobKind::InPlace => Way::InPlace,
            };
            let req = Request {
                way,
                changes: changes.clone(),
                snap: snapshot_of(&self.doc, self.baseline),
                session: self.session,
            };
            let (journal, saving) = (self.journal.clone(), self.saving.clone());
            let (host, abs) = (self.home.0.clone(), self.home.1.clone());
            std::thread::spawn(move || {
                if !settle(&saving.cancel) {
                    let _ = tx.send(Err(anyhow!(crate::remote::Cancelled)));
                    ctx2.request_repaint();
                    return;
                }
                let wake = || ctx2.request_repaint();
                let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = file
                    .save_streamed(&req, &journal, &saving, &wake)
                    .map(|done| {
                        let line = match &done {
                            Streamed::Saved { saved, .. } => saved_line(saved, &host, &abs),
                            Streamed::Copied { path, checked, .. } => match checked {
                                Some(checked) => format!("saved a copy at {path} · {checked}"),
                                None => format!("saved a copy at {path}"),
                            },
                            Streamed::Refused { why, .. } => format!("not saved: {why}"),
                            Streamed::FoundLanded { line, .. } => {
                                format!("{line}: nothing was written again")
                            }
                            Streamed::Ask(_) => "not saved: an earlier save was cut off and \
                                                 left a file at its temporary path — answer \
                                                 the question below first"
                                .to_string(),
                        };
                        JobDone::Streamed { line, done }
                    });
                drop(file);
                let _ = tx.send(result);
                ctx2.request_repaint();
            });
        } else {
            let saving = self.saving.clone();
            let copy = matches!(kind, JobKind::SaveCopy { .. });
            std::thread::spawn(move || {
                // Esc does not stop a Save of a local file: this waits for the generation.
                settle(&saving.cancel);
                if let Some((_, work)) = keep {
                    match work() {
                        Ok(Some(_)) => saving.kept.store(true, Ordering::SeqCst),
                        Ok(None) => {}
                        Err(why) => {
                            *saving.kept_why.lock().unwrap_or_else(|p| p.into_inner()) = Some(why)
                        }
                    }
                }
                // A save whose worker died holding the file leaves it as it was: usable.
                let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = if copy {
                    file.save_copy(&patches).map(JobDone::Copy)
                } else {
                    file.save(&patches).map(JobDone::Saved)
                };
                drop(file);
                let _ = tx.send(result);
                ctx2.request_repaint();
            });
        }
        self.status = Status::info(match kind {
            JobKind::SaveCopy { .. } => "saving a copy…",
            JobKind::InPlace => "writing in place…",
            JobKind::Save { .. } => "saving…",
        });
        self.job = Some(Job {
            kind,
            snapshot,
            kept,
            changes,
            rx,
        });
    }

    /// A finished save, taken after this frame's input. Closing — asked for in the prompt, or
    /// during the save — happens only when the save landed with nothing to read and nothing is
    /// unsaved; a save that did not land, or landed with notes, keeps the window open.
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
        self.edit_blocked = false;
        if let Some(rev) = job.kept {
            // What the worker kept at its start, unless it could not keep it.
            let kept = match self.journal.try_lock() {
                Ok(slot) => slot
                    .as_ref()
                    .filter(|_| self.saving.kept.load(Ordering::SeqCst))
                    .map(|j| j.dir().to_path_buf()),
                Err(_) => None,
            };
            match kept {
                Some(dir) => {
                    self.journal_dir = Some(dir);
                    self.schedule.kept(rev);
                }
                // Stopped before it kept anything (Esc while it waited for the generation on its
                // way): nothing failed — the next generation is due, as after any change.
                None if self.doc.unsaved() && matches!(&result, Err(e) if stopped(e)) => {
                    self.schedule.changed(self.doc.rev(), Instant::now())
                }
                None if self.doc.unsaved() => {
                    let why = self
                        .saving
                        .kept_why
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take();
                    self.schedule
                        .failed(why.unwrap_or_else(|| "the journal could not be written".into()))
                }
                None => {}
            }
        }
        let (status, shown) = report(&result, &mut self.outcome.lock().unwrap());
        self.status = status;
        if self.stream.is_some() {
            // Once the `Origin` has connected, whether this server can replace a file is known.
            let can = self
                .file
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .can_replace();
            if self.can_replace && !can {
                self.opened.push(CANNOT_REPLACE.to_string());
            }
            self.can_replace = can;
        }
        // What wrote the file itself (not a copy beside it): the view set's receipt (D25).
        if matches!(
            &result,
            Ok(JobDone::Saved(Saved::Written { .. })
                | JobDone::Streamed {
                    done: Streamed::Saved { .. } | Streamed::FoundLanded { .. },
                    ..
                })
        ) {
            self.saves_landed += 1;
        }
        let landed = match &result {
            Ok(JobDone::Saved(Saved::Written { .. })) => {
                self.doc.mark_saved(job.snapshot.clone());
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
                self.baseline = self
                    .file
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .opened();
                self.saved_journal();
                true
            }
            Ok(JobDone::Copy(..)) => {
                self.doc.mark_copied(job.snapshot.clone());
                true
            }
            Ok(JobDone::Streamed { done, .. }) => match done {
                Streamed::Saved { saved, .. } => {
                    // The stream was reloaded onto the saved file (06 §5): what is held is of
                    // the file before; each saved byte is the file's now.
                    self.doc.rebase_saved(&job.changes, &saved.seen);
                    self.doc.refresh();
                    self.baseline = saved.next;
                    self.cut_off = None;
                    self.saved_journal();
                    ctx.request_repaint();
                    true
                }
                Streamed::Copied { .. } => {
                    self.doc.mark_copied(job.snapshot.clone());
                    true
                }
                Streamed::Refused {
                    why, other_ways, ..
                } => {
                    if *other_ways {
                        self.offer = Some(why.clone());
                    }
                    false
                }
                Streamed::Ask(left) => {
                    self.left = Some(left.clone());
                    false
                }
                Streamed::FoundLanded { next, .. } => {
                    if let (Some(wrote), Some((base, journal::Seen::Known(ev)))) =
                        (self.cut_off.take(), next.clone())
                    {
                        if let Some(source) = &self.stream {
                            if let Err(e) = source.reload_saved(base) {
                                self.opened.push(format!(
                                    "the view could not be reloaded ({e:#}) — Cmd/Ctrl+R reloads"
                                ));
                            }
                        }
                        self.doc.rebase_saved(&wrote, &ev.pages);
                        self.doc.refresh();
                        self.baseline = base;
                    }
                    false
                }
            },
            Err(e) if crate::remote::is_outcome_unknown(e) => {
                // It may have landed: the next Save reconciles it before it sends anything.
                if self.stream.is_some() {
                    self.cut_off = Some(job.changes.clone());
                }
                false
            }
            _ => false,
        };
        let mut shown = shown;
        // A Save that did not land keeps the edits in the journal, and says where (07 §4.2).
        if !landed && self.doc.unsaved() {
            if let Some(dir) = &self.journal_dir {
                shown.push(format!(
                    "your edits are kept at {}",
                    super::binary::tilde(dir)
                ));
            }
        }
        self.pending_notices = Some(shown);
        let notes = match &result {
            Ok(JobDone::Saved(Saved::Written { notes, .. })) => !notes.is_empty(),
            Ok(JobDone::Streamed {
                done: Streamed::Saved { notes, .. } | Streamed::Copied { notes, .. },
                ..
            }) => !notes.is_empty(),
            _ => false,
        } || self.read_only.is_some();
        // A close asked for during this save is used up by it, whatever it reports.
        let waited = std::mem::take(&mut self.close_when_done);
        let asked = waited
            || matches!(
                job.kind,
                JobKind::Save { close_after: true } | JobKind::SaveCopy { close_after: true }
            );
        if !asked || !landed {
            return;
        }
        if self.doc.unsaved() && self.read_only.is_none() {
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

    // ---- the journal (07 §3) ----

    /// After a Save that landed: nothing is unsaved (no edit is taken while a Save runs), so the
    /// journal goes — a streamed Save's worker retired it already — and nothing is due.
    fn saved_journal(&mut self) {
        self.journal_epoch.fetch_add(1, Ordering::SeqCst);
        if !self.doc.unsaved() {
            if let Ok(mut slot) = self.journal.try_lock() {
                if let Some(journal) = slot.take() {
                    if let Err(e) = journal.remove() {
                        self.opened.push(format!(
                            "saved, but its journal could not be removed ({e:#})"
                        ));
                    }
                }
            }
            self.journal_dir = None;
        }
        self.schedule = Schedule::default();
        self.journal_rev = self.doc.rev();
    }

    /// Each pass: a change schedules the next generation at most 250 ms later; one that is due
    /// is written in the background, one at a time; a written one is taken.
    fn tick_journal(&mut self, ctx: &egui::Context) {
        if self.preview {
            return;
        }
        if let Some(rx) = &self.writing {
            match rx.try_recv() {
                Ok((rev, result)) => {
                    self.writing = None;
                    self.generation_written(rev, result);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.writing = None;
                    self.schedule
                        .failed("its writer ended without a result".to_string());
                }
            }
        }
        // Restored bytes still landing: the journal they came from stays as it is until all of
        // them are in (a generation holding a part would replace it).
        if !self.state_patches.is_empty() {
            return;
        }
        if let Some(n) = self.restoring.take() {
            self.status = Status::info(format!("restored {}", plural_bytes(n as u64)));
        }
        let now = Instant::now();
        self.note_revision(now);
        if self.job.is_none() && self.writing.is_none() && self.schedule.is_due(now) {
            self.write_generation(false);
        }
        if let Some(due) = self.schedule.due() {
            ctx.request_repaint_after(due.saturating_duration_since(now));
        }
    }

    /// The document's revision, given to the schedule once it changed (with edits, or a journal
    /// holding some): what the next generation holds.
    fn note_revision(&mut self, now: Instant) {
        let rev = self.doc.rev();
        if rev != self.journal_rev {
            self.journal_rev = rev;
            let kept = self.journal_dir.is_some() || self.writing.is_some();
            if kept || self.doc.changed_count() > 0 {
                self.schedule.changed(rev, now);
            }
        }
    }

    /// Writes the next generation — now, waiting for it (focus loss, close, a Save's start), or
    /// in the background. The journal is made by the first one.
    fn write_generation(&mut self, wait: bool) {
        // Nothing to write (an edit undone before the tick, restored bytes still landing): what
        // was due is dropped, or the tick would ask for a repaint every pass and the prompt never
        // settle. The next change of the document is due again.
        if !self.wants_generation() {
            self.schedule.taking();
            return;
        }
        // One at a time: a generation on its way is waited for before the next.
        if let Some(rx) = self.writing.take() {
            if let Ok((rev, result)) = rx.recv() {
                self.generation_written(rev, result);
            }
        }
        let (rev, work) = self.generation();
        if wait {
            let result = work();
            self.generation_written(rev, result);
        } else {
            let (tx, rx) = mpsc::channel();
            // Its end repaints the window: the status row says `kept at`, and the close prompt
            // where Discard deletes the kept edits (a receipt waits for it).
            let wake = self.wake.clone();
            std::thread::spawn(move || {
                let _ = tx.send((rev, work()));
                if let Some(ctx) = wake {
                    ctx.request_repaint();
                }
            });
            self.writing = Some(rx);
        }
    }

    /// Whether there is a generation to write: edits (or a journal already holding some), not a
    /// preview, and no restored bytes still landing.
    fn wants_generation(&mut self) -> bool {
        !self.preview
            && !self.at_revision
            && (self.journal_dir.is_some() || self.doc.changed_count() > 0)
            && self.state_patches.is_empty()
    }

    /// The next generation, to write: the revision it holds, and the write (the journal made by
    /// the first one). It writes nothing once the journal's epoch has moved past the one it was
    /// made in (a Save started, or the journal was retired).
    fn generation(&mut self) -> (u64, GenerationWork) {
        let rev = self.schedule.taking();
        let snap = snapshot_of(&self.doc, self.baseline);
        let slot = self.journal.clone();
        let (host, abs, cache) = self.home.clone();
        let fs = self.journal_fs.clone();
        let (epochs, epoch) = (
            self.journal_epoch.clone(),
            self.journal_epoch.load(Ordering::SeqCst),
        );
        #[cfg(test)]
        let gate = self.generation_gate.clone();
        let work = move || -> std::result::Result<Option<PathBuf>, String> {
            #[cfg(test)]
            while gate.as_ref().is_some_and(|g| g.load(Ordering::SeqCst)) {
                std::thread::sleep(Duration::from_millis(2));
            }
            let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
            if epochs.load(Ordering::SeqCst) != epoch {
                return Ok(None);
            }
            if slot.is_none() {
                let made = Journal::create(fs.clone(), &cache, &host, &abs, journal::Kind::Hex)
                    .map_err(|e| format!("{e:#}"))?;
                *slot = Some(made);
            }
            let journal = slot.as_mut().expect("made above");
            journal.publish(&snap).map_err(|e| format!("{e:#}"))?;
            Ok(Some(journal.dir().to_path_buf()))
        };
        (rev, Box::new(work))
    }

    fn generation_written(
        &mut self,
        rev: u64,
        result: std::result::Result<Option<PathBuf>, String>,
    ) {
        match result {
            Ok(Some(dir)) => {
                self.journal_dir = Some(dir);
                self.schedule.kept(rev);
            }
            // Superseded: what it held is a Save's to keep, or saved.
            Ok(None) => {}
            Err(why) => self.schedule.failed(why),
        }
    }

    /// The window is going (07 §3): with edits written nowhere, they are written once more and
    /// the journal is kept for the next open; otherwise — or after Discard — it is removed.
    fn settle_journal(&mut self) {
        // A file at a revision kept nothing; nothing an earlier session left is its to remove.
        if self.at_revision {
            return;
        }
        if let Some(rx) = self.writing.take() {
            let _ = rx.recv();
        }
        // An answer to the question on its way is waited for: it holds the journal while it
        // ends the record, and what is kept depends on whether it did.
        if let Some(rx) = self.answering.take() {
            #[cfg(test)]
            if let Some(hook) = self.before_drain.as_mut() {
                hook();
            }
            if let Ok(Ok(_)) = rx.recv() {
                self.left = None;
            }
        }
        // Restored bytes not all in: the journal they came from stays exactly as it is.
        if !self.discard && !self.state_patches.is_empty() {
            return;
        }
        // A question about an interrupted save's file still open (07 §3.1): its record stays
        // for the next open to ask again — without the edits, when they were discarded or
        // nothing is unsaved.
        if self.left.is_some() {
            if self.discard || self.scripted || !self.doc.unsaved() {
                let empty = journal::Snapshot {
                    baseline: self.baseline,
                    seen: journal::Seen::pages(BTreeMap::new()),
                    edits: file::edits_text(&BTreeMap::new()),
                    pastes: Vec::new(),
                };
                let kept = match self.journal.try_lock() {
                    Ok(mut slot) => slot.as_mut().map(|j| j.publish(&empty).map(|_| ())),
                    Err(_) => None,
                };
                if let Some(Err(e)) = kept {
                    eprintln!("sopen: the interrupted save's question could not be kept: {e:#}");
                }
            } else {
                self.write_generation(true);
            }
            return;
        }
        // A scripted run's edits are its state's, not the owner's: nothing of them is kept.
        if !self.discard && !self.scripted && self.doc.unsaved() {
            if self.schedule.status() != Keep::Kept || self.journal_dir.is_none() {
                self.write_generation(true);
            }
            return;
        }
        let taken = match self.journal.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        if let Some(journal) = taken {
            if let Err(e) = journal.remove() {
                eprintln!("sopen: the kept edits could not be removed: {e:#}");
            }
        }
    }

    /// Tests: the window ends as a kill ends it — the journal dropped as it is (its lock freed),
    /// nothing written or removed.
    #[cfg(test)]
    pub(super) fn crash(mut self) {
        self.crashed = true;
        if let Some(rx) = self.writing.take() {
            let _ = rx.recv();
        }
        drop(
            self.journal
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take(),
        );
    }

    /// The status bar's words for edits written nowhere: `unsaved — keeping…`, then `unsaved —
    /// kept at <journal>` (`unsaved — kept` where the bar has no room for the path), or
    /// `unsaved — NOT kept on disk (<why>)`. Returns the line, its short form, and whether it
    /// is an error.
    fn kept_line(&self) -> (String, String, bool) {
        let dir = self
            .journal_dir
            .as_deref()
            .map(super::binary::tilde)
            .unwrap_or_default();
        match self.schedule.status() {
            Keep::Clean => ("unsaved".to_string(), "unsaved".to_string(), false),
            Keep::Kept => (Keep::Kept.line(&dir), "unsaved — kept".to_string(), false),
            Keep::NotKept(why) => {
                let line = Keep::NotKept(why).line(&dir);
                (line.clone(), line, true)
            }
            Keep::Keeping => {
                let line = Keep::Keeping.line(&dir);
                (line.clone(), line, false)
            }
        }
    }

    // ---- recovery (07 §3, §3.1, §5.1) ----

    /// At the first pass of a viewer (never a preview, which must not hold another session's
    /// journals): what an earlier session left is listed in the background.
    fn start_recovery(&mut self, ctx: &egui::Context) {
        // Nor for a file at a revision: the look may reconcile a cut-off Save on the file — a
        // write — and nothing of another session is restored there.
        if self.recovery_started || self.preview || self.at_revision {
            return;
        }
        self.recovery_started = true;
        let (tx, rx) = mpsc::channel();
        let (file, ctx) = (self.file.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(file::recover(&file));
            ctx.request_repaint();
        });
        self.recovering = Some(rx);
    }

    fn poll_recovery(&mut self) {
        if let Some(rx) = &self.removing {
            match rx.try_recv() {
                Ok((dir, Ok(line))) => {
                    self.removing = None;
                    self.status = Status::info(line);
                    // Answered: the question its journal kept ends.
                    if let Some(dir) = dir {
                        self.temp_answered(&dir);
                    }
                }
                Ok((_, Err(why))) => {
                    self.removing = None;
                    self.status = Status::error(why);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.removing = None,
            }
        }
        let Some(rx) = &self.recovering else {
            return;
        };
        match rx.try_recv() {
            Ok(rows) => {
                for row in &rows {
                    if let Row::Note(line) = row {
                        eprintln!("sopen: {line}");
                    }
                }
                self.rows = rows;
                self.recovering = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.recovering = None,
        }
    }

    /// Bytes to put in as edits (Restore, Load as edits): each one an undo step, its page's
    /// evidence taken as it lands. Over a stream they land as their pages arrive.
    fn land(&mut self, bytes: impl IntoIterator<Item = (u64, u8)>) {
        let size = self.doc.size();
        for (at, b) in bytes.into_iter().filter(|(at, _)| *at < size) {
            if self.stream.is_some() {
                self.state_patches.push_back((at, b));
            } else {
                self.doc.set(at, b, self.cursor);
            }
        }
    }

    /// The question about a temp of uncertain ownership, kept in the journal at `dir`, was
    /// answered (Remove it, Keep it): its `save` record ends, and its row asks no more.
    fn temp_answered(&mut self, dir: &Path) {
        for row in &mut self.rows {
            if let Row::Edits { orphan, temp, .. } | Row::Kept { orphan, temp, .. } = row {
                if orphan.dir() == dir && temp.is_some() {
                    match orphan.clear_save() {
                        Ok(()) => *temp = None,
                        Err(e) => self.status = Status::error(format!("{e:#}")),
                    }
                }
            }
        }
    }

    /// Removes a file at an interrupted Save's temp path, after the owner's answer: on the
    /// `Origin`, off the frame (it may connect first). `dir`: the journal whose question it is.
    fn remove_temp(
        &mut self,
        left: LeftTempRef,
        dir: Option<PathBuf>,
        record: Option<journal::SaveIntent>,
    ) {
        let (tx, rx) = mpsc::channel();
        let (file, host) = (self.file.clone(), self.home.0.clone());
        std::thread::spawn(move || {
            let removed = file
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove_left(&left, record.as_ref());
            let _ = tx.send((
                dir,
                match removed {
                    Ok(()) => Ok(format!("removed {host}:{}", left.path)),
                    Err(e) => Err(format!("{e:#}")),
                },
            ));
        });
        self.removing = Some(rx);
        self.status = Status::info("removing…");
    }

    /// What a row's button asked for.
    fn row_action(&mut self, i: usize, action: RowAction) {
        if i >= self.rows.len() {
            return;
        }
        // Restore, Load, Discard, Remove and Keep all write: not for a file at a revision.
        if self.at_revision && action != RowAction::Later {
            return self.status = Status::error(format!("cannot answer: {AT_A_REVISION}"));
        }
        // A journal whose row still asks about a temp: Restore and Discard wait for the answer
        // (the question is in its record, which both would end).
        let asking = match &self.rows[i] {
            Row::Edits { orphan, temp, .. } | Row::Kept { orphan, temp, .. } => {
                temp.as_ref().map(|t| {
                    // The record that asked it, still naming that temp (07 §3.1).
                    let asked = match &orphan.save {
                        journal::SaveFile::Intent(intent) => {
                            file::record_temp(intent) == Some(t.path.as_str())
                        }
                        _ => false,
                    };
                    (orphan.dir().to_path_buf(), t.clone(), asked)
                })
            }
            _ => None,
        };
        match (&asking, action) {
            (Some(_), RowAction::Restore | RowAction::Discard) => {
                self.status = Status::error(
                    "first answer the question about the interrupted save's file: remove it or \
                     keep it",
                );
                return;
            }
            (Some((_, left, false)), RowAction::Remove) => {
                self.status = Status::error(format!(
                    "the interrupted save's record no longer names {}:{} — not removed",
                    self.home.0, left.path
                ));
                return;
            }
            (Some((dir, left, true)), RowAction::Remove) => {
                let record = self.rows.get(i).and_then(|row| match row {
                    Row::Edits { orphan, .. } | Row::Kept { orphan, .. } => match &orphan.save {
                        journal::SaveFile::Intent(intent) => Some((**intent).clone()),
                        _ => None,
                    },
                    _ => None,
                });
                return self.remove_temp(left.clone(), Some(dir.clone()), record);
            }
            (Some((dir, ..)), RowAction::Keep) => {
                let dir = dir.clone();
                self.temp_answered(&dir);
                self.status = Status::info("kept; it will not be asked about again");
                return;
            }
            _ => {}
        }
        // One window keeps one journal: with edits of its own, Restore is refused and the row
        // stays.
        if action == RowAction::Restore
            && (self.doc.changed_count() > 0 || self.journal_dir.is_some())
        {
            self.status = Status::error(
                "not restored: this window has edits of its own — undo or save them first",
            );
            return;
        }
        let row = self.rows.remove(i);
        match (row, action) {
            (
                Row::Edits {
                    orphan,
                    line,
                    check,
                    edits,
                    temp,
                },
                RowAction::Restore,
            ) if !matches!(
                check,
                journal::Restore::Apply { .. } | journal::Restore::Ask
            ) =>
            {
                // Restore's checks decide (07 §3): edits whose file changed under them are
                // never applied, whatever asks.
                self.rows.insert(
                    i,
                    Row::Edits {
                        orphan,
                        line,
                        check,
                        edits,
                        temp,
                    },
                );
            }
            (Row::Edits { orphan, edits, .. }, RowAction::Restore) => {
                match orphan.adopt() {
                    Ok(mut taken) => {
                        // A Save its session left cut off is ended here: the row named what to
                        // check, and these edits are now this window's.
                        if taken.pending_save().is_some() {
                            if let Err(e) = taken.clear_save() {
                                self.opened.push(format!("{e:#}"));
                            }
                        }
                        self.journal_dir = Some(taken.dir().to_path_buf());
                        *self.journal.lock().unwrap_or_else(|p| p.into_inner()) = Some(taken);
                        let n: usize = edits.iter().map(|(_, b)| b.len()).sum();
                        self.land(edits.into_iter().flat_map(|(at, bytes)| {
                            bytes
                                .into_iter()
                                .enumerate()
                                .map(move |(i, b)| (at + i as u64, b))
                        }));
                        // Said once every byte is in (over a stream, as its page arrives).
                        self.restoring = Some(n);
                        self.status =
                            Status::info(format!("putting back {}…", plural_bytes(n as u64)));
                    }
                    Err(e) => self.status = Status::error(format!("not restored: {e:#}")),
                }
            }
            (Row::Edits { orphan, .. } | Row::Kept { orphan, .. }, RowAction::Discard) => {
                match orphan.remove() {
                    Ok(()) => self.status = Status::info("the kept edits were discarded"),
                    Err(e) => self.status = Status::error(format!("not discarded: {e:#}")),
                }
            }
            (Row::Undo { size, runs, .. }, RowAction::Load) => {
                if size != self.doc.size() {
                    self.status = Status::error(format!(
                        "not loaded: the undo file is of the file at {} bytes; it has {} now",
                        grouped(size),
                        grouped(self.doc.size())
                    ));
                    return;
                }
                let n: usize = runs.iter().map(|r| r.bytes.len()).sum();
                self.land(runs.into_iter().flat_map(|r| {
                    let at = r.offset;
                    r.bytes
                        .into_iter()
                        .enumerate()
                        .map(move |(i, b)| (at + i as u64, b))
                }));
                self.status = Status::info(format!("loaded {} as edits", plural_bytes(n as u64)));
            }
            (Row::Temp { path, line, seen }, RowAction::Remove) => self.remove_temp(
                LeftTempRef {
                    path,
                    line,
                    seen,
                    proven: false,
                },
                None,
                None,
            ),
            // Keep (a temp of a Save that landed: nothing records the question) and Later: the
            // row goes, and what it held (its journal's lock) with it. A journal that still asks
            // about a temp asks again on the next open.
            (_, RowAction::Later | RowAction::Keep) => {}
            (row, _) => self.rows.insert(i, row),
        }
    }

    // ---- the rows below the grid, in cells (X3-questions) ----

    /// The rows below the grid in `room` cells (10 §6), top to bottom: the lines to read whole
    /// (the stream's warning, what open said, the read-only line, the last write's notices) in
    /// the error colour, wrapped by cells; the read-only window's keep row while the edits are
    /// listed nowhere; a refusal's words with Write in place… and Save As… (07 §4.1); the live
    /// question about a temp with Remove it and Keep it (07 §3.1); then the recovery rows (07 §3,
    /// §5.1) — a line each, a journal's question about a temp under it, its buttons (buttons,
    /// not letter keys: a letter types a byte), the question's answers first. Cells have no
    /// hover: why a row's buttons are off is a dim row under them.
    fn below(&self, cols: u16) -> Vec<Below> {
        let room = usize::from(cols.saturating_sub(2).max(1));
        let p = chrome::PALETTE;
        let (error, dim) = (Pen::new(p.error, p.back), Pen::new(p.dim, p.back));
        let mut out = vec![];
        let text = |out: &mut Vec<Below>, line: &str, pen: Pen| {
            for line in line.lines() {
                let rows = dialog::wrap(line, room);
                out.extend(rows.into_iter().map(|row| Below::Text(row, pen)));
            }
        };
        let warning = self.stream_warning();
        let lines = (warning.iter())
            .chain(&self.opened)
            .chain(&self.read_only)
            .chain(&self.notices);
        for line in lines {
            text(&mut out, line, error);
        }
        if self.read_only.is_some() && self.retained.is_none() {
            // `[ Keep edits… ]`, the field, `[ Write the list there ]` on one row where the
            // field keeps its fewest cells; else the last on the next row — and the field on a
            // row of its own where not a cell of it fits beside the button.
            let at = 1 + button_cells("Keep edits…") + 2;
            let after = 2 + button_cells("Write the list there") + 1;
            if cols >= at + KEEP_FEWEST + after {
                let field = cols - at - after;
                out.push(Below::Keep { field, write: true });
            } else if cols > at + 1 {
                let field = cols - at - 1;
                out.push(Below::Keep {
                    field,
                    write: false,
                });
                out.push(Below::KeepWrite);
            } else {
                out.push(Below::Keep {
                    field: 0,
                    write: false,
                });
                out.push(Below::KeepField(cols.saturating_sub(2)));
                out.push(Below::KeepWrite);
            }
        }
        let free = self.job.is_none() && !self.dialog_up();
        let control = |label: &'static str, press: Press, on: bool| Control { label, press, on };
        if let Some(why) = &self.offer {
            text(&mut out, &format!("not saved: {why}"), error);
            let all = vec![
                control("Write in place…", Press::InPlace, free),
                control("Save As…", Press::OfferSaveAs, free),
            ];
            out.extend(placed(all, None, cols));
        }
        if let Some(left) = &self.left {
            text(&mut out, &left.line, error);
            let free = free && self.answering.is_none();
            let all = vec![
                control("Remove it", Press::Left(true), free),
                control("Keep it", Press::Left(false), free),
            ];
            out.extend(placed(all, None, cols));
        }
        let busy = self.job.is_some() || self.removing.is_some();
        for row in &self.rows {
            let key = row_key(row);
            let (line, temp, buttons) = recovery_row(row);
            text(&mut out, line, error);
            let mut all = vec![];
            if let Some(temp) = temp {
                text(&mut out, &temp.line, error);
                for (label, action) in ASK {
                    all.push(control(label, Press::Row(key, action), !busy));
                }
            }
            let waits = |action| {
                temp.is_some() && matches!(action, RowAction::Restore | RowAction::Discard)
            };
            for &(label, action) in buttons {
                all.push(control(
                    label,
                    Press::Row(key, action),
                    !busy && !waits(action),
                ));
            }
            let rule = temp.is_some().then_some(ASK.len());
            out.extend(placed(all, rule, cols));
            if temp.is_some() && buttons.iter().any(|&(_, a)| waits(a)) {
                let why = "Restore and Discard wait: first answer the question about the \
                           interrupted save's file";
                text(&mut out, why, dim);
            }
        }
        if !self.rows.is_empty() && self.job.is_some() {
            text(
                &mut out,
                "the rows' buttons are off: a save is running",
                dim,
            );
        }
        out
    }

    /// The controls of the rows below the grid, `cols` cells wide, that act now, in the order
    /// Tab walks them — exactly those [`HexWindow::below_ui`] draws and lets act there: each
    /// button that is on, and the keep row's path field between its two buttons.
    fn below_controls(&self, cols: u16) -> Vec<egui::Id> {
        let mut out = vec![];
        for row in &self.below(cols) {
            match row {
                Below::Text(..) => {}
                Below::Buttons(all, _) => {
                    let on = all.iter().filter(|(x, c)| c.on && shows(*x, cols));
                    out.extend(on.map(|(_, c)| below_id(c.press)));
                }
                Below::Keep { field, .. } => {
                    if shows(1, cols) {
                        out.push(below_id(Press::KeepEdits));
                    }
                    if *field > 0 {
                        out.push(keep_id());
                    }
                }
                Below::KeepField(field) => {
                    if *field > 0 {
                        out.push(keep_id());
                    }
                }
                Below::KeepWrite => {}
            }
            if matches!(row_write(row), Some(true)) && shows(1, cols) {
                out.push(below_id(Press::WriteList));
            }
        }
        out
    }

    /// The rows below the grid at `ui`'s cells: their texts, their `[ buttons ]` — two blanks
    /// apart, a `│` between a question's answers and the row's own; the bar's colours on the one
    /// with the keyboard, the dialog's weak colour on one that is off, as the toolbar's — each an
    /// egui control at its cells, and the keep row's path field (`ops::dialog`'s). What a click,
    /// or Enter or Space on the focused one, pressed.
    fn below_ui(&mut self, ui: &mut egui::Ui, rows: &[Below]) -> Option<Press> {
        let ctx = ui.ctx().clone();
        let p = chrome::PALETTE;
        let grid = Grid::over(ui.max_rect());
        let mut buf = Buffer::empty(grid.all());
        cells::fill(&mut buf, grid.all(), Pen::new(p.text, p.back));
        let cols = grid.cols;
        let (mut pressed, mut caret) = (None, None);
        // A button at cell `x` of row `y`, cut at the row's end (one wider than the row stands
        // alone there, [`placed`]): drawn, and acting at the cells it shows.
        let mut button = |ui: &mut egui::Ui, buf: &mut Buffer, x: u16, y: u16, c: &Control| {
            let w = button_cells(c.label).min(cols.saturating_sub(x));
            if w == 0 {
                return;
            }
            let id = below_id(c.press);
            let sense = if c.on { Sense::click() } else { Sense::hover() };
            let resp = ui.interact(grid.rect(Cells::new(x, y, w, 1)), id, sense);
            if c.on && resp.clicked() {
                pressed = Some(c.press);
            }
            let pen = if !c.on {
                Pen::new(p.dialog_weak, p.dialog_back)
            } else if ctx.memory(|m| m.has_focus(id)) {
                Pen::new(p.bar_text, p.bar)
            } else {
                Pen::new(p.dialog_text, p.dialog_back)
            };
            cells::put(buf, x, y, &format!("[ {} ]", c.label), w.into(), pen);
        };
        let keep = Control {
            label: "Keep edits…",
            press: Press::KeepEdits,
            on: true,
        };
        let write = Control {
            label: "Write the list there",
            press: Press::WriteList,
            on: true,
        };
        for (y, row) in (0u16..).zip(rows) {
            match row {
                Below::Text(text, pen) => {
                    let room = usize::from(cols.saturating_sub(2));
                    cells::put(&mut buf, 1, y, text, room, *pen);
                }
                Below::Buttons(all, rule) => {
                    if let Some(x) = rule {
                        cells::put(&mut buf, *x, y, "│", 1, Pen::new(p.text, p.back));
                    }
                    for (x, c) in all {
                        button(ui, &mut buf, *x, y, c);
                    }
                }
                Below::Keep { field, write: here } => {
                    button(ui, &mut buf, 1, y, &keep);
                    let at = 1 + button_cells(keep.label) + 2;
                    let field = Cells::new(at, y, *field, 1);
                    if field.width > 0 {
                        caret = caret.or(self.keep_field(ui, &grid, &mut buf, field));
                    }
                    if *here {
                        button(ui, &mut buf, field.right() + 2, y, &write);
                    }
                }
                Below::KeepField(field) => {
                    let field = Cells::new(1, y, *field, 1);
                    if field.width > 0 {
                        caret = caret.or(self.keep_field(ui, &grid, &mut buf, field));
                    }
                }
                Below::KeepWrite => button(ui, &mut buf, 1, y, &write),
            }
        }
        self.blank_under(&ctx, &grid, &mut buf);
        cells::paint(
            &ui.painter_at(grid.rect(grid.all())),
            grid.origin,
            &buf,
            caret,
        );
        pressed
    }

    /// The key that pressed a control below the grid ([`HexWindow::held`]), while it stays
    /// down: its presses — a held key's are its auto-repeats — and, for Space, the text each one
    /// sends are taken from the pass, so they never type into the grid the keyboard went back to
    /// (or press what has it now). Let go — its release in this pass, or no longer down — it is
    /// forgotten, and what comes after the release stays.
    fn drop_held(&mut self, ctx: &egui::Context) {
        let Some(key) = self.held else {
            return;
        };
        ctx.input_mut(|i| {
            let released = i.events.iter().position(
                |e| matches!(e, egui::Event::Key { key: k, pressed: false, .. } if *k == key),
            );
            let end = released.unwrap_or(i.events.len());
            let mut k = 0;
            i.events.retain(|e| {
                let before = k < end;
                k += 1;
                let held = match e {
                    egui::Event::Key {
                        key: k,
                        pressed: true,
                        ..
                    } => *k == key,
                    egui::Event::Text(t) => key == Key::Space && t == " ",
                    _ => false,
                };
                !(before && held)
            });
            if released.is_some() || !i.key_down(key) {
                self.held = None;
            }
        });
    }

    /// The keep row's path field at cells `at` of `buf` (on `grid`): its engine and its cells
    /// (`ops::dialog`'s), and the caret's cell while it has the keyboard.
    fn keep_field(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        buf: &mut Buffer,
        at: Cells,
    ) -> Option<Position> {
        let ctx = ui.ctx().clone();
        dialog::field(ui, grid, at, keep_id(), &mut self.keep_to, |e| e);
        let focused = ctx.memory(|m| m.has_focus(keep_id()));
        dialog::field_cells(&ctx, buf, at, keep_id(), &self.keep_to, focused)
    }

    /// What a control of the rows below the grid asked for.
    fn below_press(&mut self, ctx: &egui::Context, press: Press) {
        match press {
            Press::KeepEdits => self.keep_edits(),
            Press::WriteList => self.keep_edits_at(),
            Press::InPlace => {
                let changes = self.doc.changes();
                let undo = self
                    .file
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .undo_path(self.session, self.baseline);
                self.in_place_ask = Some(patch::in_place_question(
                    &self.home.0,
                    &self.home.1,
                    changes.len(),
                    &undo,
                ));
            }
            Press::OfferSaveAs => self.open_save_as(false),
            Press::Left(remove) => self.answer_left(ctx, remove),
            Press::Row(key, action) => {
                let Some(i) = self.rows.iter().position(|r| row_key(r) == key) else {
                    return;
                };
                let before = rows_seen(&self.rows);
                self.row_action(i, action);
                // The rows changed under the control that had the keyboard (a row went, a
                // question was answered): the keyboard goes back to the grid, never to a
                // control that comes into that place.
                if rows_seen(&self.rows) != before {
                    ctx.memory_mut(|m| m.request_focus(grid_id()));
                }
            }
        }
    }

    /// The keys of the rows below the grid (`controls`, in Tab's order). From the grid, Tab past
    /// the ASCII column — or anywhere, read-only, where nothing is typed — gives the keyboard to
    /// the first control, Shift+Tab before the hex column to the last. While a control has it,
    /// Tab and Shift+Tab walk them — past the last back to the grid's hex column, before the
    /// first to its ASCII column — Esc gives it back to the grid, and Enter or Space press the
    /// control (egui's click on a focused one); no other key acts. Each is taken from egui's own
    /// focus moves, which would walk the toolbar too. Returns whether a control has the keyboard.
    fn below_keys(&mut self, ctx: &egui::Context, controls: &[egui::Id]) -> bool {
        let lock = egui::EventFilter {
            tab: true,
            horizontal_arrows: true,
            vertical_arrows: true,
            escape: true,
        };
        let focused = ctx.memory(|m| m.focused());
        let at = focused.and_then(|f| controls.iter().position(|c| *c == f));
        // A control that had the keyboard until egui's own Esc took it at the start of this pass
        // (its lock is set from its second pass on).
        let left = || (controls.iter()).any(|c| ctx.memory(|m| m.had_focus_last_frame(*c)));
        let take = |shift: bool| {
            ctx.input_mut(|i| {
                let at = i.events.iter().position(|e| {
                    matches!(e, egui::Event::Key { key: Key::Tab, pressed: true, modifiers, .. }
                        if modifiers.shift == shift && !modifiers.command && !modifiers.alt)
                });
                at.map(|at| i.events.remove(at)).is_some()
            })
        };
        let focus = |id: egui::Id| {
            ctx.memory_mut(|m| {
                m.request_focus(id);
                m.move_focus(egui::FocusDirection::None);
            })
        };
        // A key's auto-repeat never presses a control: only a press of its own does — also in
        // the pass in which Tab brings the keyboard to one (the control has it at once, and is
        // drawn later in the pass). Typed text is its `Text`, which this leaves.
        if !controls.is_empty() {
            ctx.input_mut(|input| {
                input.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::Key {
                            key: Key::Enter | Key::Space,
                            pressed: true,
                            repeat: true,
                            ..
                        }
                    )
                })
            });
        }
        let Some(i) = at else {
            if focused.is_none() && left() && bare_key(ctx, Key::Escape) {
                focus(grid_id());
                return true;
            }
            if focused != Some(grid_id()) || controls.is_empty() {
                return false;
            }
            let typing = !self.locked();
            if (!typing || self.cursor.pane == Pane::Ascii) && take(false) {
                focus(controls[0]);
                return true;
            }
            if (!typing || self.cursor.pane == Pane::Hex) && take(true) {
                focus(controls[controls.len() - 1]);
                return true;
            }
            return false;
        };
        ctx.memory_mut(|m| m.set_focus_lock_filter(controls[i], lock));
        if take(false) {
            match controls.get(i + 1) {
                Some(next) => focus(*next),
                None => {
                    focus(grid_id());
                    self.place(self.cursor.at, Pane::Hex);
                }
            }
        } else if take(true) {
            match i.checked_sub(1) {
                Some(before) => focus(controls[before]),
                None => {
                    focus(grid_id());
                    self.place(self.cursor.at, Pane::Ascii);
                }
            }
        } else if bare_key(ctx, Key::Escape) {
            focus(grid_id());
        }
        true
    }

    // ---- Save As (07 §7) ----

    /// Opens the Save As modal over the edits; `close_after`: the window closes once it lands.
    fn open_save_as(&mut self, close_after: bool) {
        if self.save_as.is_some() || self.job.is_some() {
            return;
        }
        if self.at_revision {
            return self.status = Status::error(format!("cannot save: {AT_A_REVISION}"));
        }
        if let Some(why) = &self.read_only {
            return self.status = Status::error(why.clone());
        }
        let changes = self.doc.changes();
        let seen = self.doc.evidence(&changes);
        let mut producer = file::HexProducer::new(self.file.clone(), changes, seen, self.baseline);
        producer.hold = save_as_hold();
        #[cfg(test)]
        if let Some(hold) = self.test_hold {
            producer.hold = Some(hold);
            producer.release = self.test_release.clone();
        }
        if producer.hold.is_some() {
            producer.held = Some(self.save_as_held.clone());
        }
        let name = crate::remote::split_parent(&self.home.1).1.to_string();
        let rate = self.stream.as_ref().and_then(Source::rate);
        #[cfg(test)]
        let rate = self.test_rate.or(rate);
        let job = save_as::Job {
            name,
            size: save_as::Size::Exact(self.doc.size()),
            rate,
            producer: Box::new(producer),
        };
        let folder = self
            .save_as_folder
            .clone()
            .unwrap_or_else(super::binary::download_dir);
        #[cfg(test)]
        let chooser: Box<dyn super::binary::Chooser> = match &self.chooser {
            Some(make) => make(),
            None => Box::new(super::binary::NativeChooser),
        };
        #[cfg(not(test))]
        let chooser: Box<dyn super::binary::Chooser> = Box::new(super::binary::NativeChooser);
        let modal = SaveAs::new(job, &folder, chooser);
        self.save_as = Some((modal, close_after, self.doc.patches().clone()));
    }

    /// The Save As modal's pass, while it is open: a save that landed counts as Save copy does
    /// (07 §7 "What it counts as").
    fn show_save_as(&mut self, ctx: &egui::Context) {
        let Some((modal, close_after, edits)) = self.save_as.as_mut() else {
            return;
        };
        // Where Save As starts writing is the modal's pass: a file at a revision has none — a
        // modal not writing yet goes (one already writing finishes).
        if self.at_revision && !modal.busy() {
            self.save_as = None;
            return;
        }
        let ended = modal.show_on(ctx, Grid::over(self.area));
        let (close_after, edits) = (*close_after, edits.clone());
        match ended {
            Some(save_as::Ended::Saved { line, notes, .. }) => {
                self.save_as = None;
                self.doc.mark_copied(edits);
                self.status = Status::info(line);
                self.pending_notices = Some(notes.clone());
                if close_after && notes.is_empty() && !self.doc.unsaved() {
                    self.close_ok = true;
                    super::request_close(ctx);
                }
            }
            Some(save_as::Ended::Closed) => self.save_as = None,
            None => {}
        }
    }

    /// Whether input may change anything: not under the close prompt, not once closing is
    /// decided, not once the window is read-only.
    fn locked(&self) -> bool {
        self.prompt || self.close_ok || self.read_only.is_some()
    }

    /// A dialog of this window has the keys: Save As, the confirmation, Write in place.
    fn dialog_up(&self) -> bool {
        self.save_as.is_some() || self.confirm.is_some() || self.in_place_ask.is_some()
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

    /// Keys for the whole window. `goto_owns` = the Go to field has this frame's typing (it is
    /// focused, was clicked, lost focus on Enter, or Cmd/Ctrl+G gave it the keyboard): then no
    /// byte is typed and no key moves the cursor. `controls`: the rows below the grid's, which
    /// take the keyboard from the grid by Tab ([`HexWindow::below_keys`]) and keep every key
    /// while they have it.
    fn keys(&mut self, ctx: &egui::Context, goto_owns: bool, controls: &[egui::Id]) {
        use super::command_key;
        // Esc stops a streamed Save between its requests (07 §4.2 step 5): its temp is removed
        // and the target is untouched. A request already sent finishes on the server first.
        if self.job.is_some()
            && self.stream.is_some()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.saving.cancel.store(true, Ordering::SeqCst);
        }
        if self.prompt || self.close_ok || self.dialog_up() {
            return;
        }
        if !goto_owns && self.below_keys(ctx, controls) {
            return;
        }
        if self.locked() {
            return;
        }
        // Save As first (07 §7): egui's shortcut check below would take Alt+Cmd/Ctrl+S for
        // Cmd/Ctrl+S. F7 alone is a plain key here as F5 is (the grid holds the keyboard, so
        // `save_as::pressed` leaves it): never while a field has the typing.
        if save_as::pressed(ctx) || (!goto_owns && bare_key(ctx, Key::F7)) {
            self.open_save_as(false);
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
        // No edit, undo or redo while a Save runs (07 §10): what it writes is the edits at its
        // start, and the journal's last generation stays the one it records.
        let saving = self.job.is_some();
        if self.reloading.is_some() {
            // Nothing is edited until the reload has landed; the cursor still moves.
        } else if command_key(ctx, Key::Z, true) {
            if saving {
                self.edit_blocked = true;
            } else {
                self.redo();
            }
        } else if command_key(ctx, Key::Z, false) {
            if saving {
                self.edit_blocked = true;
            } else {
                self.undo();
            }
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
        // Viewing (D35): typed text is not read — no byte changes until Enter or Insert.
        let viewing = self.in_viewer && !self.editing;
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
        for c in typed.into_iter().filter(|_| !viewing) {
            if self.no_edit_here() {
                break;
            }
            if self.reloading.is_some() {
                self.status = Status::info(RELOADING);
                break;
            }
            if saving {
                self.edit_blocked = true;
                break;
            }
            if self.doc.full_at(self.cursor.at.min(last)) {
                self.status = Status::error(EDITS_FULL);
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

    /// The toolbar, one row of cells (10 §6): `Go to` and its field — black cells, what is typed
    /// in white, the hint dim while it is empty, the caret its cell while it has the keyboard —
    /// then `[ Undo ]  [ Redo ]  [ Save ]  [ Save copy ]  [ Save As… ]`, two blank cells apart,
    /// in the dialog's colours (blue, the weak colour, while one does nothing), each an egui
    /// interaction at its cells with what it does on hover. Nothing in it comes, goes or changes
    /// its label; on a narrow window the field gives up cells first (down to [`GOTO_FEWEST`]),
    /// then the last buttons do, never half of one. Returns whether the field has this frame's
    /// typing besides holding the keyboard: it let the keyboard go in this frame (Enter goes
    /// there), or it was pressed.
    fn toolbar(&mut self, ui: &mut egui::Ui) -> bool {
        let ctx = ui.ctx().clone();
        let p = chrome::PALETTE;
        // A save or a reload under way: no other one, and no edit, starts (07 §10).
        let busy = self.job.is_some() || self.reloading.is_some();
        let free = !self.locked() && !busy && !self.dialog_up();
        // A file at a revision has no place to be saved (03 §6.3): all three off, with that
        // reason on hover before any other.
        let saves = !self.at_revision;
        let running = if self.at_revision {
            AT_A_REVISION
        } else {
            "a save is running"
        };
        let save_off = if self.at_revision {
            AT_A_REVISION
        } else if !self.can_replace {
            CANNOT_REPLACE
        } else if self.reloading.is_some() {
            RELOADING
        } else if busy {
            "a save is running"
        } else {
            "no changes to save"
        };
        let save_tip = if self.stream.is_some() {
            "⌘/Ctrl+S — writes your changes into the original (copied on its host, then renamed \
             over it); the bytes they replace are kept locally"
        } else {
            "⌘/Ctrl+S — overwrites the original after a local backup"
        };
        let tools = [
            Tool {
                does: Does::Undo,
                on: free && self.doc.can_undo(),
                tip: "⌘/Ctrl+Z",
                off: "",
            },
            Tool {
                does: Does::Redo,
                on: free && self.doc.can_redo(),
                tip: "⇧⌘/Shift+Ctrl+Z",
                off: "",
            },
            Tool {
                does: Does::Save,
                on: saves && free && self.doc.dirty() && self.can_replace,
                tip: save_tip,
                off: save_off,
            },
            Tool {
                does: Does::SaveCopy,
                on: saves && free,
                tip: "⇧⌘/Shift+Ctrl+S — a new <name>.edited file next to the original",
                off: running,
            },
            Tool {
                does: Does::SaveAs,
                on: saves && free,
                tip: "⌥⌘/Alt+Ctrl+S, F7 — a new local file with your changes",
                off: running,
            },
        ];
        let (grid, mut buf) = cell_row(ui);
        let body = Pen::new(p.text, p.back);
        cells::put(&mut buf, 1, 0, "Go to", 5, body);
        // The field's cells: its nominal width where the whole row fits, else what is left,
        // never fewer than `GOTO_FEWEST`.
        let buttons: u16 = tools.iter().map(|t| 2 + t.cells()).sum();
        let left = grid.cols.saturating_sub(GOTO_AT + buttons);
        let field = Cells::new(GOTO_AT, 0, left.clamp(GOTO_FEWEST, GOTO_CELLS), 1);
        let field_rect = grid.rect(field);
        ui.interact(field_rect, goto_id().with("hover"), Sense::hover())
            .on_hover_text("⌘/Ctrl+G · Enter goes there");
        dialog::field(ui, &grid, field, goto_id(), &mut self.goto, |e| e);
        let focused = ctx.memory(|m| m.has_focus(goto_id()));
        let caret = dialog::field_cells(&ctx, &mut buf, field, goto_id(), &self.goto, focused);
        if self.goto.is_empty() {
            let hint = cells::fit_start(GOTO_HINT, field.width.into());
            let pen = Pen::new(p.dim, chrome::cga::BLACK);
            cells::put(&mut buf, field.x, 0, &hint, field.width.into(), pen);
        }
        let mut pressed = None;
        let mut x = field.right() + 2;
        for tool in tools {
            let w = tool.cells();
            if x + w > grid.cols {
                break;
            }
            let at = Cells::new(x, 0, w, 1);
            let id = tool_id(tool.does);
            let sense = if tool.on {
                Sense::click()
            } else {
                Sense::hover()
            };
            let resp = ui.interact(grid.rect(at), id, sense);
            let tip = if tool.on { tool.tip } else { tool.off };
            let resp = if tip.is_empty() {
                resp
            } else {
                resp.on_hover_text(tip)
            };
            if tool.on && resp.clicked() {
                pressed = Some(tool.does);
            }
            let pen = if !tool.on {
                Pen::new(p.dialog_weak, p.dialog_back)
            } else if ctx.memory(|m| m.has_focus(id)) {
                Pen::new(p.bar_text, p.bar)
            } else {
                Pen::new(p.dialog_text, p.dialog_back)
            };
            cells::put(
                &mut buf,
                x,
                0,
                &format!("[ {} ]", tool.does.label()),
                w.into(),
                pen,
            );
            x += w + 2;
        }
        self.blank_under(&ctx, &grid, &mut buf);
        cells::paint(
            &ui.painter_at(grid.rect(grid.all())),
            grid.origin,
            &buf,
            caret,
        );
        match pressed {
            Some(Does::Undo) => self.undo(),
            Some(Does::Redo) => self.redo(),
            Some(Does::Save) => self.start(&ctx, JobKind::Save { close_after: false }),
            Some(Does::SaveCopy) => self.start(&ctx, JobKind::SaveCopy { close_after: false }),
            Some(Does::SaveAs) => self.open_save_as(false),
            None => {}
        }
        // The field let the keyboard go in this frame (Enter, Esc, a click elsewhere): Enter
        // goes there.
        let lost = ctx.memory(|m| m.had_focus_last_frame(goto_id()) && !m.has_focus(goto_id()));
        if lost && ui.input(|i| i.key_pressed(Key::Enter)) {
            self.goto();
        }
        let pressed_on =
            ui.input(|i| i.pointer.primary_pressed()) && ui.rect_contains_pointer(field_rect);
        lost || pressed_on
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

    /// What the status row says now: while a Save runs, that editing waits for it (when an
    /// edit was tried) or where it is; else a reload under way, else a key that did nothing
    /// because its byte is loading, else an error already there, else the baseline pass, else
    /// rows on their way, else what the window last said. The stream's own warning is not
    /// here: see [`HexWindow::stream_warning`].
    pub(super) fn status_text(&mut self, ctx: &egui::Context) -> Status {
        if self.job.is_some() {
            if self.edit_blocked {
                return Status::info(EDITING_RESUMES);
            }
            if self.checking.active.load(Ordering::SeqCst) {
                return Status::info(self.checking_line());
            }
            let progress = *self
                .saving
                .progress
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(progress) = progress {
                return Status::info(progress.line(&self.home.0));
            }
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
        if self.under_edits {
            return Status::error(patch::CHANGED_UNDER_EDITS);
        }
        if self.checking.active.load(Ordering::SeqCst) {
            return Status::info(self.checking_line());
        }
        match self.loading_line(ctx) {
            Some(line) => Status::info(line),
            None if self.in_viewer && self.status.text.is_empty() => {
                Status::info(if self.editing { EDITING } else { VIEWING })
            }
            None => self.status.clone(),
        }
    }

    /// `checking the file for Save — 37 %`.
    fn checking_line(&self) -> String {
        patch::checking_line(
            self.checking.done.load(Ordering::SeqCst),
            self.checking.total.load(Ordering::SeqCst),
        )
    }

    /// The baseline pass (07 §4.5): started at the first edit of a streamed file of at most
    /// 256 MiB; once it has ended, every edited page's evidence must agree with it, or the file
    /// changed under the edits and Save is refused.
    fn tick_pass(&mut self) {
        let Some(source) = &self.stream else {
            return;
        };
        if self.preview || self.baseline.sha256.is_some() || self.doc.size() > HASH_CHECK_MAX {
            return;
        }
        if self.doc.changed_count() == 0 {
            return;
        }
        self.checking.start(source);
        if self.pass_rev == Some(self.doc.rev()) {
            return;
        }
        if let Some(Ok(pass)) = self.checking.ended(source) {
            let changes = self.doc.changes();
            self.under_edits = pass.agrees(&self.doc.evidence(&changes)).is_err();
            self.pass_rev = Some(self.doc.rev());
        }
    }

    /// The stream's warning (06 §5: replaced, changed, failed), for as long as that is its
    /// state. It is shown whole in the rows below the grid, where no line of the status row can
    /// take it off or cut its reload hint short.
    pub(super) fn stream_warning(&self) -> Option<String> {
        self.stream.as_ref().and_then(|s| s.state().status())
    }

    /// The status row, one row of cells whatever it says (10 §6): the cursor's offset and byte,
    /// the size (in bytes on hover), the changed bytes, a one-cell spinner while a save runs, what
    /// the window last said — cut with "…" to the room it has (whole on hover) — and where the
    /// edits are kept (07 §3) at its right end, `│` between them. The grid above never moves.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let p = chrome::PALETTE;
        let cursor = self.cursor_line();
        let size = self.doc.size();
        let changed = self.doc.changed_count() as u64;
        let unsaved = self.doc.unsaved();
        if let Some(e) = self.doc.read_error() {
            if !self.status.error {
                self.status = Status::error(e.to_string());
            }
        }
        let status = self.status_text(&ctx);
        let shown = self.job.is_some() || !status.text.is_empty();
        let (grid, mut buf) = cell_row(ui);
        let body = Pen::new(p.text, p.back);
        let end = grid.cols.saturating_sub(1);
        let mut x = 1u16;
        let put = |buf: &mut Buffer, x: &mut u16, text: &str, pen: Pen| -> Cells {
            let w = cells::put(buf, *x, 0, text, usize::from(end.saturating_sub(*x)), pen) as u16;
            let at = Cells::new(*x, 0, w, 1);
            *x += w;
            at
        };
        put(&mut buf, &mut x, &cursor, body);
        put(&mut buf, &mut x, RULE, body);
        let size_at = put(&mut buf, &mut x, &crate::human(size), body);
        put(&mut buf, &mut x, RULE, body);
        put(
            &mut buf,
            &mut x,
            &format!("{} changed", plural_bytes(changed)),
            body,
        );
        if shown {
            put(&mut buf, &mut x, RULE, body);
        }
        if self.job.is_some() {
            // A one-cell spinner (10 §6), a quarter turn every tenth of a second.
            let turn = (ctx.input(|i| i.time) * 10.0) as usize % SPINNER.len();
            put(&mut buf, &mut x, SPINNER[turn], body);
            put(&mut buf, &mut x, " ", body);
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // Where the edits are kept holds the row's right end; what the window last said takes
        // the room before it, cut with "…" when it is longer.
        let room = usize::from(end.saturating_sub(x));
        let kept = unsaved
            .then(|| self.kept_line())
            .map(|(full, short, failed)| {
                // The path when it fits in half the room; whole on hover either way.
                let line = if cells::width(&full) <= room / 2 {
                    full
                } else {
                    short
                };
                (line, failed)
            });
        let reserved = kept
            .as_ref()
            .map_or(0, |(line, _)| cells::width(line) + cells::width(RULE));
        let text: String = (status.text.chars())
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let said = cells::fit_start(&text, room.saturating_sub(reserved));
        let color = if status.error { p.error } else { p.text };
        let said_at = put(&mut buf, &mut x, &said, Pen::new(color, p.back));
        let mut kept_at = None;
        if let Some((line, failed)) = &kept {
            x = end.saturating_sub(reserved as u16).max(x);
            put(&mut buf, &mut x, RULE, body);
            let color = if *failed { p.error } else { p.text };
            kept_at = Some(put(&mut buf, &mut x, line, Pen::new(color, p.back)));
        }
        self.blank_under(&ctx, &grid, &mut buf);
        cells::paint(
            &ui.painter_at(grid.rect(grid.all())),
            grid.origin,
            &buf,
            None,
        );
        let hover = |at: Cells, salt: &str| {
            ui.interact(
                grid.rect(at),
                egui::Id::unique(("sopen-hex-status", salt)),
                Sense::hover(),
            )
        };
        hover(size_at, "size").on_hover_text(format!("{} bytes", grouped(size)));
        if said != text {
            hover(said_at, "said").on_hover_text(&status.text);
        }
        if let (Some(at), Some(dir)) = (kept_at, &self.journal_dir) {
            hover(at, "kept").on_hover_text(format!("kept at {}", dir.display()));
        }
    }

    /// The window, as PC Shell's File Edit, in cells (10 §4–§6): its frame, the file on its
    /// first line, then the rows — offsets, hex, ASCII, the cursor in both columns (the bar's
    /// colours where it types, black in the other), changed bytes yellow on black, bytes on
    /// their way `··` dim — and the scroll bar on its right border. Rows the file does not reach
    /// are blank cells of the frame. Only whole rows of cells are shown, and only those are read
    /// and painted. The caret is the cell the next digit goes into when that is a byte's low
    /// nibble (as the underline was), unless `goto_focused` (a field has the keyboard).
    fn grid(&mut self, ui: &mut egui::Ui, goto_focused: bool) {
        let ctx = ui.ctx().clone();
        let outer = ui.max_rect();
        let p = chrome::PALETTE;
        let grid = Grid::over(outer);
        let mut buf = Buffer::empty(grid.all());
        // Single-lined in a preview: it does not have the keyboard.
        let inner = cells::frame(&mut buf, grid.all(), TITLE, !self.preview);
        let size = self.doc.size();
        let offset_chars = offset_text(0, size).chars().count() as u16;
        // Bytes a row: sixteen. A preview (02 §4.2) fits its rows to the side it is in: sixteen
        // where the ASCII column still fits, else eight.
        let per = if self.preview && !row_fits(inner.width, offset_chars, ROW) {
            ROW / 2
        } else {
            ROW
        };
        // The file's name on the first line, a blank line, then the rows. A preview has no
        // bars: the side's frame names the file.
        let head = if self.preview { 0 } else { 2 };
        let rows_cells = Cells::new(
            inner.x,
            inner.y + head.min(inner.height),
            inner.width,
            inner.height.saturating_sub(head),
        );
        let rows_rect = grid.rect(rows_cells);
        let bar_cells = Cells::new(inner.right(), inner.y, 1, inner.height);
        let bar = grid.rect(bar_cells);
        let total = size.div_ceil(per);
        // The whole rows of cells the frame has room for — none when notices took its height —
        // and a page of them for the keys and the cursor's place, a row at least.
        let room = u64::from(rows_cells.height);
        let visible = room.max(1);
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
        let (cw, row_h) = (chrome::CELL.x, chrome::CELL.y);

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
        // while a field (Go to, the read-only path field), a row's control or a question has it.
        if self.prompt || self.dialog_up() {
            // A question has the keyboard: the grid's lock (Tab, the arrows, Esc) is not in
            // the way of its Tab.
            ctx.memory_mut(|m| m.surrender_focus(grid_id()));
        } else if !goto_focused && !self.preview {
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
                let room = (bar.height() - 3.0 * row_h).max(1.0);
                let frac = ((at.y - bar.top() - 1.5 * row_h) / room).clamp(0.0, 1.0);
                self.top_row = (f64::from(frac) * max_top as f64).round() as u64;
            }
        }

        let hex_at = rows_cells.x + offset_chars + 1;
        let ascii_at = hex_at + per as u16 * 3 + 2;
        let hex_x = grid.rect(Cells::new(hex_at, 0, 1, 1)).left();
        let ascii_x = grid.rect(Cells::new(ascii_at, 0, 1, 1)).left();
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
        // the cursor is followed again on the next one, or `at=` would leave its row cut off.
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

        let rows = room.min(total - self.top_row);
        // The frame's one read, now that the rows on screen are known.
        let screen = self.top_row * per;
        let on_bar = bar_resp.is_pointer_button_down_on();
        self.read_screen(&ctx, screen, (rows * per).min(size - screen), on_bar);

        let body = Pen::new(p.text, p.back);
        if !self.preview && inner.height > 0 {
            let title = cells::fit_left(&self.title, inner.width.into());
            cells::put(&mut buf, inner.x, inner.y, &title, inner.width.into(), body);
        }
        if size == 0 && rows_cells.height > 0 {
            let line = cells::fit_start("empty file — nothing to show", rows_cells.width.into());
            let w = cells::width(&line) as u16;
            let x = rows_cells.x + (rows_cells.width - w) / 2;
            let y = rows_cells.y + (rows_cells.height - 1) / 2;
            cells::put(&mut buf, x, y, &line, w.into(), Pen::new(p.dim, p.back));
        }
        let wide = cells::ambiguous_wide(&ctx);
        // No cursor in a preview: nothing there is being edited.
        let cursor_at = (!self.preview).then_some(self.cursor.at);
        let end = inner.right();
        let mut caret = None;
        for k in 0..rows {
            let row = self.top_row + k;
            let y = rows_cells.y + k as u16;
            let off = row * per;
            let n = (size - off).min(per);
            let dim = Pen::new(p.dim, p.back);
            cells::put(
                &mut buf,
                rows_cells.x,
                y,
                &offset_text(off, size),
                usize::from(end - rows_cells.x),
                dim,
            );
            for i in 0..n {
                let at = off + i;
                let shown = self.doc.cell(at);
                let changed = self.doc.changed(at);
                let cursor = (Some(at) == cursor_at).then_some(self.cursor.pane);
                let pen_in = |pane: Pane| match cursor {
                    Some(typing) if typing == pane => Pen::new(p.bar_text, p.bar),
                    Some(_) => Pen::new(p.bar_text, p.bar_idle),
                    None if changed => Pen::new(p.changed, p.changed_back),
                    // On its way (06 §7.1): `··` in the skin's dim colour, never a blank.
                    None if shown == Cell::Pending => dim,
                    None => body,
                };
                let (digits, glyph) = match shown {
                    Cell::Byte(b) => (format!("{b:02X}"), column_glyph(Some(b), wide)),
                    // `·` is of width A: in an ambiguous-wide terminal its stand-in, as the
                    // ASCII column's (a printer lays content of width A in two cells).
                    Cell::Pending if wide => ("..".to_string(), column_glyph(None, wide)),
                    Cell::Pending => ("··".to_string(), column_glyph(None, wide)),
                    Cell::Unreadable => ("??".to_string(), '?'),
                };
                let hx = hex_at + hex_col(i) as u16;
                let ax = ascii_at + i as u16;
                if hx < end {
                    let room = usize::from(end - hx).min(2);
                    cells::put(&mut buf, hx, y, &digits, room, pen_in(Pane::Hex));
                }
                if ax < end {
                    cells::put(&mut buf, ax, y, &glyph.to_string(), 1, pen_in(Pane::Ascii));
                }
                if cursor == Some(Pane::Hex) && self.cursor.low && hx + 1 < end && !goto_focused {
                    // The next digit goes into the low nibble: the caret is there.
                    caret = Some(Position::new(hx + 1, y));
                }
            }
        }
        self.rows_painted = rows;
        let at = (max_top > 0).then(|| (self.top_row as f64 / max_top as f64) as f32);
        cells::scroll_bar(&mut buf, bar_cells.x, bar_cells.y, bar_cells.height, at);
        self.blank_under(&ctx, &grid, &mut buf);
        cells::paint(&ui.painter_at(outer), grid.origin, &buf, caret);
        self.layout = Some(Layout {
            rows: rows_rect,
            row_h,
            cw,
            hex_x,
            ascii_x,
        });
        self.painted = true;
    }

    /// Where the window's questions stand (10 §6): its grid between its top and its own F-key
    /// rows — hosted, the browser's between its title and status rows.
    fn dialog_grid(&self) -> Grid {
        Grid::over(self.area)
    }

    /// A question shown in this pass covers `layout`'s box and shadow (the end of the pass
    /// checks it against what was blanked under it).
    fn cover(&mut self, layout: &dialog::Layout, grid: &Grid) {
        let rects = layout.covers().into_iter().map(|c| grid.rect(c));
        self.covering.extend(rects);
    }

    /// The cells of `buf` (on `grid`) under F1's help and under the questions up blanked first — one buffer of cells laid over another (cells.rs) — so a wide glyph half
    /// under a box's edge (a Hangul name) goes as a terminal erases it.
    fn blank_under(&self, ctx: &egui::Context, grid: &Grid, buf: &mut Buffer) {
        blank_under_help(ctx, grid, buf);
        let p = chrome::PALETTE;
        for r in &self.under {
            cells::fill(buf, cells_on(grid, *r), Pen::new(p.text, p.back));
        }
    }

    /// The close prompt laid out on `grid` as it stands now; `None` while it is not up.
    fn prompt_layout(&self, grid: Grid) -> Option<dialog::Layout> {
        if !self.prompt || self.at_revision {
            return None;
        }
        let read_only = self.read_only.is_some();
        let question = if read_only {
            format!(
                "Your edits to {} are kept nowhere and cannot be saved here; close anyway?",
                self.title
            )
        } else {
            format!("Save the changes to {} before closing?", self.title)
        };
        let id = prompt_id();
        let mut buttons = vec![];
        if !read_only && self.can_replace {
            buttons.push(Button::new("Save", id.with("save")));
        }
        if !read_only && !self.can_replace {
            buttons.push(Button::new("Save copy", id.with("copy")));
        }
        if !read_only {
            buttons.push(Button::new("Save As…", id.with("save as")));
        }
        buttons.extend([
            Button::new("Discard", id.with("discard")),
            Button::new("Cancel", id.with("cancel")),
        ]);
        let mut parts = vec![
            Part::Text(question, Ink::Text),
            Part::Blank,
            Part::Buttons(buttons),
        ];
        if let Some(dir) = &self.journal_dir {
            let tilde = super::binary::tilde(dir);
            let deletes = format!("Discard deletes the kept edits at {tilde}");
            parts.push(Part::Text(deletes, Ink::Weak));
        }
        Some(dialog::layout(grid, "Unsaved changes", PROMPT_WIDE, &parts))
    }

    /// The first Save's question above 256 MiB laid out on `grid`; `None` while it is not up.
    fn confirm_layout(&self, grid: Grid) -> Option<dialog::Layout> {
        self.confirm?;
        let id = confirm_id();
        let text = format!(
            "Save to {} ({})? sopen checks its size, modification time and the pages under your \
             edits. A program writing to the file while it saves can lose that write.",
            self.title,
            crate::human(self.doc.size())
        );
        let parts = [
            Part::Text(text, Ink::Text),
            Part::Blank,
            Part::Buttons(vec![
                Button::new("Save (Enter)", id.with("save")),
                Button::new("Cancel", id.with("cancel")),
            ]),
        ];
        Some(dialog::layout(grid, "Save", CONFIRM_WIDE, &parts))
    }

    /// Write in place's dialog laid out on `grid`; `None` while it is not up.
    fn in_place_layout(&self, grid: Grid) -> Option<dialog::Layout> {
        let question = self.in_place_ask.as_ref()?;
        let id = in_place_id();
        let mut parts: Vec<Part> = (question.lines())
            .map(|l| Part::Text(l.to_string(), Ink::Text))
            .collect();
        parts.extend([
            Part::Blank,
            Part::Buttons(vec![
                Button::new("Write in place", id.with("write")),
                Button::new("Cancel", id.with("cancel")),
            ]),
        ]);
        Some(dialog::layout(
            grid,
            "Write in place",
            IN_PLACE_WIDE,
            &parts,
        ))
    }

    /// What the questions up now cover on the window's grid, their boxes and shadows (points):
    /// the window's cells under them are blanked before they are painted, in the pass that
    /// shows them.
    fn questions_cover(&self) -> Vec<Rect> {
        let grid = self.dialog_grid();
        let mut out: Vec<Cells> = [
            self.prompt_layout(grid),
            self.confirm_layout(grid),
            self.in_place_layout(grid),
        ]
        .iter()
        .flatten()
        .flat_map(dialog::Layout::covers)
        .collect();
        if let Some((modal, ..)) = &self.save_as {
            out.extend(modal.covers(grid));
        }
        out.into_iter().map(|c| grid.rect(c)).collect()
    }

    /// `Save the changes to host:/abs before closing?` — `[ Save ]` (`[ Save copy ]` where Save
    /// is off) · `[ Save As… ]` · `[ Discard ]` · `[ Cancel ]` (07 §10), in cells (10 §6, through
    /// `ops::dialog`): Tab walks the buttons, Enter or Space presses the one with the keyboard,
    /// Esc (or a click outside) is Cancel. Where Discard deletes the kept edits is a row under
    /// the buttons (cells have no hover). Read-only, with the edits kept nowhere: Discard ·
    /// Cancel.
    fn close_prompt(&mut self, ctx: &egui::Context) {
        // A file at a revision has nothing to ask about (its Discard removes the journal).
        if self.at_revision {
            self.prompt = false;
        }
        let grid = self.dialog_grid();
        let Some(layout) = self.prompt_layout(grid) else {
            return;
        };
        self.cover(&layout, &grid);
        let id = prompt_id();
        let shown = dialog::show(ctx, id, &layout, Inputs::default());
        if shown.controls.iter().any(|c| shown.clicked(*c)) {
            spend_presses(ctx);
        }
        if shown.clicked(id.with("save")) {
            self.prompt = false;
            self.start(ctx, JobKind::Save { close_after: true });
        } else if shown.clicked(id.with("copy")) {
            self.prompt = false;
            self.start(ctx, JobKind::SaveCopy { close_after: true });
        } else if shown.clicked(id.with("save as")) {
            self.prompt = false;
            self.open_save_as(true);
        } else if shown.clicked(id.with("discard")) {
            self.prompt = false;
            self.discard = true;
            self.close_ok = true;
            super::request_close(ctx);
        } else if shown.clicked(id.with("cancel")) || shown.modal.should_close() {
            self.prompt = false;
        }
    }

    /// The first Save of a file above 256 MiB (07 §4.5): what its checks cannot see, asked once
    /// a session, in cells. Enter saves — unless a button has the keyboard, which takes it as
    /// its own click — Esc cancels.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(kind) = self.confirm else {
            return;
        };
        let id = confirm_id();
        let (save_id, cancel_id) = (id.with("save"), id.with("cancel"));
        let focused = ctx.memory(|m| m.focused());
        let on_button = focused.is_some_and(|f| f == save_id || f == cancel_id);
        let enter = !on_button && bare_key(ctx, Key::Enter);
        let escape = bare_key(ctx, Key::Escape);
        let grid = self.dialog_grid();
        let Some(layout) = self.confirm_layout(grid) else {
            return;
        };
        self.cover(&layout, &grid);
        let shown = dialog::show(ctx, id, &layout, Inputs::default());
        if enter || shown.clicked(save_id) {
            self.confirm = None;
            self.confirmed = true;
            self.start(ctx, kind);
        } else if escape || shown.clicked(cancel_id) || shown.modal.should_close() {
            self.confirm = None;
        }
    }

    /// Write in place (07 §4.4): only behind this dialog, from a refusal's row; in cells, Esc
    /// cancels.
    fn in_place_dialog(&mut self, ctx: &egui::Context) {
        let id = in_place_id();
        let (write_id, cancel_id) = (id.with("write"), id.with("cancel"));
        let grid = self.dialog_grid();
        let Some(layout) = self.in_place_layout(grid) else {
            return;
        };
        let escape = bare_key(ctx, Key::Escape);
        self.cover(&layout, &grid);
        let shown = dialog::show(ctx, id, &layout, Inputs::default());
        if shown.clicked(write_id) {
            self.in_place_ask = None;
            self.start(ctx, JobKind::InPlace);
        } else if escape || shown.clicked(cancel_id) || shown.modal.should_close() {
            self.in_place_ask = None;
        }
    }

    /// The live session's answer to its question about a temp (07 §3.1), once it is back: the
    /// question ends, or the status row says why not. (Its row, Remove it or Keep it, is one of
    /// the rows below the grid; the next Save waits for the answer, since the record that holds
    /// the question is that Save's to replace.)
    fn poll_left(&mut self) {
        if let Some(rx) = &self.answering {
            match rx.try_recv() {
                Ok(Ok(line)) => {
                    self.answering = None;
                    self.left = None;
                    self.status = Status::info(line);
                }
                Ok(Err(why)) => {
                    self.answering = None;
                    self.status = Status::error(why);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.answering = None,
            }
        }
    }

    /// The owner's answer to the live question, on the `Origin`, off the frame.
    fn answer_left(&mut self, ctx: &egui::Context, remove: bool) {
        // Both answers end the record, a write; Remove it removes a file: not for a file at a
        // revision (the question stays in its record for the next open).
        if self.at_revision {
            return self.status = Status::error(format!("cannot answer: {AT_A_REVISION}"));
        }
        let Some(left) = self.left.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let (file, journal, ctx) = (self.file.clone(), self.journal.clone(), ctx.clone());
        std::thread::spawn(move || {
            let answered = file
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .answer_left(&left, remove, &journal)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(answered);
            ctx.request_repaint();
        });
        self.answering = Some(rx);
        self.status = Status::info(if remove {
            "removing…"
        } else {
            "keeping it…"
        });
    }

    /// `SOPEN_SAVE_AS` in a window (a receipt): once the state is on screen, the Save As modal
    /// opens on that folder and its Save is pressed.
    fn save_as_receipt(&mut self, ctx: &egui::Context) {
        if self.save_as_hook.is_none()
            || !self.state_patches.is_empty()
            || self.recovering.is_some()
            || !self.painted
        {
            return;
        }
        let folder = self.save_as_hook.take();
        self.save_as_folder = folder;
        self.open_save_as(false);
        ctx.input_mut(|i| {
            i.events.push(egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            })
        });
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
        let whole = ui.max_rect();
        self.wake = Some(ctx.clone());
        self.drop_held(&ctx);
        // Under the questions up (the area as last laid out): the toolbar's cells.
        self.under = self.questions_cover();
        self.start_recovery(&ctx);
        self.poll_recovery();
        self.poll_left();
        // Losing the keyboard focus writes the edits at once (07 §3).
        let focused = ctx.input(|i| i.focused);
        if self.focused && !focused && self.job.is_none() {
            self.write_generation(true);
        }
        self.focused = focused;
        if self.state_patches.is_empty() && std::mem::take(&mut self.ask_close) {
            // `SOPEN_STATE` `close=ask`: as if closing were asked for (the prompt's receipt),
            // once the state's patches are in.
            self.close_requested();
        }
        self.save_as_receipt(&ctx);
        // F6 is PC Shell's Goto, as Cmd/Ctrl+G — but a plain key: not while a field (Go to,
        // the read-only path field) has the typing.
        let cmd_g = !self.locked()
            && (super::command_key(&ctx, Key::G, false)
                || (!ctx.text_edit_focused() && bare_key(&ctx, Key::F6)));
        if cmd_g {
            self.goto.clear();
            ctx.memory_mut(|m| m.request_focus(goto_id()));
        }
        // The toolbar, its row of cells.
        let goto_took = egui::Panel::top("hexbar")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .resizable(false)
            .exact_size(chrome::CELL.y)
            .show(ui, |ui| self.toolbar(ui))
            .inner;
        // Whether the field has the keyboard is asked of egui's memory, not of a response's
        // `has_focus`, which is also false whenever the platform says the window itself is not
        // focused (the owner in another application for a moment, a window that came up behind
        // another one). The grid would take the keyboard back in such a pass, and the offset
        // typed next would go over bytes.
        let goto_has = ctx.memory(|m| m.has_focus(goto_id()));
        let goto_owns = cmd_g || goto_has || goto_took;
        // The F-key row, the window's last (on its own: the browser around it has the row).
        super::own_keys(ui, HEX_KEYS);
        // The questions stand between the window's top and its own F-key rows.
        let above_keys = ui.available_rect_before_wrap().bottom();
        self.area = Rect::from_min_max(whole.min, egui::pos2(whole.right(), above_keys));
        // The rows below the grid's controls, Tab's way into them; one with the keyboard (the
        // read-only window's path field among them) keeps this frame's keys.
        let cols = (ui.available_width() / chrome::CELL.x).floor() as u16;
        let controls = self.below_controls(cols);
        self.keys(&ctx, goto_owns, &controls);
        // A question the keys opened or answered: the rest of the window's cells under it.
        self.under = self.questions_cover();
        // A report that lands while the button is down waits until it is let go. Its rows sit
        // below the grid: they take rows off its bottom, never move its top or its rows.
        if !ctx.input(|i| i.pointer.primary_down()) {
            if let Some(next) = self.pending_notices.take() {
                self.notices = next;
            }
        }
        egui::Panel::bottom("status")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .resizable(false)
            .exact_size(chrome::CELL.y)
            .show(ui, |ui| self.status_bar(ui));
        // The rows below the grid, whole rows of cells.
        let rows = self.below(cols);
        if !rows.is_empty() {
            let pressed = egui::Panel::bottom("disclosures")
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .resizable(false)
                .exact_size(rows.len() as f32 * chrome::CELL.y)
                .show(ui, |ui| self.below_ui(ui, &rows))
                .inner;
            if let Some(press) = pressed {
                // Pressed by Enter or Space (not a click): that key is held back until let go.
                let by_key = [Key::Enter, Key::Space]
                    .into_iter()
                    .find(|k| ctx.input(|i| i.key_pressed(*k)));
                spend_presses(&ctx);
                self.below_press(&ctx, press);
                self.held = by_key.or(self.held);
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                // A field or a row's control that has the keyboard (a click into the path field
                // this frame) keeps it: the grid does not take it back.
                let focused = ctx.memory(|m| m.focused());
                let row_owns = focused.is_some_and(|f| self.below_controls(cols).contains(&f));
                self.grid(ui, goto_owns || row_owns)
            });
        self.poll_job(&ctx);
        self.tick_pass();
        self.tick_journal(&ctx);
        self.close_prompt(&ctx);
        self.confirm_dialog(&ctx);
        self.in_place_dialog(&ctx);
        self.show_save_as(&ctx);
        if let Some((modal, ..)) = &self.save_as {
            let grid = self.dialog_grid();
            let rects: Vec<Rect> = modal
                .covers(grid)
                .into_iter()
                .map(|c| grid.rect(c))
                .collect();
            self.covering.extend(rects);
        }
        // A question opened or moved after the window's cells were painted: the next pass
        // blanks the cells under it (or under it no more).
        let covered = std::mem::take(&mut self.covering);
        if covered != self.under {
            ctx.request_repaint();
        }
        let focused = ctx.memory(|m| m.focused());
        self.row_focus = focused.is_some_and(|f| self.below_controls(cols).contains(&f));
    }

    /// Over a stream (06 §11): once the state's patches are in and the screen's bytes have
    /// arrived or failed — or, with `loading=1`, at the first frame with rows, those still
    /// loading shot as they are.
    /// And once what an earlier session left has been listed (its rows on screen). Under
    /// `SOPEN_SAVE_AS` (a receipt), once that Save As holds or has ended. With the close prompt
    /// up, once the edits are in a generation: the prompt then says where Discard deletes them
    /// (a row more, which moves its box), and the status row where they are kept.
    fn ready(&self) -> bool {
        let save_as = match &self.save_as {
            Some((modal, ..)) => !modal.busy() || self.save_as_held.load(Ordering::SeqCst),
            None => self.save_as_hook.is_none(),
        };
        let asked = !self.prompt || (self.writing.is_none() && self.schedule.due().is_none());
        asked
            && self.painted
            && self.job.is_none()
            && self.state_patches.is_empty()
            && self.reloading.is_none()
            && (self.preview
                || self.at_revision
                || (self.recovery_started && self.recovering.is_none()))
            && save_as
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

    /// A file at a revision (03 §6.3): a viewer only — no byte is typed over, Save, Save copy
    /// and Save As are off with [`AT_A_REVISION`] on hover and refused with it, and no journal
    /// is kept or looked for. Patches of a state or a Restore still to land are dropped.
    fn set_read_only(&mut self, read_only: bool) {
        self.at_revision = read_only;
        if read_only {
            self.state_patches.clear();
            // Told late (its opener says so before the first pass): an earlier session's rows
            // go unanswered — their Restore, Discard and Remove all write — and so do the
            // close prompt, the questions before a Save (its size, Write in place), the
            // question about an interrupted save's file and a Save As not writing yet. A save
            // already writing finishes.
            self.rows.clear();
            self.recovering = None;
            self.prompt = false;
            self.confirm = None;
            self.in_place_ask = None;
            if self.answering.is_none() {
                self.left = None;
            }
            if self
                .save_as
                .as_ref()
                .is_some_and(|(modal, ..)| !modal.busy())
            {
                self.save_as = None;
            }
        }
    }

    /// Held without being drawn, a streamed window does not poll its file (06 §5).
    fn set_shown(&mut self, shown: bool) {
        if let Some(stream) = &self.stream {
            stream.set_visible(shown);
        }
    }

    /// The Hex view of the browser's viewer (D35): viewing from now on, also each time it is
    /// shown again; its typing column kept.
    fn set_in_viewer(&mut self, in_viewer: bool) {
        self.in_viewer = in_viewer;
        self.editing = false;
    }

    /// A question up (the close prompt, Save As, a confirmation, Write in place) is the view
    /// keys' Question; else the hex view's viewing or editing (D35), read-only for a revision or
    /// a file turned read-only, saving while a Save Esc stops runs.
    fn view_state(&mut self) -> crate::view_keys::State {
        if self.prompt || self.dialog_up() || self.row_focus {
            return crate::view_keys::State::Question;
        }
        crate::view_keys::State::Hex(crate::view_keys::Hex {
            editing: self.editing,
            read_only: self.at_revision || self.read_only.is_some(),
            saving: self.job.is_some() && self.stream.is_some(),
        })
    }

    fn hold(&mut self) -> crate::views::Hold {
        if self.job.is_some() {
            crate::views::Hold::Saving
        } else if self.doc.unsaved() {
            crate::views::Hold::Unsaved
        } else {
            crate::views::Hold::Clean
        }
    }

    fn view_key(&mut self, command: crate::view_keys::Command) {
        use crate::view_keys::{Command, Ending};
        match command {
            Command::StartEditing => {
                self.editing = true;
                self.status = Status::default();
            }
            Command::EndEditing | Command::Arm(Ending::Editing) => {
                self.editing = false;
                self.status = Status::default();
            }
            Command::ReadOnly => {
                let why = match &self.read_only {
                    Some(_) if !self.at_revision => self.read_only_why.as_str(),
                    _ => AT_A_REVISION,
                };
                self.status = Status::error(format!("cannot edit: {why}"));
            }
            _ => {}
        }
    }

    fn byte_place(&self) -> Option<u64> {
        Some(self.cursor.at)
    }

    fn saves_landed(&self) -> u64 {
        self.saves_landed
    }

    /// The cursor on `at`, its row at the top (08 D11).
    fn go_to_byte(&mut self, at: u64) {
        let at = self.last().map_or(at, |last| at.min(last));
        self.cursor.at = at;
        self.cursor.low = false;
        self.top_row = at / ROW;
        self.follow = true;
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok || self.preview {
            return true;
        }
        // A file at a revision has nothing to ask about: nothing of it can be saved anywhere.
        if self.at_revision && self.job.is_none() && self.save_as.is_none() {
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
        if let Some((modal, ..)) = &self.save_as {
            // A Save As under way is stopped, and the modal says so; the window asks again.
            if modal.busy() {
                modal.cancel();
                return false;
            }
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
        // Restored bytes still landing are edits written nowhere yet.
        if !self.doc.unsaved() && self.state_patches.is_empty() {
            return true;
        }
        // Closing keeps the edits at once (07 §3), on a worker — the frame never waits for the
        // journal — rather than at the schedule's next tick: the prompt says where Discard
        // deletes them as soon as it can, and the window is not settled until it does
        // ([`Window::ready`]). Not while an answer about a temp holds the journal: the
        // schedule's tick keeps them once it is back.
        if !self.prompt {
            self.note_revision(Instant::now());
            let keeping = self.schedule.status() == Keep::Keeping;
            let free = self.job.is_none() && self.answering.is_none() && self.removing.is_none();
            if keeping && free && self.writing.is_none() {
                self.write_generation(false);
            }
        }
        self.prompt = true;
        false
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod preview_tests;
