//! The built-in editor's screen (`.prd/12-editor.md` §4, §8, E10, E16, E19, E20; ED-screen):
//! the edit window in cells over [`Editor`] — its frame with `[■]`, the name, `1` and `[↕]` in
//! the top border and `* line:col` in the bottom one, the gutter, the rows in syntect's colours,
//! the caret, the block and the scroll bar — and the rows around it: the title row's menu names,
//! the status row's fields and armings, the F-key row. Nothing opens it yet: F4 and the layout
//! are ED-layout's; the dialogs and the menus are [`dialogs`]' (ED-dialogs).
//!
//! - **Highlighting is the viewer's** (E18): its [`Highlight`], on a worker of the viewer's pool,
//!   reads a [`LineSource`] over a snapshot of the buffer ([`Snap`]: the original shared, the
//!   typed bytes copied once) — never the live buffer, never on a frame. One ask waits for it at
//!   most ([`Mailbox`]: a newer one replaces it, keeping the earlier changed line), and an edit
//!   makes the page asked before it stale at once, so no reply is laid on another document. A
//!   line is looked at no further than [`LINE_MAX`] past its start, a newer ask stopping a long
//!   read; the page's first line is found by the buffer's own index. Its kept states stand every
//!   [`STATE_LINES`] lines (E19; EH-3 measures them). An edit drops the states after the first
//!   changed line's; the rows keep their old colours, laid around what changed, until the worker
//!   answers for the document as it is now (EH-1). Highlighting stops where the viewer's does
//!   (EH-2): at a line over [`LINE_MAX`], which the walk finds and never parses, and past
//!   [`HIGHLIGHT_MAX`] at the viewer's last index mark below it.
//! - **One row per line** (E16), scrolled sideways to keep the caret's character, and a preedit
//!   at it, whole in view — after a resize too. A row is laid in the file's order, a character
//!   per cell — two for a wide one, a tab to the next multiple of [`TAB`], a control character
//!   its code page 437 glyph (the viewer's stand-in in an ambiguous-wide terminal), a cell
//!   taking [`MARKS`] combining marks: `edit::ops`' column rule, so the cell the caret is drawn
//!   in is the column ↑ ↓ keep. A long line is walked from checkpoints every [`STRIDE`] bytes,
//!   kept across an edit before the place it changed, so a row or the caret deep in it decodes
//!   about a stride; more than a stride of marks ends a row.
//! - **The caret** is two scan lines in Insert and its character's cells, their colours swapped,
//!   in Overwrite (§4's [추정] choice), in both backends — the window's foot covered there
//!   ([`EditView::paint`]); a terminal adds its cursor.
//! - **The input method** (E10, E20): the window's preedit is drawn over the cells at the caret,
//!   and the IME rectangle set there — held to the text's edges while the caret is scrolled
//!   away — whenever the editor has the keyboard, but not while an arming waits for a letter,
//!   which then matches the physical key (`edit::arm`'s rule).
//! - **Keys** go through `edit::arm::Keys` ([`EditView::press`]): edits are made here, the rest
//!   (Save, leaving, the switch) handed back to the binding (ED-layout), as are the frame's `[↕]`
//!   and the `^S` prompt's `[Save]` clicked ([`EditView::take_commands`]). F10's menus, Find, Go
//!   to and the prompts are [`dialogs`]' (ED-dialogs): while one is up the keys go to it alone.

use super::cells::{self, Buffer, Cells, Grid, Pen, Position};
use super::chrome::{self, cga, FKey, Strip};
use super::text::{self, Highlight, Line, LineSource, Pool, Runs, Slot, HIGHLIGHT_MAX, LINE_MAX};
use super::Window;
use crate::edit::arm::{save_over, Armed, Backend, Command, Keys, Press};
use crate::edit::buffer::{Buffer as Doc, Original, Piece, Span};
use crate::edit::ops::{Clip, Editor, Said, Sel, TAB};
use crate::encoding::{Encoding, Pick};
use crate::journal::Keep;
use anyhow::Result;
use eframe::egui::{self, Color32};
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use syntect::parsing::SyntaxReference;
use unicode_width::UnicodeWidthChar;

mod dialogs;
pub use dialogs::{Chosen, Exit, FileState};

/// The editor's kept highlighting states stand every this many lines (E19): at most about 5 ms
/// of parsing a keystroke. EH-3 holds their heap over a 16 MiB Rust file to 32 MiB at it.
pub const STATE_LINES: u64 = 64;

/// The gutter's digits at the least: five cells with its blanks, as §4 lays 80 columns out.
const DIGITS: usize = 3;

/// Bytes read at a time while a line is walked.
const CHUNK: u64 = 16 * 1024;

/// A long line keeps a checkpoint (a character start and its column) every this many bytes.
pub const STRIDE: u64 = 64 * 1024;

/// Lines whose checkpoints are kept; past it they all go (the next walk makes them again).
const WALKED_LINES: usize = 64;

/// Bytes past a line's start, beyond [`LINE_MAX`], its end is looked for in: a BOM and an LF unit.
const LOOK: u64 = 8;

/// The cells of the file's name the `^S` prompt keeps at the least (R1 S10).
const NAME_MIN: usize = 12;

/// Zero-width characters (combining marks) a cell takes after its glyph, as a terminal keeps a
/// few; the rest of a run is not drawn (R1 MUST 6).
const MARKS: usize = 8;

/// The title row's menus (§4): each name's first letter chooses it.
pub const MENUS: [&str; 6] = ["File", "Edit", "Search", "Block", "Git", "Options"];

/// The F-key row while the editor has the keyboard (§4): `1Help 2Save 3Exit 4View 5Save 6Panel
/// 7SaveAs 8 9View 10Menu`.
pub const KEYS: &[FKey] = &[
    chrome::HELP_KEY,
    key(2, "Save"),
    key(3, "Exit"),
    key(4, "View"),
    key(5, "Save"),
    key(6, "Panel"),
    key(7, "SaveAs"),
    key(9, "View"),
    key(10, "Menu"),
];

const fn key(n: u8, label: &'static str) -> FKey {
    FKey { n, label }
}

