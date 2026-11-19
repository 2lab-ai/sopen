//! The text viewer (spec §3a): a text file read-only, with syntax highlighting (syntect —
//! Sublime syntaxes on the pure-Rust regex engine), line numbers, and paging: the file is read a
//! page at a time through a sparse line index, never whole, so a file of any size opens alike.
//!
//! The index is one pass over the file when it opens (on the opener's worker): a mark every
//! [`BLOCK_LINES`] lines, and at the first line start [`SPAN`] bytes past the last mark. A line
//! that does not start at a mark starts less than a span past one, so finding any line reads at
//! most a span; a longer line is followed by a mark, so its end is known without reading it.
//! Only [`LINE_MAX`] bytes of a line are shown.
//!
//! Highlighting carries state from the top of the file: the state at each mark is computed
//! once, in order, and a page is highlighted from the mark before it. Each frame aims to spend
//! no more than [`HIGHLIGHT_BUDGET`] on it — a cooperative target, checked after every line (a
//! line in progress finishes) — and the next frame goes on from there; until then the page
//! shows plain. A line is parsed only whole: highlighting stops at the first
//! line longer than [`LINE_MAX`] (only its start is read, and a comment or string closed past the
//! cut would colour everything after it), and the rest of the file is plain. Files over
//! [`HIGHLIGHT_MAX`] are always shown plain.
//!
//! The file is read live; the index is of the file as opened. A file that changes since then
//! (size, modification time, inode) is noticed before each new page: the window says so and
//! reads no more. Every offset taken from the index is checked, never trusted.

use super::{chrome, Embedded, Outcome, Status, Window};
use anyhow::{Context as _, Result};
use eframe::egui::{self, text::LayoutJob, Color32, Key, Modifiers, Pos2, Rect};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter, ThemeSet};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// A mark in the line index every this many lines…
const BLOCK_LINES: u64 = 256;
/// …or at the first line start this far past the last mark, whichever comes first.
const SPAN: u64 = 64 * 1024;
/// Bytes of a line that are shown; the rest is cut (the bar says so).
pub const LINE_MAX: usize = 4096;
/// Files above this are shown without highlighting (its state runs from the top of the file).
pub const HIGHLIGHT_MAX: u64 = 16 << 20;
/// Highlighting work per frame while it catches up with the page on screen: a cooperative
/// target, checked between lines (one line's parse, at most [`LINE_MAX`] bytes, always finishes).
const HIGHLIGHT_BUDGET: Duration = Duration::from_millis(6);
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
    opened: (u64, Option<std::time::SystemTime>, u64),
    /// Bytes read for lines (not the index pass) — what paging costs.
    page_reads: u64,
}

/// What [`TextDoc::changed`] compares.
fn identity(meta: &std::fs::Metadata) -> (u64, Option<std::time::SystemTime>, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.len(), meta.modified().ok(), meta.ino())
}

impl TextDoc {
    /// Opens `path` and indexes its lines (one pass over the file, holding no text).
    pub fn open(path: &Path) -> Result<Self> {
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
        }
        if line_len > LINE_MAX as u64 && first_long.is_none() {
            first_long = Some(line);
        }
        let ends_newline = last_byte == Some(b'\n');
        if last_byte.is_some() && !ends_newline {
            line += 1;
        }
        Ok(Self {
            file,
            size,
            marks,
            lines: line,
            ends_newline,
            first_long,
            opened,
            page_reads: 0,
        })
    }

    /// The file is not the one indexed any more: another size, modification time or inode
    /// (or it cannot be stat'ed).
    pub fn changed(&self) -> bool {
        self.file.metadata().map(|m| identity(&m)).ok() != Some(self.opened)
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
                // More lines start before the next mark, so this one ends before it: never read
                // past the mark (a file changed meanwhile could make it a line of any length).
                let mut text = Vec::new();
                let mut cut = false;
                loop {
                    let room_to_mark = next.offset.saturating_sub(pos);
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

/// What the window says once the file changed: it reads no more.
const CHANGED: &str =
    "the file changed since it was opened — Esc, then open it again to see it now";

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

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(ThemeSet::load_defaults)
}

/// Loads the bundled syntaxes and themes (tens of milliseconds, once): the opener calls it on its
/// worker, so the window does not wait for it.
pub fn warm() {
    syntaxes();
    themes();
}

/// The syntax for a file named `name` whose first line is `first`: by extension, by the whole
/// name (`Makefile`), then by the first line (`#!/bin/sh`). `None` for plain text.
fn syntax_for(name: &str, first: &str) -> Option<&'static SyntaxReference> {
    let set = syntaxes();
    let found = crate::kind::extension(name)
        .and_then(|ext| set.find_syntax_by_extension(&ext))
        .or_else(|| set.find_syntax_by_extension(name))
        .or_else(|| set.find_syntax_by_first_line(first))?;
    (found.name != "Plain Text").then_some(found)
}