#[cfg(test)]
thread_local! {
    /// The bytes this thread's snapshots read, and its rows and carets decoded: the tests'
    /// measure of the work.
    static SNAP_READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static DECODED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The cells `c` takes at column `col`: `edit::ops`' rule — a tab to the next multiple of
/// [`TAB`], a control character its one glyph, else its width (a mark none).
fn width(c: char, col: u64) -> u64 {
    match c {
        '\t' => TAB - col % TAB,
        c if c.is_control() => 1,
        c => c.width().unwrap_or(1) as u64,
    }
}

/// What a cell shows for `c`: a control character as its code page 437 glyph, a C1 control as
/// `·` (08 D32), anything else as itself.
fn face(c: char) -> char {
    if ('\u{80}'..='\u{9F}').contains(&c) {
        '·'
    } else {
        chrome::shown_char(c)
    }
}

/// Whether `c`'s face is a glyph of sopen's own (a C0 control or DEL as its code page 437 glyph,
/// a C1 control as `·`): an ambiguous-wide terminal is sent the viewer's stand-in for it.
fn own(c: char) -> bool {
    (c < ' ' && c != '\t') || c == '\u{7F}' || ('\u{80}'..='\u{9F}').contains(&c)
}

/// A line's text as the highlighter and the rows both read it: `bytes` from `at` (a line's
/// start, after the BOM on the first), up to its LF or [`LINE_MAX`] bytes (`cut`) — the CR
/// before an LF (`ended`) is the line's end, not its text (E35); a lone CR is a character.
fn line_text(enc: Encoding, at: u64, mut bytes: Vec<u8>, cut: bool, ended: bool) -> String {
    if !cut && ended && enc.ends_with_cr(&bytes, at) {
        bytes.truncate(bytes.len() - enc.unit());
    }
    let (cells, _) = enc.decode(&bytes, !cut);
    cells.iter().map(|c| c.ch).collect()
}

// ---------------------------------------------------------------------------------------------
// The worker's side: the buffer as a line source.

/// The document as the worker reads it, never the live buffer: the original shared, the pieces
/// as spans, the typed bytes they name in chunks copied once ([`Typed`]).
#[derive(Clone)]
struct Snap {
    original: Original,
    /// Each piece's span and where it starts in the document.
    spans: Arc<[(u64, Span)]>,
    typed: Arc<[(u64, Arc<[u8]>)]>,
    len: u64,
    /// The document's lines (the viewer's stop past [`HIGHLIGHT_MAX`] is the worker's to find:
    /// [`BufLines::viewer_stop`]).
    lines: u64,
    enc: Encoding,
    bom: u64,
}

impl Snap {
    /// Appends the bytes of `range` to `out`.
    fn read(&self, range: Range<u64>, out: &mut Vec<u8>) {
        let end = range.end.min(self.len);
        let mut at = range.start;
        #[cfg(test)]
        SNAP_READ.with(|n| n.set(n.get() + end.saturating_sub(at)));
        let mut i = self
            .spans
            .partition_point(|(s, _)| *s <= at)
            .saturating_sub(1);
        while at < end && i < self.spans.len() {
            let (s, span) = &self.spans[i];
            let r = match span {
                Span::Original { range, .. } | Span::Add(range) => range,
            };
            let to = end.min(s + (r.end - r.start));
            let (a, b) = (r.start + (at - s), r.start + (to - s));
            match span {
                Span::Original { .. } => {
                    out.extend_from_slice(&self.original.bytes()[a as usize..b as usize])
                }
                Span::Add(_) => {
                    let mut k = self
                        .typed
                        .partition_point(|(s, _)| *s <= a)
                        .saturating_sub(1);
                    let mut x = a;
                    while x < b {
                        let (s, bytes) = &self.typed[k];
                        let upto = b.min(s + bytes.len() as u64);
                        out.extend_from_slice(&bytes[(x - s) as usize..(upto - s) as usize]);
                        (x, k) = (upto, k + 1);
                    }
                }
            }
            (at, i) = (to, i + 1);
        }
    }

    /// Where the line from `at` ends, looked for no further than [`LINE_MAX`] (and [`LOOK`])
    /// past `at`: its LF unit's start and the next line's start — the document's end for the
    /// last line — or `None` for a longer line, whose end is never looked for (R1 MUST 1).
    fn line_end(&self, at: u64) -> Option<(u64, u64)> {
        let limit = (at + LINE_MAX as u64 + LOOK).min(self.len);
        // A short read first, as most lines are short, then longer ones.
        let (mut x, mut step) = (at, 256);
        let mut bytes = Vec::new();
        while x < limit {
            bytes.clear();
            self.read(x..(x + step).min(limit), &mut bytes);
            if let Some(i) = self.enc.find_lf(&bytes, x) {
                let lf = x + i as u64;
                return Some((lf, lf + self.enc.lf().len() as u64));
            }
            (x, step) = (x + bytes.len() as u64, step * 4);
        }
        (limit == self.len).then_some((self.len, self.len))
    }
}

/// The typed bytes as the worker's snapshots share them: the add buffer only grows, so what is
/// copied once stays true. A short last chunk takes the next bytes, so chunks stay few.
#[derive(Default)]
struct Typed {
    chunks: Vec<(u64, Arc<[u8]>)>,
    len: u64,
}

impl Typed {
    fn catch_up(&mut self, b: &Doc<Sel>) {
        if b.add_len() <= self.len {
            return;
        }
        let mut new = Vec::new();
        for c in b.add_chunks(self.len..b.add_len()) {
            new.extend_from_slice(c);
        }
        match self.chunks.last_mut() {
            Some((_, last)) if last.len() < STRIDE as usize => {
                let mut joined = last.to_vec();
                joined.extend_from_slice(&new);
                *last = joined.into();
            }
            _ => self.chunks.push((self.len, new.into())),
        }
        self.len = b.add_len();
    }
}

/// A read stopped for a newer ask, or because the view went: never an error to show.
#[derive(Debug)]
struct Superseded;

impl std::fmt::Display for Superseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a newer page was asked for")
    }
}

impl std::error::Error for Superseded {}

/// The buffer as the highlighter reads it: lines found from the nearest start known — line 0,
/// a kept state's line, the line after the last one read, or the page's first line as the
/// buffer's own index gives it (12 §7.1) — each looked at no further than [`LINE_MAX`] past
/// its start. A line longer than that comes cut and ends the read (the walk stops there, as the
/// viewer's does: [`LineSource::finds_stop`]); so does the viewer's stop past [`HIGHLIGHT_MAX`]
/// ([`BufLines::viewer_stop`]).
struct BufLines {
    snap: Snap,
    /// Where the line of kept state `i` (line `i` × [`STATE_LINES`]) starts, as far as a read
    /// went.
    starts: Vec<u64>,
    /// The line after the last one read, and where it starts.
    next: Option<(u64, u64)>,
    /// The page's first line, and where the buffer's index says it starts.
    known: Option<(u64, u64)>,
    /// The text viewer's index marks within [`HIGHLIGHT_MAX`] as far as found, and its stop.
    marks: Vec<(u64, u64)>,
    stop: Option<u64>,
    /// A newer ask came or the view went: a read stops between lines ([`Superseded`]).
    cancel: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl BufLines {
    fn new(snap: Snap) -> Self {
        BufLines {
            snap,
            starts: vec![0],
            next: None,
            known: None,
            marks: vec![(0, 0)],
            stop: None,
            cancel: None,
        }
    }

    /// The document is `snap` now, changed from line `edit` on — the starts and the viewer's
    /// marks after it go, and its stop is found again — and its page starts as `known` says.
    fn renew(&mut self, snap: Snap, edit: Option<u64>, known: Option<(u64, u64)>) {
        self.snap = snap;
        self.known = known;
        if let Some(line) = edit {
            self.starts.truncate((line / STATE_LINES) as usize + 1);
            self.next = self.next.filter(|&(n, _)| n <= line);
            self.marks.retain(|&(n, _)| n <= line);
            self.stop = None;
        }
    }

    fn cancelled(&self) -> Result<()> {
        match self.cancel.as_ref().is_some_and(|c| c()) {
            true => Err(anyhow::Error::new(Superseded)),
            false => Ok(()),
        }
    }

    /// Where the text viewer stops highlighting a document past [`HIGHLIGHT_MAX`] (`text.rs`'s
    /// `share_first`, R1 S5): at the line of its last index mark at or below it — a mark every
    /// `BLOCK_LINES` lines, or at the first line start a `SPAN` past the last. `None` for a
    /// shorter document, highlighted whole. One pass over the line starts from the last mark
    /// kept, a chunk at a time, a newer ask stopping it; a line longer than [`LINE_MAX`] ends it
    /// there, where highlighting stops anyway.
    fn viewer_stop(&mut self) -> Result<Option<u64>> {
        if self.snap.len <= HIGHLIGHT_MAX || self.stop.is_some() {
            return Ok(self.stop);
        }
        let (enc, len) = (self.snap.enc, self.snap.len);
        let lf = enc.lf().len() as u64;
        let (mut last, mut last_at) = *self.marks.last().expect("line 0's mark");
        let (mut line, mut start, mut x) = (last, last_at, last_at);
        let mut bytes = Vec::new();
        let stop = 'scan: loop {
            if x >= len {
                break last;
            }
            self.cancelled()?;
            let to = (x + CHUNK).min(len);
            bytes.clear();
            self.snap.read(x..to, &mut bytes);
            let mut k = 0;
            while let Some(i) = enc.find_lf(&bytes[k..], x + k as u64) {
                if x + (k + i) as u64 - start > LINE_MAX as u64 + LOOK {
                    break 'scan line;
                }
                k += i + lf as usize;
                (line, start) = (line + 1, x + k as u64);
                if line - last >= text::BLOCK_LINES || start - last_at >= text::SPAN {
                    if start > HIGHLIGHT_MAX {
                        break 'scan last;
                    }
                    (last, last_at) = (line, start);
                    self.marks.push((line, start));
                }
            }
            x = to;
            if x < len && x - start > LINE_MAX as u64 + LOOK {
                break line;
            }
        };
        self.stop = Some(stop);
        Ok(self.stop)
    }
}

impl LineSource for BufLines {
    fn read_lines(&mut self, first: u64, count: usize) -> Result<Vec<Line>> {
        let stop = self.viewer_stop()?;
        let last = (first.saturating_add(count as u64).min(self.snap.lines))
            .min(stop.map_or(u64::MAX, |s| s + 1));
        let mut out = Vec::new();
        if first >= last {
            return Ok(out);
        }
        let i = ((first / STATE_LINES) as usize).min(self.starts.len() - 1);
        let mut from = (i as u64 * STATE_LINES, self.starts[i]);
        for known in [self.next, self.known].into_iter().flatten() {
            if known.0 <= first && known.0 > from.0 {
                from = known;
            }
        }
        let (mut line, mut at) = from;
        let enc = self.snap.enc;
        while line < last {
            self.cancelled()?;
            if line.is_multiple_of(STATE_LINES) && line / STATE_LINES == self.starts.len() as u64 {
                self.starts.push(at);
            }
            let text_at = if line == 0 { self.snap.bom } else { at };
            let end = self.snap.line_end(at);
            let cut = Some(line) == stop
                || end.is_none_or(|(lf, _)| lf.saturating_sub(text_at) > LINE_MAX as u64);
            if line >= first {
                let lf = end.map_or(text_at + LINE_MAX as u64, |(lf, _)| lf);
                let from = text_at.min(lf);
                let mut bytes = Vec::new();
                self.snap
                    .read(from..lf.min(from + LINE_MAX as u64), &mut bytes);
                let ended = end.is_some_and(|(lf, next)| next > lf);
                let text = line_text(enc, from, bytes, cut, ended);
                out.push(Line { text, cut });
            }
            match end.filter(|_| !cut) {
                Some((_, next)) => (line, at) = (line + 1, next),
                // The read ends with a cut line: nothing after it is looked for.
                None => return Ok(out),
            }
        }
        self.next = Some((line, at));
        Ok(out)
    }

    fn mark_before(&self, line: u64) -> usize {
        (line / STATE_LINES) as usize
    }

    fn mark_line(&self, i: usize) -> u64 {
        (i as u64 * STATE_LINES).min(self.snap.lines)
    }

    fn finds_stop(&self) -> bool {
        true
    }
}

/// A page the view wants highlighted, under generation `gen`, of the document `snap`: its
/// first line `top` and where that starts (the buffer's index), `count` lines; `edit`: the first
/// line changed since the last ask.
struct Ask {
    gen: u64,
    snap: Snap,
    edit: Option<u64>,
    top: (u64, u64),
    count: usize,
}

enum Done {
    /// The page of `count` lines at `top` asked under `gen`: the runs of the lines the worker
    /// read, and where highlighting stops.
    Page {
        gen: u64,
        top: u64,
        count: usize,
        runs: Vec<Runs>,
        stop: u64,
    },
    Failed(String),
}

/// The earlier of two changed lines (an ask's `edit`).
fn earlier(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// The newest ask, waiting for the worker: one at most — a newer one replaces it, keeping the
/// earlier changed line of the two — so a held worker never piles snapshots up (R1 MUST 2).
#[derive(Default)]
struct Mailbox {
    ask: Mutex<Option<Ask>>,
    ready: Condvar,
}

impl Mailbox {
    fn put(&self, mut ask: Ask) {
        let mut slot = lock(&self.ask);
        if let Some(old) = slot.take() {
            ask.edit = earlier(old.edit, ask.edit);
        }
        *slot = Some(ask);
        self.ready.notify_one();
    }
}

/// The highlighting worker, from the view's side: dropping it tells it to stop; it is never
/// joined (a syntect call in progress ends on its own).
struct Worker {
    mail: Arc<Mailbox>,
    done: Receiver<Done>,
    latest: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    #[cfg_attr(not(test), allow(dead_code))]
    parsed: Arc<AtomicU64>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Under the lock: the worker sees the stop before it waits, or is woken from it.
        let _held = lock(&self.mail.ask);
        self.mail.ready.notify_all();
    }
}

impl Worker {
    /// Starts `hl` on a thread of its own when `pool` has a slot: it takes the newest ask, its
    /// edits and those of every ask it replaced applied first.
    fn start(mut hl: Highlight, ctx: egui::Context, pool: &Arc<Pool>) -> Option<Worker> {
        let slot: Slot = pool.admit()?;
        let mail = Arc::new(Mailbox::default());
        let (tx, done) = mpsc::channel();
        let latest = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let parsed = hl.parsed();
        let current = Arc::new(AtomicU64::new(0));
        let cancel: Arc<dyn Fn() -> bool + Send + Sync> = {
            let (l, st, c) = (latest.clone(), stop.clone(), current.clone());
            Arc::new(move || {
                st.load(Ordering::Relaxed) || l.load(Ordering::Relaxed) != c.load(Ordering::Relaxed)
            })
        };
        let (mailbox, st) = (mail.clone(), stop.clone());
        std::thread::spawn(move || {
            let _slot = slot;
            let wake = ctx.clone();
            let send = |done: Done| {
                let _ = tx.send(done);
                wake.request_repaint();
            };
            let work = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut src: Option<BufLines> = None;
                loop {
                    let ask = {
                        let mut waiting = lock(&mailbox.ask);
                        loop {
                            if st.load(Ordering::SeqCst) {
                                return;
                            }
                            if let Some(ask) = waiting.take() {
                                break ask;
                            }
                            waiting =
                                (mailbox.ready.wait(waiting)).unwrap_or_else(|p| p.into_inner());
                        }
                    };
                    current.store(ask.gen, Ordering::Relaxed);
                    let src = match &mut src {
                        Some(s) => {
                            s.renew(ask.snap, ask.edit, Some(ask.top));
                            s
                        }
                        None => {
                            let mut s = BufLines::new(ask.snap);
                            (s.known, s.cancel) = (Some(ask.top), Some(cancel.clone()));
                            src.insert(s)
                        }
                    };
                    if let Some(line) = ask.edit {
                        hl.edited(src.mark_before(line));
                    }
                    let gen = ask.gen;
                    match hl.page(src, ask.top.0, ask.count, &|| cancel()) {
                        Ok(Some(runs)) => send(Done::Page {
                            gen,
                            top: ask.top.0,
                            count: ask.count,
                            runs,
                            stop: hl.stop(),
                        }),
                        Ok(None) => {}
                        Err(e) if e.is::<Superseded>() => {}
                        Err(e) => {
                            send(Done::Failed(format!("{e:#}")));
                            break;
                        }
                    }
                }
            }));
            if work.is_err() {
                send(Done::Failed("the highlighter panicked".to_string()));
            }
        });
        Some(Worker {
            mail,
            done,
            latest,
            stop,
            parsed,
        })
    }

    /// Asks waiting for the worker (the tests).
    #[cfg(test)]
    fn pending(&self) -> usize {
        usize::from(lock(&self.mail.ask).is_some())
    }
}

/// A line's colours as the worker gave them, a colour per character, with the text they are of.
struct Painted {
    text: Vec<char>,
    colors: Vec<Color32>,
}

/// Highlighting from the view's side: the worker, what was asked last, and the colours of the
/// lines around the screen.
#[derive(Default)]
struct Hl {
    worker: Option<Worker>,
    /// Tried (a syntax, a slot): not again until the document is read again.
    tried: bool,
    /// The original's generation it is of (a re-read starts highlighting over).
    generation: u32,
    gen: u64,
    /// The newest ask: the document's version, the top line, the rows.
    asked: Option<(u64, u64, usize)>,
    answered: bool,
    /// The first line changed since the last ask.
    edit: Option<u64>,
    lines: BTreeMap<u64, Painted>,
    typed: Typed,
    /// Why it is off or stopped, once.
    status: Option<String>,
}

impl Hl {
    /// The colours of line `n` showing `text`: the worker's, or — while the line changed since
    /// they were made (EH-1) — its old ones laid around the change, the characters typed in the
    /// colour of the one before them. `None`: plain.
    fn colors(&self, n: u64, text: &[char]) -> Option<Vec<Color32>> {
        let p = self.lines.get(&n)?;
        if p.text == text {
            return Some(p.colors.clone());
        }
        let same = |a: &[char], b: &[char]| a.iter().zip(b).take_while(|(x, y)| x == y).count();
        let pre = same(&p.text, text);
        let rev = |t: &[char]| t.iter().rev().copied().collect::<Vec<_>>();
        let post = same(&rev(&p.text[pre..]), &rev(&text[pre..]));
        let new = text.len() - pre - post;
        let fill = p.colors.get(pre.wrapping_sub(1)).or(p.colors.get(pre));
        let mut out = p.colors[..pre].to_vec();
        out.extend(std::iter::repeat_n(fill.copied()?, new));
        out.extend_from_slice(&p.colors[p.colors.len() - post..]);
        Some(out)
    }