/// A highlighted line: runs of text and their colour.
type Runs = Vec<(Color32, String)>;

/// Parser and highlighter state after some line.
type State = (ParseState, HighlightState);

/// What one frame may spend on highlighting — until a moment, and (tests) a number of lines —
/// and the lines parsed so far this frame.
#[derive(Debug, Clone, Copy)]
struct Budget {
    until: Instant,
    lines: usize,
    done: usize,
}

impl Budget {
    fn spent(&self) -> bool {
        self.done >= self.lines || Instant::now() >= self.until
    }
}

/// Where highlighting got to: the state before line `next`.
struct Cursor {
    next: u64,
    state: State,
}

/// A page being highlighted: the cursor, and the page's lines done so far.
struct PageJob {
    top: u64,
    at: Cursor,
    runs: Vec<Runs>,
}

/// Highlighting state at each mark, computed in order from the top of the file, a budget at a
/// time; and the page on screen, the same way.
struct Highlight {
    highlighter: Highlighter<'static>,
    /// The state at mark `i`'s line, for every `i < states.len()`.
    states: Vec<State>,
    /// On the way to the next mark.
    frontier: Option<Cursor>,
    job: Option<PageJob>,
    /// Lines from here on are plain (the first line cut for display, see the module doc).
    stop: u64,
    plain: Color32,
    /// Lines parsed so far (tests: the work a frame does).
    parsed: u64,
    /// A read found the file changed under the index ([`Stale`]): nothing more is done.
    stale: bool,
}

/// Lines read at a time while highlighting.
const BATCH: usize = 64;

impl Highlight {
    fn new(
        syntax: &'static SyntaxReference,
        dark: bool,
        plain: Color32,
        stop: u64,
    ) -> Option<Self> {
        let theme = themes().themes.get(chrome::syntax_theme(dark))?;
        let highlighter = Highlighter::new(theme);
        let start = (
            ParseState::new(syntax),
            HighlightState::new(&highlighter, ScopeStack::new()),
        );
        Some(Self {
            highlighter,
            states: vec![start],
            frontier: None,
            job: None,
            stop,
            plain,
            parsed: 0,
            stale: false,
        })
    }