    /// The document changed from line `line` on, `delta` lines more: the colours below it move
    /// with their lines.
    fn edited(&mut self, line: u64, delta: i64) {
        self.edit = Some(self.edit.map_or(line, |e| e.min(line)));
        if delta != 0 {
            let below = self.lines.split_off(&(line + 1));
            for (n, p) in below {
                if let Some(n) = n.checked_add_signed(delta).filter(|&n| n > line) {
                    self.lines.insert(n, p);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Walking a line.

/// A long line's checkpoints, per line start: a character start and its column every
/// [`STRIDE`] bytes walked. An edit drops those at its place and past it, and the lines that
/// start there or past it (R1 MUST 8).
#[derive(Default)]
struct Checkpoints(HashMap<u64, Vec<(u64, u64)>>);

impl Checkpoints {
    /// The last checkpoint of the line at `ls` that `keep(at, col)` takes, else the start.
    fn before(&self, ls: u64, keep: impl Fn(u64, u64) -> bool) -> (u64, u64) {
        let v = self.0.get(&ls).map_or(&[][..], Vec::as_slice);
        let i = v.partition_point(|&(at, col)| keep(at, col));
        i.checked_sub(1).map_or((ls, 0), |i| v[i])
    }

    /// Where the line at `ls` takes its next checkpoint: a stride past its last.
    fn due(&self, ls: u64) -> u64 {
        let last = self.0.get(&ls).and_then(|v| v.last());
        last.map_or(ls, |l| l.0) + STRIDE
    }

    /// A checkpoint at `at`, at least a stride past the line's last.
    fn record(&mut self, ls: u64, at: u64, col: u64) {
        if !self.0.contains_key(&ls) && self.0.len() >= WALKED_LINES {
            self.0.clear();
        }
        self.0.entry(ls).or_default().push((at, col));
    }

    /// The document changed from byte `at` on: what was found before it stays.
    fn edited(&mut self, at: u64) {
        self.0.retain(|&ls, v| {
            v.retain(|&(x, _)| x < at);
            ls < at && !v.is_empty()
        });
    }
}

/// The characters of the line `ls..end` from `from` (a character start, its column), each with
/// where it starts, its column and its width, while `go` says so.
fn walk(
    b: &Doc<Sel>,
    walked: &mut Checkpoints,
    (ls, end): (u64, u64),
    from: (u64, u64),
    mut go: impl FnMut(u64, char, u64, u64) -> bool,
) {
    let enc = b.encoding();
    let (mut at, mut col) = from;
    let mut due = walked.due(ls);
    let mut bytes = Vec::new();
    while at < end {
        let to = (at + CHUNK).min(end);
        bytes.clear();
        b.read(at..to, &mut bytes);
        #[cfg(test)]
        DECODED.with(|n| n.set(n.get() + (to - at)));
        let (mut cells, mut used) = enc.decode(&bytes, to == end);
        if used == 0 {
            (cells, used) = enc.decode(&bytes, true);
        }
        let mut x = at;
        for c in cells {
            let w = width(c.ch, col);
            if !go(x, c.ch, col, w) {
                return;
            }
            col += w;
            x += u64::from(c.len);
            if x >= due {
                walked.record(ls, x, col);
                due = x + STRIDE;
            }
        }
        at += used as u64;
    }
}

/// A character of a row: where it starts, what it is, its column and width, and its index in
/// its line (for its colour).
struct Ch {
    at: u64,
    c: char,
    col: u64,
    w: u64,
    i: usize,
}

// ---------------------------------------------------------------------------------------------
// The view.

/// The editor on screen: the document and its keys, the place on screen, highlighting, and what
/// the status row says.
pub struct EditView {
    ed: Editor,
    keys: Keys,
    clip: Option<Clip>,
    /// The file's name: the frame's title and the prompts'.
    name: String,
    syntax: Option<&'static SyntaxReference>,
    /// The encoding as the status row says it: `UTF-16LE (BOM)`, `UTF-8 (guessed)`.
    encoding: String,
    /// The git letter (§10): ED-git's to give.
    git: Option<char>,
    keep: Keep,
    said: Option<Said>,
    /// The editor has the keyboard: a double frame, the caret, the input method.
    focused: bool,
    strip: Option<Strip>,
    /// The first line and column on screen, and the text's rows and columns last laid out.
    top: u64,
    left: u64,
    rows: u64,
    cols: u64,
    /// The caret moved, the text changed or the window did: the next pass brings the caret
    /// into view.
    follow: bool,
    /// The wheel's part of a row, kept for its next event (R1 S7).
    wheel_rest: f32,
    /// The cells the caret's character takes (Overwrite's block: R1 S9).
    caret_w: u16,
    /// The pass is shown in an ambiguous-wide terminal (10 §10).
    wide: bool,
    /// The document's version: one more at each change.
    version: u64,
    hl: Hl,
    walked: Checkpoints,
    /// The input method's composition (E20).
    preedit: String,
    commands: Vec<Command>,
    column_clicked: bool,
    /// The menus, Find, Go to and the prompts (ED-dialogs).
    dialogs: dialogs::Dialogs,
    #[cfg(test)]
    hook: Option<text::ParseHook>,
}

impl EditView {
    /// The editor on `ed`, the file `name`, its keys from `backend`; `guessed`: the opening's
    /// encoding was guessed (08 §7.2). Its syntax by the name, else by the first line.
    pub fn new(ed: Editor, name: &str, backend: Backend, guessed: bool) -> EditView {
        let b = ed.buffer();
        let enc = b.encoding();
        let head = &b.original().bytes()[..b.original().bytes().len().min(4)];
        let encoding = Pick {
            encoding: enc,
            guessed,
        }
        .label(enc, head);
        let mut view = EditView {
            ed,
            keys: Keys::new(backend),
            clip: None,
            name: name.to_string(),
            syntax: None,
            encoding,
            git: None,
            keep: Keep::Clean,
            said: None,
            focused: true,
            strip: Some(Strip {
                slots: [Some("Text"), Some("Hex"), Some("Raw")],
                on: Some("Text"),
                armed: false,
            }),
            top: 0,
            left: 0,
            rows: 22,
            cols: 49,
            follow: true,
            wheel_rest: 0.0,
            caret_w: 1,
            wide: false,
            version: 0,
            hl: Hl::default(),
            walked: Checkpoints::default(),
            preedit: String::new(),
            commands: Vec::new(),
            column_clicked: false,
            dialogs: dialogs::Dialogs::new(name, backend),
            #[cfg(test)]
            hook: None,
        };
        // The first line for the syntax, read no further than highlighting would parse it.
        let mut head = view.snap();
        (head.len, head.lines) = (head.len.min(LINE_MAX as u64 + 4), 1);
        view.syntax = text::syntax_of(name, &mut BufLines::new(head)).unwrap_or(None);
        view.hl.generation = view.ed.buffer().generation();
        view
    }

    pub fn editor(&self) -> &Editor {
        &self.ed
    }

    /// The editor for what changes it outside its keys — a Save (`Session::save` marks it
    /// saved), a re-read (`Session::reload`, `reread`): what the view keeps of the document
    /// (the walked lines, the page asked of the highlighter) is made again at the next pass.
    pub fn editor_mut(&mut self) -> &mut Editor {
        self.version += 1;
        self.walked = Checkpoints::default();
        self.follow = true;
        self.hl.gen += 1;
        if let Some(worker) = &self.hl.worker {
            worker.latest.store(self.hl.gen, Ordering::Relaxed);
        }
        &mut self.ed
    }

    /// The editor has the keyboard (`true`), or the column has it (ED-layout).
    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// The git letter for the status row (§10, ED-git); `None` until then.
    pub fn set_git(&mut self, letter: Option<char>) {
        self.git = letter;
    }

    /// The journal's state (§9.5): the status row's `unsaved — kept`.
    pub fn set_keep(&mut self, keep: Keep) {
        self.keep = keep;
    }

    /// The views on the title row (08 §8), or none.
    pub fn set_strip(&mut self, strip: Option<Strip>) {
        self.strip = strip;
    }

    /// A line for the status row: F4's E37 line, a Save's result.
    pub fn say(&mut self, said: Said) {
        self.said = Some(said);
    }

    /// What the binding is to do that a click, a menu or a prompt here asked for: Save from the
    /// `^S` prompt's `[Save]`; a menu's Save, Save as…, Save copy, Reload, VS Code / Zed, Exit
    /// and Paste; the close prompt's and the conflict dialog's Save, Save copy and Save As…
    /// (oldest first). What else a prompt chose is [`EditView::take_chosen`]'s.
    pub fn take_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }

    /// Whether `[↕]` was clicked since the last call: the column hidden or shown (§6.3).
    pub fn take_column_click(&mut self) -> bool {
        std::mem::take(&mut self.column_clicked)
    }

    /// One input (`edit::arm`'s [`Press`]): the arming and Shift known kept, an edit made, the
    /// status row's line set, F10's menu bar, Find's or Go to's field opened. The command is
    /// handed back: Save, leaving and the rest are the binding's. While a menu, a field or a
    /// prompt is up ([`EditView::asking`]) it takes every key, so this takes none — but a focus
    /// report or a resize still tells the keys, which forget Shift known then (E31).
    pub fn press(&mut self, press: &Press) -> Command {
        if self.asking() {
            if matches!(press, Press::Focus | Press::Resize) {
                self.keys.press(press);
            }
            return Command::Nothing;
        }
        self.dialogs.pressed(press);
        let command = self.keys.press(press);
        if !matches!(command, Command::Nothing | Command::Typing) {
            self.said = command.line(&self.name).map(Said::Warned);
        }
        self.run(&command);
        self.open(&command);
        command
    }

    /// An editing command made, and what it says on the status row.
    fn run(&mut self, command: &Command) {
        let out = self.edit(|ed, clip| ed.apply(command, clip));
        if out.is_some() {
            self.said = out;
        } else if let Some(question) = self.ed.question() {
            self.said = Some(Said::Asked(question));
        }
    }

    /// `f` on the editor (and the last clip), the view following what it changed: the kept
    /// highlighting and walked lines from the first changed byte on, the caret into view.
    fn edit<R>(&mut self, f: impl FnOnce(&mut Editor, &mut Option<Clip>) -> R) -> R {
        let caret = self.ed.sel();
        let before: Vec<Piece> = self.ed.buffer().pieces().to_vec();
        let lines = self.ed.buffer().lines();
        let out = f(&mut self.ed, &mut self.clip);
        if self.ed.buffer().pieces() != before.as_slice() {
            self.changed(&before, lines);
        }
        self.follow |= self.ed.sel() != caret;
        out
    }

    /// The document changed from `before` (its pieces) with `lines` lines: from the first byte
    /// that may differ on, the kept states and the checkpoints go and the colours below move.
    fn changed(&mut self, before: &[Piece], lines: u64) {
        let b = self.ed.buffer();
        let mut at = 0;
        for (old, new) in before.iter().zip(b.pieces()) {
            if old != new {
                let same_start = match (b.span(old), b.span(new)) {
                    (Span::Original { range: x, .. }, Span::Original { range: y, .. })
                    | (Span::Add(x), Span::Add(y)) => x.start == y.start,
                    _ => false,
                };
                if same_start {
                    at += old.len().min(new.len());
                }
                break;
            }
            at += old.len();
        }
        let line = b.line_of(at.min(b.len()));
        let delta = b.lines() as i64 - lines as i64;
        self.version += 1;
        self.walked.edited(at);
        // An edit that keeps the caret (Del, Ctrl+T) may still move its character (R2 4).
        self.follow = true;
        // A page asked of the document before this edit is never laid on it (R1 MUST 3): the
        // worker drops it, and the view takes no reply asked before now.
        self.hl.gen += 1;
        if let Some(worker) = &self.hl.worker {
            worker.latest.store(self.hl.gen, Ordering::Relaxed);
        }
        self.hl.edited(line, delta);
    }

    /// The snapshot the worker reads.
    fn snap(&mut self) -> Snap {
        let b = self.ed.buffer();
        self.hl.typed.catch_up(b);
        let mut at = 0;
        let spans: Vec<(u64, Span)> = (b.pieces().iter())
            .map(|p| {
                let s = (at, b.span(p));
                at += p.len();
                s
            })
            .collect();
        Snap {
            original: b.original().clone(),
            spans: spans.into(),
            typed: self.hl.typed.chunks.clone().into(),
            len: b.len(),
            lines: b.lines(),
            enc: b.encoding(),
            bom: self.ed.bom(),
        }
    }

    /// Line `n`'s text bytes and where its ending starts (before the CR of a CR LF), and its
    /// raw length (to its LF).
    fn body(&self, n: u64) -> (u64, u64, u64) {
        let b = self.ed.buffer();
        let start = if n == 0 {
            self.ed.bom()
        } else {
            b.line_start(n).unwrap_or(b.len())
        };
        let Some(next) = b.line_start(n + 1) else {
            return (start, b.len(), b.len() - start);
        };
        let enc = b.encoding();
        let (lf, u) = (next - enc.lf().len() as u64, enc.unit() as u64);
        let mut cr = Vec::new();
        if lf >= start + u {
            b.read(lf - u..lf, &mut cr);
        }
        let end = if cr == enc.cr() { lf - u } else { lf };
        (start, end, lf - start)
    }

    /// The characters of the line `ls..end` from the first that reaches column `left` to column
    /// `right`.
    fn visible(&mut self, (ls, end): (u64, u64), left: u64, right: u64) -> Vec<Ch> {
        let from = self.walked.before(ls, |_, col| col <= left);
        // An index from the line's start: only a line that has no checkpoint is highlighted.
        let mut i = 0;
        let mut out = Vec::new();
        // The marks: a cell takes [`MARKS`] of them, and more than a stride of them in the row
        // ends it (R1 MUST 6, counted per row: R2 5) — the bytes of the runs before, and where
        // the run now started.
        let (mut run, mut mark_bytes, mut marks) = (None::<u64>, 0, 0);
        walk(
            self.ed.buffer(),
            &mut self.walked,
            (ls, end),
            from,
            |at, c, col, w| {
                if col >= right {
                    return false;
                }
                if w > 0 {
                    if let Some(start) = run.take() {
                        mark_bytes += at - start;
                    }
                    marks = 0;
                } else {
                    let start = *run.get_or_insert(at);
                    if mark_bytes + (at - start) > STRIDE {
                        return false;
                    }
                    marks += 1;
                }
                let shown = col + w > left || (w == 0 && col >= left);
                if shown && (w > 0 || marks <= MARKS) {
                    out.push(Ch { at, c, col, w, i });
                }
                i += 1;
                true
            },
        );
        out
    }

    /// The caret's line and column, and the cells its character takes: two for a wide one,
    /// else one (a tab's caret is its first cell; at the line's end, one).
    fn caret_place(&mut self) -> (u64, u64, u64) {
        let caret = self.ed.caret();
        let line = self.ed.buffer().line_of(caret);
        let (ls, end, _) = self.body(line);
        let from = self.walked.before(ls, |at, _| at <= caret);
        let (mut col, mut cells) = (from.1, 1);
        walk(
            self.ed.buffer(),
            &mut self.walked,
            (ls, end),
            from,
            |at, c, x, w| {
                col = x + if at < caret { w } else { 0 };
                if at >= caret && c != '\t' {
                    cells = w.max(1);
                }
                at < caret
            },
        );
        (line, col, cells)
    }

    /// The place on line `n` at column `goal`: the first character reaching past it, or the
    /// line's end.
    fn at_column(&mut self, n: u64, goal: u64) -> u64 {
        let (ls, end, _) = self.body(n);
        let from = self.walked.before(ls, |_, col| col <= goal);
        let mut place = end;
        walk(
            self.ed.buffer(),
            &mut self.walked,
            (ls, end),
            from,
            |at, _, col, w| {
                if col + w > goal {
                    place = at;
                    return false;
                }
                true
            },
        );
        place
    }

    /// The line ending a new line takes here (§7.2): the caret line's, else the first line's,
    /// else LF.
    fn ending(&self) -> &'static str {
        let b = self.ed.buffer();
        let crlf = |n: u64| {
            b.line_start(n + 1)?;
            let (start, end, raw) = self.body(n);
            Some(end - start < raw)
        };
        match crlf(b.line_of(self.ed.caret())).or_else(|| crlf(0)) {
            Some(true) => "CRLF",
            _ => "LF",
        }
    }

    // -----------------------------------------------------------------------------------------
    // Highlighting.

    /// Starts highlighting once, hears the worker, and asks for the rows on screen when the
    /// document or the place changed.
    fn highlight(&mut self, ctx: &egui::Context) {
        if self.ed.buffer().generation() != self.hl.generation {
            self.hl = Hl {
                generation: self.ed.buffer().generation(),
                ..Hl::default()
            };
        }
        if !self.hl.tried {
            self.hl.tried = true;
            let hl = self
                .syntax
                .and_then(|s| Highlight::new(s, chrome::PALETTE.text, u64::MAX));
            #[cfg(test)]
            let hl = hl.map(|mut h| {
                if let Some(hook) = &self.hook {
                    h.set_hook(hook.clone());
                }
                h
            });
            if let Some(hl) = hl {
                self.hl.worker = Worker::start(hl, ctx.clone(), &Pool::global());
                if self.hl.worker.is_none() {
                    self.hl.status =
                        Some("highlighting is busy in other viewers; shown plain".into());
                }
            }
        }
        let Some(worker) = &self.hl.worker else {
            return;
        };
        let mut failed = None;
        while let Ok(done) = worker.done.try_recv() {
            match done {
                Done::Page {
                    gen,
                    top,
                    count,
                    runs,
                    stop,
                } if gen == self.hl.gen => {
                    // The reply is the whole page's: its rows it did not carry — past a stop
                    // its read ended at — and anything past the stop are plain (R2 1).
                    let (carried, end) = (top + runs.len() as u64, top + count as u64);
                    (self.hl.lines).retain(|&n, _| n < stop && !(carried..end).contains(&n));
                    for (k, line) in runs.into_iter().enumerate() {
                        let text = line.iter().flat_map(|(_, t)| t.chars()).collect();
                        let colors = (line.iter())
                            .flat_map(|(c, t)| std::iter::repeat_n(*c, t.chars().count()))
                            .collect();
                        self.hl
                            .lines
                            .insert(top + k as u64, Painted { text, colors });
                    }
                    self.hl.answered = true;
                }
                Done::Page { .. } => {}
                Done::Failed(why) => failed = Some(why),
            }
        }
        if let Some(why) = failed {
            self.hl.worker = None;
            // Plain from now on: no colours of before are laid around the next edits (R1 S1).
            self.hl.lines.clear();
            self.hl.status = Some(format!("highlighting stopped ({why}); shown plain"));
            return;
        }
        let want = (self.version, self.top, self.rows as usize);
        if self.hl.asked != Some(want) {
            let snap = self.snap();
            // The page's first line as the buffer's own index finds it (R1 MUST 1).
            let b = self.ed.buffer();
            let start = b.line_start(self.top).unwrap_or(b.len());
            let hl = &mut self.hl;
            let worker = hl.worker.as_ref().expect("started");
            hl.gen += 1;
            worker.latest.store(hl.gen, Ordering::Relaxed);
            let ask = Ask {
                gen: hl.gen,
                snap,
                edit: hl.edit.take(),
                top: (self.top, start),
                count: want.2,
            };
            worker.mail.put(ask);
            hl.asked = Some(want);
            hl.answered = false;
            // The colours kept: the screen's and a screen either side.
            let span = 2 * self.rows.max(1);
            let keep = self.top.saturating_sub(span)..self.top + span + self.rows;
            hl.lines.retain(|n, _| keep.contains(n));
        }
    }

    // -----------------------------------------------------------------------------------------
    // The cells.

    /// The edit window at `frame` of `grid`, into `buf` (§4): the frame, the gutter, the rows,
    /// the block, the scroll bar, the preedit; a click, a drag and the wheel heard first. The
    /// caret's cell, while the editor has the keyboard.
    pub fn window_cells(
        &mut self,
        ui: &mut egui::Ui,
        grid: &Grid,
        buf: &mut Buffer,
        frame: Cells,
    ) -> Option<Position> {
        let ctx = ui.ctx().clone();
        let p = chrome::PALETTE;
        let lines = self.ed.buffer().lines();
        let digits = DIGITS.max(lines.to_string().len()) as u16;
        let inner = Cells::new(
            frame.x + 1,
            frame.y + 1,
            frame.width.saturating_sub(2),
            frame.height.saturating_sub(2),
        );
        let text_x = (inner.x + digits + 2).min(inner.right());
        let text = Cells::new(text_x, inner.y, inner.right() - text_x, inner.height);
        let laid = (u64::from(text.height), u64::from(text.width));
        // A window resized brings the caret back into view (R1 MUST 5); the wheel's rows on a
        // layout that stays are kept.
        self.follow |= laid != (self.rows, self.cols);
        (self.rows, self.cols) = laid;
        self.wide = cells::ambiguous_wide(&ctx);
        self.ed.set_page(self.rows);
        self.mouse(ui, grid, frame, text);
        let (line, col, w) = self.caret_place();
        self.caret_w = w.min(2) as u16;
        if std::mem::take(&mut self.follow) {
            if line < self.top {
                self.top = line;
            } else if line >= self.top + self.rows {
                self.top = line + 1 - self.rows.max(1);
            }
            // The caret's character whole, and a preedit at it (R1 MUST 4).
            let need = w.max(cells::width(&self.preedit) as u64);
            if col < self.left {
                self.left = col;
            } else if col + need > self.left + self.cols {
                self.left = (col + need).saturating_sub(self.cols).min(col);
            }
        }
        self.top = self.top.min(lines.saturating_sub(1));
        self.highlight(&ctx);
        self.frame_cells(buf, frame, line, col);
        if frame.width < 2 || frame.height < 2 {
            return None;
        }
        let dim = Pen::new(p.dim, p.back);
        let block = self.ed.shown();
        for k in 0..text.height {
            let n = self.top + u64::from(k);
            if n >= lines {
                break;
            }
            let y = text.y + k;
            let number = (n + 1).to_string();
            cells::put_right(buf, text_x - 1, y, &number, dim);
            self.row_cells(buf, n, y, text, block.clone());
        }
        let max_top = lines.saturating_sub(self.rows);
        let thumb = (max_top > 0).then(|| self.top.min(max_top) as f32 / max_top as f32);
        cells::scroll_bar(buf, inner.right(), inner.y, inner.height, thumb);
        // While a menu, a field or a prompt has the keys, no caret here and no input method at
        // it: a menu's letter matches the physical key (E10), a field sets its own.
        if !self.focused || self.asking() || text.width == 0 || text.height == 0 {
            return None;
        }
        // The caret's cell — else its place held to the text's edges, where the input method
        // still goes (R1 MUST 9).
        let shown = (self.top..self.top + self.rows).contains(&line)
            && (self.left..self.left + self.cols).contains(&col);
        let at = Position::new(
            text_x + col.saturating_sub(self.left).min(self.cols - 1) as u16,
            text.y + line.saturating_sub(self.top).min(self.rows - 1) as u16,
        );
        let caret = shown.then(|| self.composing(buf, text, at));
        self.ime(&ctx, grid, text, caret.unwrap_or(at));
        if let Some(at) = caret.filter(|_| self.ed.overwrite() && self.preedit.is_empty()) {
            // Overwrite: the character's cells, both of a wide one, their colours swapped — in
            // the text, never over the scroll bar (R2 4), and the cover the same.
            self.caret_w = self.caret_w.min(text.right() - at.x);
            for x in at.x..at.x + self.caret_w {
                let cell = &mut buf[(x, at.y)];
                let (fg, bg) = (cell.fg, cell.bg);
                (cell.fg, cell.bg) = (bg, fg);
            }
        }
        caret
    }

    /// Paints `buf` (`grid`'s) with the caret, as [`cells::paint`] does — in Overwrite with the
    /// foot it draws covered in the block's colour, so the caret is the swapped cells alone (R1
    /// S9).
    pub fn paint(&self, ui: &egui::Ui, grid: &Grid, buf: &Buffer, caret: Option<Position>) {
        let painter = ui.painter_at(grid.rect(grid.all()));
        cells::paint(&painter, grid.origin, buf, caret);
        // (A field's caret, while one has the keys, is the two scan lines.)
        let overwrite = self.ed.overwrite() && self.preedit.is_empty() && !self.asking();
        let Some(at) = caret.filter(|_| overwrite) else {
            return;
        };
        if let ratatui_core::style::Color::Rgb(r, g, b) = buf[(at.x, at.y)].bg {
            let cells = grid.rect(Cells::new(at.x, at.y, self.caret_w.max(1), 1));
            let foot =
                egui::Rect::from_min_max(egui::pos2(cells.left(), cells.bottom() - 2.0), cells.max);
            painter.rect_filled(foot, 0.0, Color32::from_rgb(r, g, b));
        }
    }

    /// The frame with its title and buttons, double while the editor has the keyboard: `[■]`,
    /// the name centred, `1` and `[↕]` in the top border; `*` while unsaved and `line:col` in the
    /// bottom one.
    fn frame_cells(&self, buf: &mut Buffer, frame: Cells, line: u64, col: u64) {
        let p = chrome::PALETTE;
        cells::frame(buf, frame, "", self.focused);
        if frame.width < 16 || frame.height < 2 {
            return;
        }
        let line_pen = Pen::new(
            if self.focused {
                p.frame_active.color
            } else {
                p.frame_idle.color
            },
            p.back,
        );
        let green = Pen::new(cga::LIGHT_GREEN, p.back);
        let (l, r, t, bottom) = (frame.x, frame.right() - 1, frame.y, frame.bottom() - 1);
        cells::put(buf, l + 2, t, "[■]", 3, green);
        cells::put(buf, r - 6, t, "1", 1, line_pen);
        cells::put(buf, r - 4, t, "[↕]", 3, green);
        let room = usize::from(frame.width).saturating_sub(16);
        let name = cells::fit_left(&self.name, room);
        if !name.is_empty() {
            let w = cells::width(&name) as u16;
            let x = l + 6 + (frame.width - 14 - w) / 2;
            cells::fill(buf, Cells::new(x - 1, t, w + 2, 1), line_pen);
            cells::put(buf, x, t, &name, w.into(), line_pen);
        }
        let star = if self.ed.buffer().is_saved() { "" } else { "*" };
        let place = format!("{star} {}:{} ", line + 1, col + 1);
        let room = usize::from(frame.width).saturating_sub(4);
        let x = l + if star.is_empty() { 3 } else { 2 };
        cells::put(buf, x, bottom, &place, room, line_pen);
    }

    /// Line `n` in row `y` of the text area `text`: its characters from column `left`, in their
    /// colours, the block's in Turbo's blue on cyan. A glyph that does not fit at the right edge
    /// ends the row; the part of one cut by the left edge is blank; a mark joins its glyph.
    fn row_cells(
        &mut self,
        buf: &mut Buffer,
        n: u64,
        y: u16,
        text: Cells,
        block: Option<Range<u64>>,
    ) {
        let p = chrome::PALETTE;
        let (left, right) = (self.left, self.left + u64::from(text.width));
        let (ls, end, raw) = self.body(n);
        let chars = self.visible((ls, end), left, right);
        // A line highlighting covers: its characters from its start, as the worker read them.
        let colors = (raw <= LINE_MAX as u64)
            .then(|| {
                let mut bytes = Vec::new();
                self.ed.buffer().read(ls..end, &mut bytes);
                let enc = self.ed.buffer().encoding();
                let all: Vec<char> = line_text(enc, ls, bytes, false, false).chars().collect();
                self.hl.colors(n, &all)
            })
            .flatten();
        // Each cell's symbol made once: its glyph and the marks after it (R1 MUST 6).
        let mut cell: Option<(u16, String, Pen)> = None;
        let put = |buf: &mut Buffer, cell: &mut Option<(u16, String, Pen)>| {
            if let Some((x, shown, pen)) = cell.take() {
                cells::put(buf, x, y, &shown, usize::from(text.right() - x), pen);
            }
        };
        for ch in chars {
            if ch.w == 0 {
                if let Some((_, shown, _)) = cell.as_mut() {
                    shown.push(ch.c);
                }
                continue;
            }
            put(buf, &mut cell);
            if ch.col + ch.w > right {
                break;
            }
            let plain = if ('\u{80}'..='\u{9F}').contains(&ch.c) {
                p.dim
            } else {
                p.text
            };
            let fg = (colors.as_ref()).map_or(plain, |c| c.get(ch.i).copied().unwrap_or(plain));
            let pen = match &block {
                Some(r) if r.contains(&ch.at) => Pen::new(cga::BLUE, cga::CYAN),
                _ => Pen::new(fg, p.back),
            };
            let from = ch.col.max(left);
            let shown = if ch.c == '\t' || ch.col < left {
                " ".repeat((ch.col + ch.w - from) as usize)
            } else if self.wide && own(ch.c) {
                // sopen's own glyph as the viewer sends it there (10 §10, T38; R1 S6).
                text::own_glyph(face(ch.c), true).to_string()
            } else {
                face(ch.c).to_string()
            };
            cell = Some((text.x + (from - left) as u16, shown, pen));
        }
        put(buf, &mut cell);
    }

    /// The input method's preedit over the cells at the caret `at` (E20): where the caret then
    /// stands.
    fn composing(&mut self, buf: &mut Buffer, text: Cells, at: Position) -> Position {
        let p = chrome::PALETTE;
        let mut caret = at;
        if !self.preedit.is_empty() {
            let room = usize::from(text.right().saturating_sub(at.x));
            let pen = Pen::new(p.field_text, cga::BLACK);
            let w = cells::put(buf, at.x, at.y, &self.preedit, room, pen) as u16;
            caret.x = (at.x + w).min(text.right().saturating_sub(1));
        }
        caret
    }

    /// The IME rectangle at `at` of the text `text`, while the editor has the keyboard and no
    /// arming waits for a letter (E10: the input method on only then).
    fn ime(&self, ctx: &egui::Context, grid: &Grid, text: Cells, at: Position) {
        if self.keys.armed() != Armed::None {
            return;
        }
        let cursor = grid.rect(Cells::new(at.x, at.y, 1, 1));
        ctx.output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: grid.rect(text),
                cursor_rect: cursor,
                should_interrupt_composition: false,
            })
        });
    }

    /// A click or a drag in the text (the caret, a block), the wheel (scrolling), the frame's
    /// `[■]` (F3) and `[↕]` (the column); the input method's preedit.
    fn mouse(&mut self, ui: &mut egui::Ui, grid: &Grid, frame: Cells, text: Cells) {
        let ctx = ui.ctx().clone();
        let id = ui.scope_id().with("sopen-edit");
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        // A new composition is brought into view with the caret (R1 MUST 4).
                        self.follow |= self.preedit != *text;
                        self.preedit.clone_from(text);
                    }
                    egui::Event::Ime(_) => self.preedit.clear(),
                    _ => {}
                }
            }
        });
        if frame.width >= 16 {
            let button = |x: u16| grid.rect(Cells::new(x, frame.y, 3, 1));
            let close = ui.interact(button(frame.x + 2), id.with("close"), egui::Sense::CLICK);
            if close.clicked() {
                chrome::press_fkey(&ctx, 3);
            }
            let column = ui.interact(
                button(frame.right() - 5),
                id.with("zoom"),
                egui::Sense::CLICK,
            );
            self.column_clicked |= column.clicked();
        }
        if text.width == 0 || text.height == 0 {
            return;
        }
        let area = grid.rect(text);
        let response = ui.interact(area, id.with("text"), egui::Sense::click_and_drag());
        // The wheel scrolls the rows (the caret stays where it is), as the list's does.
        let wheel: f32 = if ui.rect_contains_pointer(grid.rect(frame)) {
            ctx.input(|i| {
                (i.events.iter())
                    .map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, .. } => match unit {
                            egui::MouseWheelUnit::Point => -delta.y / chrome::CELL.y,
                            _ => -delta.y * 3.0,
                        },
                        _ => 0.0,
                    })
                    .sum()
            })
        } else {
            0.0
        };
        if wheel != 0.0 {
            // What is less than a row is kept for the next event, as the viewer's is (R1 S7).
            let rows = wheel + self.wheel_rest;
            let whole = rows.trunc();
            self.wheel_rest = rows - whole;
            let last = self.ed.buffer().lines().saturating_sub(1);
            self.top = self.top.saturating_add_signed(whole as i64).min(last);
        }
        let pressed = response.drag_started() || response.clicked();
        if !(pressed || response.dragged()) {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        let Some(cell) = grid.at(pos) else {
            return;
        };
        let n = self.top + u64::from(cell.y.saturating_sub(text.y));
        let n = n.min(self.ed.buffer().lines() - 1);
        let goal = self.left + u64::from(cell.x.saturating_sub(text.x));
        let at = self.at_column(n, goal);
        let extend = (response.dragged() && !pressed) || ctx.input(|i| i.modifiers.shift);
        self.ed.place(at, extend);
        self.follow = true;
    }

    /// The status row in row `y` of `buf`, `cols` wide (§4): the fields — `Insert Indent`, the
    /// syntax, the encoding, the line ending, the git letter, `unsaved — kept` — then the arming
    /// or the last line; while `^S` waits, its prompt alone. The prompt's `[Save]`, where it is.
    /// In an ambiguous-wide terminal (`wide`) sopen's `·` and `…` are §10's `.` and `~`.
    pub fn status_cells(&self, buf: &mut Buffer, y: u16, cols: u16, wide: bool) -> Option<Cells> {
        let p = chrome::PALETTE;
        let pen = Pen::new(p.text, p.back);
        cells::fill(buf, Cells::new(0, y, cols, 1), pen);
        let room = usize::from(cols.saturating_sub(2));
        let fit = |text: &str, room: usize| {
            let shown = cells::fit_start(text, room);
            match wide {
                true => shown.replace('·', ".").replace('…', "~"),
                false => shown,
            }
        };
        if self.keys.armed() == Armed::Save {
            // The name gives way first, to its last [`NAME_MIN`] cells, so `[Save]` and what
            // every other key does stay; narrower than the words themselves (84 cells) their end
            // gives way after it (R1 S10).
            let fixed = cells::width(&save_over(""));
            let name_room = room.saturating_sub(fixed).max(NAME_MIN);
            let prompt = save_over(&cells::fit_left(&self.name, name_room));
            let shown = fit(&prompt, room);
            cells::put(buf, 1, y, &shown, room, Pen::new(p.warn, p.back));
            let at = shown.find("[Save]")?;
            let x = 1 + cells::width(&shown[..at]) as u16;
            let save = Cells::new(x, y, 6, 1);
            cells::put(buf, x, y, "[Save]", 6, Pen::new(p.bar_text, p.bar));
            return Some(save);
        }
        let mut fields = Vec::new();
        if let Some(mark) = self.keys.mark(&self.name) {
            fields.push(mark);
        }
        let modes: Vec<&str> = [
            (!self.ed.overwrite(), "Insert"),
            (self.ed.indent(), "Indent"),
        ]
        .into_iter()
        .filter_map(|(on, word)| on.then_some(word))
        .collect();
        if !modes.is_empty() {
            fields.push(modes.join(" "));
        }
        fields.push(
            self.syntax
                .map_or("Plain Text", |s| s.name.as_str())
                .to_string(),
        );
        fields.push(self.encoding.clone());
        fields.push(self.ending().to_string());
        if let Some(letter) = self.git {
            fields.push(letter.to_string());
        }
        let kept = match (&self.keep, self.ed.buffer().is_saved()) {
            (_, true) => None,
            (Keep::Clean, false) => Some("unsaved".to_string()),
            (Keep::Kept, false) => Some("unsaved — kept".to_string()),
            (keep, false) => Some(keep.line("")),
        };
        fields.extend(kept);
        let line = fields.join(" │ ");
        let x = 1 + cells::put(buf, 1, y, &fit(&line, room), room, pen) as u16;
        let (said, error) = match &self.said {
            Some(Said::Refused(t)) => (Some(t.clone()), true),
            Some(Said::Asked(t) | Said::Warned(t)) => (Some(t.clone()), false),
            None => (self.hl.status.clone(), false),
        };
        if let Some(said) = said.filter(|_| x + 4 < cols) {
            let room = usize::from(cols - 1 - x - 3);
            cells::put(buf, x, y, " │ ", 3, pen);
            let color = if error { p.error } else { p.text };
            cells::put(
                buf,
                x + 3,
                y,
                &fit(&said, room),
                room,
                Pen::new(color, p.back),
            );
        }
        None
    }

    /// The title row in row `y` of `buf`, `cols` wide (§4): ` xtools ` in its red block, the
    /// menus with each name's letter red, the views as §4 writes them (`1 Text·2 Hex·3 Raw`, the
    /// one on screen in the bar's colours, 08 §8) and the clock. In an ambiguous-wide terminal
    /// (`wide`) the views' `·` is §10's `.`, as the browser's title row has it.
    pub fn title_cells(&self, buf: &mut Buffer, y: u16, cols: u16, wide: bool) {
        let p = chrome::PALETTE;
        let pen = Pen::new(p.title_text, p.title_back);
        cells::fill(buf, Cells::new(0, y, cols, 1), pen);
        let all = usize::from(cols);
        let block = format!(" {} ", chrome::PROGRAM);
        let block_pen = Pen::new(p.program_text, p.program_back);
        cells::put(buf, 0, y, &block, all, block_pen);
        let clock = Self::clock();
        let end = all.saturating_sub(cells::width(&clock) + 1);
        cells::put(buf, end as u16, y, &clock, all, pen);
        if let Some((mut at, entries)) = self.strip_place(cols, wide) {
            let dot = if wide { "." } else { "·" };
            let strip = self.strip.as_ref().expect("a strip placed");
            for (i, entry) in entries.iter().enumerate() {
                if i > 0 {
                    at += cells::put(buf, at, y, dot, 1, pen) as u16;
                }
                let on = (strip.on).is_some_and(|on| entry.ends_with(&format!(" {on}")));
                let entry_pen = if on { Pen::new(p.bar_text, p.bar) } else { pen };
                at += cells::put(buf, at, y, entry, all, entry_pen) as u16;
            }
        }
        // The menus, F10's bar on the one it is on (ED-dialogs).
        let red = Pen::new(cga::RED, p.title_back);
        let bar = Pen::new(p.bar_text, p.bar);
        for (i, x) in self.menu_places(cols, wide).into_iter().enumerate() {
            let name = MENUS[i];
            let (first, rest) = name.split_at(1);
            let (first_pen, rest_pen) = match self.dialogs.bar_on() == Some(i) {
                true => (bar, bar),
                false => (red, pen),
            };
            cells::put(buf, x, y, first, 1, first_pen);
            cells::put(buf, x + 1, y, rest, all, rest_pen);
        }
    }

    /// The title row's clock: `│12:34`, or nothing where the local time is not known.
    fn clock() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        chrome::local_tm(now).map_or(String::new(), |tm| {
            format!("│{}", chrome::clock_text(tm.tm_hour, tm.tm_min))
        })
    }

    /// Where the views stand on a title row `cols` wide, and their texts: before the clock,
    /// where they fit after the program's block.
    fn strip_place(&self, cols: u16, wide: bool) -> Option<(u16, Vec<String>)> {
        let strip = self.strip.as_ref()?;
        let all = usize::from(cols);
        let x = cells::width(&format!(" {} ", chrome::PROGRAM));
        let end = all.saturating_sub(cells::width(&Self::clock()) + 1);
        let dot = if wide { "." } else { "·" };
        let entries: Vec<String> = (strip.entries().into_iter())
            .map(|(_, t)| t.replace('·', dot))
            .collect();
        let w = cells::width(&entries.join(dot)) + 1;
        (end > x + w).then(|| ((end - w) as u16, entries))
    }

    /// Where each of the title row's menus that fits starts, on a row `cols` wide: after the
    /// program's block, a blank either side of each name, before the views and the clock.
    fn menu_places(&self, cols: u16, wide: bool) -> Vec<u16> {
        let all = usize::from(cols);
        let mut end = all.saturating_sub(cells::width(&Self::clock()) + 1);
        if let Some((at, _)) = self.strip_place(cols, wide) {
            end = usize::from(at);
        }
        let mut x = cells::width(&format!(" {} ", chrome::PROGRAM));
        let mut out = Vec::new();
        for name in MENUS {
            x += 1;
            if x + cells::width(name) + 1 > end {
                break;
            }
            out.push(x as u16);
            x += cells::width(name) + 1;
        }
        out
    }
}

impl Window for EditView {
    /// The editor as a screen of its own (its tests; ED-layout puts its parts in the browser's
    /// rows and beside the column): the title row, the edit window, the status row and the F-key
    /// row, a grid of cells painted once.
    fn ui(&mut self, ui: &mut egui::Ui) {
        chrome::key_panel_cells(ui, "sopen-edit-keys", KEYS);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let grid = Grid::over(ui.max_rect());
                let mut buf = Buffer::empty(grid.all());
                let p = chrome::PALETTE;
                cells::fill(&mut buf, grid.all(), Pen::new(p.text, p.back));
                if grid.rows < 3 {
                    return;
                }
                let wide = cells::ambiguous_wide(ui.ctx());
                self.title_cells(&mut buf, 0, grid.cols, wide);
                let frame = Cells::new(0, 1, grid.cols, grid.rows - 2);
                let caret = self.window_cells(ui, &grid, &mut buf, frame);
                let save = self.status_cells(&mut buf, grid.rows - 1, grid.cols, wide);
                let caret = self.dialogs_ui(ui, &grid, &mut buf, (0, grid.rows - 1), caret);
                if let Some(save) = save.filter(|_| !self.asking()) {
                    let id = ui.scope_id().with("sopen-edit-save");
                    if ui
                        .interact(grid.rect(save), id, egui::Sense::CLICK)
                        .clicked()
                    {
                        let command = self.press(&Press::PromptSave);
                        self.commands.push(command);
                    }
                }
                self.paint(ui, &grid, &buf, caret);
            });
        // The clock turns over at the next minute.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(60));
    }

    /// Drawn as the document and the caret are, and highlighting has answered for the rows on
    /// screen, or there is none.
    fn ready(&self) -> bool {
        let drawn = self.hl.tried && !self.follow;
        let asked = self
            .hl
            .asked
            .is_some_and(|(version, ..)| version == self.version);
        drawn && (self.hl.worker.is_none() || (self.hl.answered && asked))
    }

    fn keys(&self) -> &'static [FKey] {
        KEYS
    }

    fn paints_cells(&self) -> bool {
        true
    }
}