    fn run(&mut self, state: &mut State, line: &str) -> Runs {
        self.parsed += 1;
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

    /// Parses lines from `at` up to line `to` while `budget` lasts (checked after every line),
    /// keeping each line's runs in `keep` when given; whether it got there.
    fn walk(
        &mut self,
        doc: &mut TextDoc,
        at: &mut Cursor,
        to: u64,
        budget: &mut Budget,
        mut keep: Option<&mut Vec<Runs>>,
    ) -> bool {
        while at.next < to {
            if budget.spent() {
                return false;
            }
            let want = (to - at.next).min(BATCH as u64) as usize;
            let lines = match doc.read_lines(at.next, want) {
                Ok(lines) => lines,
                Err(e) => {
                    self.stale |= is_stale(&e);
                    return false;
                }
            };
            if lines.is_empty() {
                return false;
            }
            for line in &lines {
                if budget.spent() {
                    return false;
                }
                let runs = self.run(&mut at.state, &line.text);
                if let Some(keep) = keep.as_deref_mut() {
                    keep.push(runs);
                }
                at.next += 1;
                budget.done += 1;
            }
        }
        true
    }

    /// Computes the states up to mark `upto` while `budget` lasts; whether they are there.
    fn advance(&mut self, doc: &mut TextDoc, upto: usize, budget: &mut Budget) -> bool {
        while self.states.len() <= upto {
            let i = self.states.len() - 1;
            let to = doc.next_mark(i).line;
            let mut at = self.frontier.take().unwrap_or_else(|| Cursor {
                next: doc.marks[i].line,
                state: self.states[i].clone(),
            });
            if !self.walk(doc, &mut at, to, budget, None) {
                self.frontier = Some(at);
                return false;
            }
            self.states.push(at.state);
        }
        true
    }

    /// The page at `top` (its `lines`), highlighted up to the stop line (plain after it) — or
    /// `None` while it is not done; the next call goes on from where this one stopped.
    fn page(
        &mut self,
        doc: &mut TextDoc,
        top: u64,
        lines: &[Line],
        budget: &mut Budget,
    ) -> Option<Vec<Runs>> {
        let end = (top + lines.len() as u64).min(self.stop);
        let mut runs = Vec::new();
        if top < end {
            if self.job.as_ref().is_none_or(|j| j.top != top) {
                let k = doc.mark_before(top);
                if !self.advance(doc, k, budget) {
                    return None;
                }
                self.job = Some(PageJob {
                    top,
                    at: Cursor {
                        next: doc.marks[k].line,
                        state: self.states[k].clone(),
                    },
                    runs: Vec::new(),
                });
            }
            let mut job = self.job.take().expect("set above");
            // Up to the page (from the mark before it), then the page's own lines.
            let reached = self.walk(doc, &mut job.at, top, budget, None)
                && self.walk(doc, &mut job.at, end, budget, Some(&mut job.runs));
            if !reached {
                self.job = Some(job);
                return None;
            }
            runs = job.runs;
        }
        runs.truncate(lines.len());
        let plain = self.plain;
        runs.extend(
            lines[runs.len()..]
                .iter()
                .map(|l| vec![(plain, l.text.clone())]),
        );
        Some(runs)
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

/// The page on screen: its first line, how many, the lines and how they are drawn.
struct Page {
    top: u64,
    count: usize,
    lines: Vec<Line>,
    runs: Vec<Runs>,
    highlighted: bool,
}

struct TextWindow {
    /// `host:/abs`.
    title: String,
    doc: TextDoc,
    syntax: Option<&'static SyntaxReference>,
    /// Built on the first pass (it needs the theme: dark or light).
    hl: Option<Highlight>,
    hl_tried: bool,
    /// The first line on screen, and how many fit (the last pass's).
    top: u64,
    visible: u64,
    /// Columns scrolled to the right.
    left: usize,
    wheel_rest: f32,
    page: Option<Page>,
    /// Lines from open that must be read whole (where earlier copies' unsaved edits went).
    notices: Vec<String>,
    status: Status,
    painted: bool,
    /// The file changed since it was opened: nothing more is read.
    stale: bool,
    /// Lines a frame may parse (tests); the time budget holds in any case.
    budget_lines: usize,
}

impl TextWindow {
    /// Lines highlighted so far (tests).
    #[cfg(test)]
    fn parsed(&self) -> u64 {
        self.hl.as_ref().map_or(0, |h| h.parsed)
    }

    fn new(mut doc: TextDoc, title: String, name: &str, notices: Vec<String>) -> Self {
        // Changed since it was indexed (between the opener's worker and now): nothing is read.
        let stale = doc.changed();
        let first = if stale {
            String::new()
        } else {
            doc.read_lines(0, 1)
                .ok()
                .and_then(|l| l.into_iter().next())
                .map(|l| l.text)
                .unwrap_or_default()
        };
        let syntax = syntax_for(name, &first);
        let status = match (syntax, doc.first_long) {
            _ if stale => Status::error(CHANGED),
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
            doc,
            syntax,
            hl: None,
            hl_tried: false,
            top: 0,
            visible: 1,
            left: 0,
            wheel_rest: 0.0,
            page: None,
            notices,
            status,
            painted: false,
            stale,
            budget_lines: usize::MAX,
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
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
            self.top = self.doc.line_count().saturating_sub(self.visible);
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
        let same = self
            .page
            .as_ref()
            .is_some_and(|p| p.top == top && p.count == count);
        if !self.stale && self.doc.changed() {
            self.stale = true;
            self.status = Status::error(CHANGED);
        }
        if self.stale {
            // The index no longer describes the file: the page on screen stays (an empty one if
            // none was read yet), nothing more is read.
            let page = self.page.get_or_insert_with(|| Page {
                top,
                count,
                lines: Vec::new(),
                runs: Vec::new(),
                highlighted: true,
            });
            page.highlighted = true;
            return;
        }
        if !same {
            let lines = match self.doc.read_lines(top, count) {
                Ok(lines) => lines,
                Err(e) if is_stale(&e) => {
                    // Changed after the check, during the read: said the same way.
                    self.stale = true;
                    self.status = Status::error(CHANGED);
                    return self.ensure_page(ctx, plain);
                }
                Err(e) => {
                    self.status = Status::error(format!("{e:#}"));
                    Vec::new()
                }
            };
            let runs = lines
                .iter()
                .map(|l| expand_tabs(vec![(plain, l.text.clone())]))
                .collect();
            self.page = Some(Page {
                top,
                count,
                lines,
                runs,
                highlighted: false,
            });
        }
        let (Some(hl), Some(page)) = (self.hl.as_mut(), self.page.as_mut()) else {
            return;
        };
        if page.highlighted {
            return;
        }
        let mut budget = Budget {
            until: Instant::now() + HIGHLIGHT_BUDGET,
            lines: self.budget_lines,
            done: 0,
        };
        match hl.page(&mut self.doc, top, &page.lines, &mut budget) {
            Some(runs) => {
                page.runs = runs.into_iter().map(expand_tabs).collect();
                page.highlighted = true;
            }
            None if hl.stale => {
                self.stale = true;
                self.status = Status::error(CHANGED);
                page.highlighted = true;
            }
            None => ctx.request_repaint(),
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
        let rect = ui.max_rect();
        let p = chrome::Palette::of(ui.visuals());
        let font = chrome::mono(ui);
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        let row_h = (text_h + 2.0).ceil();
        let lines = self.doc.line_count();
        self.visible = (((rect.height() - 4.0) / row_h).floor() as u64).max(1);
        if ui.rect_contains_pointer(rect) {
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
            if self.doc.size() <= HIGHLIGHT_MAX {
                let stop = self.doc.first_long.unwrap_or(u64::MAX);
                self.hl = self
                    .syntax
                    .and_then(|s| Highlight::new(s, ui.visuals().dark_mode, p.text, stop));
            }
        }
        self.ensure_page(&ctx, p.text);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, p.back);
        let digits = lines.max(1).to_string().len();
        let gutter = cw * (digits as f32 + 1.0) + 6.0;
        let text_x = rect.left() + gutter + cw;
        let text_painter =
            ui.painter_at(Rect::from_min_max(Pos2::new(text_x, rect.top()), rect.max));
        if let Some(page) = &self.page {
            for (k, runs) in page.runs.iter().enumerate() {
                let y = rect.top() + 2.0 + k as f32 * row_h;
                painter.text(
                    Pos2::new(rect.left() + gutter, y),
                    egui::Align2::RIGHT_TOP,
                    (page.top + k as u64 + 1).to_string(),
                    font.clone(),
                    p.dim,
                );
                let mut job = LayoutJob::default();
                let mut skip = self.left;
                for (color, text) in runs {
                    let n = text.chars().count();
                    let part: String = if skip >= n {
                        skip -= n;
                        continue;
                    } else {
                        let rest = text.chars().skip(skip).collect();
                        skip = 0;
                        rest
                    };
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
        egui::Panel::top("sopen-text-bar").show(ui, |ui| {
            ui.add_space(3.0);
            self.bar(ui);
            ui.add_space(1.0);
        });
        egui::Panel::bottom("sopen-text-status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
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
            && self
                .page
                .as_ref()
                .is_some_and(|p| p.highlighted || self.hl.is_none())
    }
}

/// The text viewer inside another window (the file browser). `title` is `host:/abs`, `name` the
/// file's name (it picks the syntax); `notices` are shown whole in its rows.
pub fn embed(doc: TextDoc, title: String, name: &str, notices: Vec<String>) -> Embedded {
    Embedded {
        window: Box::new(TextWindow::new(doc, title, name, notices)),
        settle: Box::new(|_| Some(Outcome::default())),
    }
}

#[cfg(test)]
mod tests;