/// EH-3's probe (`tests/kept_states_heap.rs`): the editor's kept highlighting states over
/// `bytes`, a file named `name` read as UTF-8, made on this thread as the worker makes them
/// ([`KeptStates::fill`]) and held until the probe goes, so a counting allocator sees their heap.
#[doc(hidden)]
pub struct KeptStates {
    hl: Highlight,
    src: BufLines,
}

impl KeptStates {
    pub fn new(bytes: Vec<u8>, name: &str) -> KeptStates {
        let enc = Encoding::Utf8;
        let ed = Editor::open(Original::new(bytes, enc), Default::default());
        let mut view = EditView::new(ed, name, Backend::Window, false);
        let syntax = view.syntax.expect("a syntax for the name");
        let src = BufLines::new(view.snap());
        let hl = Highlight::new(syntax, chrome::PALETTE.text, u64::MAX).expect("a highlighter");
        KeptStates { hl, src }
    }

    /// Every kept state up to the last line's.
    pub fn fill(&mut self) {
        let last = self.src.snap.lines.saturating_sub(1);
        let page = self.hl.page(&mut self.src, last, 1, &|| false);
        page.expect("highlighted").expect("not dropped");
    }

    /// The states kept.
    pub fn count(&self) -> usize {
        self.hl.kept()
    }

    /// The lines offered.
    pub fn lines(&self) -> u64 {
        self.src.snap.lines
    }
}

#[cfg(test)]
mod tests;
