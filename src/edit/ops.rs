//! The editor's commands over the buffer (`.prd/12-editor.md` §5, §7.2, §7.3; ED-ops), in the
//! file's own bytes and encoding. Screen-free (12 §15): the screen, the dialogs and the keys'
//! binding are ED-screen's, ED-dialogs' and ED-layout's.
//!
//! - **The caret** is a byte offset where a decoded character starts, or a line's end: never
//!   inside an encoding unit or a CP949 pair, never between a CR and its LF (E35), never before
//!   the BOM (§7.2). ← → step one decoded [`Cell`] (§7.2 says a Cell, not a grapheme cluster), a
//!   line's ending (LF, or CR LF) being one; ↑ ↓ PgUp PgDn keep the column in cells (a tab to the
//!   next multiple of [`TAB`], a wide character two); Ctrl+← → go by words (letters, digits, `_`).
//!   A character start is looked for from the line's start, or within [`WINDOW`] bytes before the
//!   place by the encoding's own rules (08 §7.3 rule 4), so a step costs at most that.
//! - **The block** is Turbo's ([`Block`]): two markers that stay where text is inserted at them,
//!   kept while the caret moves (persistent: typing does not replace it), shown while begin <
//!   end and not hidden. Del and Backspace delete it while it is shown; Shift with a move
//!   extends it from its other end.
//! - **The clipboard** (E36): Copy and Cut give a [`Clip`] — the decoded text and the block's
//!   pieces and bytes. A paste of exactly its text pastes the pieces (their bytes, the
//!   undecodable ones too); pieces another buffer made are stale here, so the bytes, when the
//!   encodings are one; any other paste holding U+FFFD is refused whole, before it is encoded.
//! - **New lines** (§7.2): a line break typed or pasted — LF, CR LF, a lone CR, as the
//!   terminal's paste gives them all as LF — is written as the ending of the line it splits,
//!   else the file's first ending, else LF. Enter autoindents while `Indent` is on.
//! - **Every edit is checked before it is made** ([`Doc::check`]), on the document it would
//!   make, so a refused one changes nothing and its undo history is untouched (the buffer
//!   clears redo at its first change): every part but the last a whole number of units (a file
//!   cut short in the middle of its last unit takes nothing after it, ED-buf's alignment); no
//!   typed or pasted text joined with the bytes beside it into another character (a CP949 lead
//!   with no trail takes no trail from what is typed: `8C` then `c` is not 똠) — where a delete
//!   or a block command brings the file's own bytes together they read as they read, and the
//!   caret and the block go to character starts; and no byte-order mark at byte 0 that was not
//!   there (U+FEFF typed or pasted there is refused; a delete or a move that brings one there
//!   is done, and says so).
//! - **The encoding question** (E37) is a state: the first non-ASCII character typed, pasted or
//!   given as a replacement in a file whose encoding was guessed or does not decode every byte
//!   is held until [`Editor::answer`].
//! - **Undo** (§7.3): typing (Overwrite's too) and Enter in one run, a Backspace run, a Del run;
//!   every other command one step; each with the caret and the block before and after it.
//! - **Find** (E28) matches decoded characters, a line's ending as one `\n`, with Turbo C 2.0's
//!   options ([`Options`]; G as the research note reads it, `research/editor.md` §1.4 [추정]).

use super::arm::{BlockOp, Command, Motion};
use super::buffer::{Buffer, Group, Original, Piece};
use crate::encoding::encode::Refused;
use crate::encoding::{Around, Cell, Encoding, AHEAD};
use std::collections::VecDeque;
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

/// The viewer's tab width (`gui/text.rs`'s `TAB`): Tab's spaces reach the next multiple of it.
pub const TAB: u64 = 4;

/// A character start is looked for at most this far back from a place (else the line's start).
const WINDOW: u64 = 4096;

/// Bytes decoded at a time when characters are read on, and when a search goes back.
const CHUNK: u64 = 16 * 1024;

/// How much of the file Tab's rule reads for indented lines.
const TABS_SAMPLE: u64 = 64 * 1024;

const FFFD: char = char::REPLACEMENT_CHARACTER;

/// Turbo's block: two markers, shown while the begin is before the end and it is not hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub begin: u64,
    pub end: u64,
    pub hidden: bool,
}

impl Block {
    /// The bytes the block covers while it is shown.
    pub fn shown(&self) -> Option<Range<u64>> {
        (!self.hidden && self.begin < self.end).then_some(self.begin..self.end)
    }
}

/// The caret and the block: what an undo or a redo gives back (§7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sel {
    pub caret: u64,
    pub block: Option<Block>,
}

/// The last Copy or Cut (E36): its decoded text, which the system clipboard holds, and beside
/// it the block's pieces and bytes, in memory only. The caller keeps the last one.
#[derive(Debug, Clone)]
pub struct Clip {
    text: String,
    pieces: Vec<Piece>,
    bytes: Vec<u8>,
    encoding: Encoding,
}

impl Clip {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Find's options, Turbo C 2.0's letters (§5.1; TC2-RG p. 418–419).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// U: ignore case.
    pub ignore_case: bool,
    /// W: whole words — no letter, digit or `_` right outside a match's word-character ends.
    pub whole_words: bool,
    /// B: backward from the caret.
    pub backward: bool,
    /// N: Replace without asking (the caller's confirmation is skipped: [`Editor::replace_all`]).
    pub no_ask: bool,
    /// G: the whole file — a Find starts at the file's start (its end, backward), a Replace
    /// takes every match.
    pub global: bool,
}

impl Options {
    /// The option field's letters, in any case, a 2-set jamo as its key's letter (§5.5).
    pub fn parse(letters: &str) -> Options {
        let mut o = Options::default();
        for c in letters.chars().filter_map(super::arm::letter_of) {
            match c {
                'U' => o.ignore_case = true,
                'W' => o.whole_words = true,
                'B' => o.backward = true,
                'N' => o.no_ask = true,
                'G' => o.global = true,
                _ => {}
            }
        }
        o
    }
}

/// A Find or Replace's needle and options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub needle: String,
    pub options: Options,
}

/// Why the encoding may be wrong (E37): step 2 or 3 of 08 §7.2 guessed it, or bytes it cannot
/// decode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Doubt {
    pub guessed: bool,
    pub undecodable: u64,
}

impl Doubt {
    /// The read's one decode pass (§7.2): the bytes of `original` its encoding cannot decode —
    /// a U+FFFD the bytes do not spell. `guessed` is the opening's `(guessed)`.
    pub fn of(original: &Original, guessed: bool) -> Doubt {
        let e = original.encoding();
        let bytes = original.bytes();
        let real = e.encode("\u{FFFD}").ok();
        let (mut at, mut undecodable) = (0, 0);
        while at < bytes.len() {
            let end = (at + CHUNK as usize).min(bytes.len());
            let (mut cells, mut used) = e.decode(&bytes[at..end], end == bytes.len());
            if used == 0 {
                (cells, used) = e.decode(&bytes[at..], true);
            }
            let mut x = at;
            for c in cells {
                let len = usize::from(c.len);
                if c.ch == FFFD && real.as_deref() != Some(&bytes[x..x + len]) {
                    undecodable += len as u64;
                }
                x += len;
            }
            at += used;
        }
        Doubt {
            guessed,
            undecodable,
        }
    }

    fn any(self) -> bool {
        self.guessed || self.undecodable > 0
    }
}

/// The encoding question's buttons (E37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Type,
    Cancel,
}

/// What the status row says after a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// Nothing changed, and why.
    Refused(String),
    /// The encoding question is up (E37): nothing is typed until it is answered.
    Asked(String),
    /// Done, and what to know about it.
    Warned(String),
}

/// `None`: done (or nothing to do), nothing to say.
pub type Outcome = Option<Said>;

/// What an edit writes, for its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Typed,
    Pasted,
    Replaced,
    Deleted,
    Moved,
    Copied,
    Indented,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Typed => "typed",
            Verb::Pasted => "pasted",
            Verb::Replaced => "replaced",
            Verb::Deleted => "deleted",
            Verb::Moved => "moved",
            Verb::Copied => "copied",
            Verb::Indented => "indented",
        }
    }

    /// Text the owner gives: a byte-order mark it would make at byte 0 is refused, where one
    /// a delete or a move brings there is only said.
    fn writes_text(self) -> bool {
        matches!(self, Verb::Typed | Verb::Pasted | Verb::Replaced)
    }
}

/// What a change puts where it removes.
#[derive(Debug, Clone)]
enum Put<'c> {
    Nothing,
    /// Encoded bytes, appended to the add buffer.
    Bytes(Vec<u8>),
    /// A range of the document as it is before the edit, as its pieces (Ctrl+K C, V).
    Old(Range<u64>),
    /// The caller's clip's pieces, its bytes where they are stale: nothing copied.
    Clip(&'c [Piece], &'c [u8]),
}

/// One change, in the document's offsets before the edit: `remove` bytes at `at`, then `put`.
#[derive(Debug, Clone)]
struct Change<'c> {
    at: u64,
    remove: u64,
    put: Put<'c>,
}

fn change(at: u64, remove: u64, put: Put<'_>) -> Change<'_> {
    Change { at, remove, put }
}

/// What a check found wrong (§7.2, E35, ED-ops's carried rules).
enum Wrong {
    /// Part of a unit inside the text; `end`: the file's own partial last unit, with something
    /// after it.
    Unit { end: bool },
    /// Typed or pasted text that would join the bytes beside it into one character.
    Join,
}

/// The document as an edit would make it: ranges of today's document and new bytes, in order.
enum Seg<'a> {
    Old(Range<u64>),
    New(&'a [u8]),
}

impl Seg<'_> {
    fn len(&self) -> u64 {
        match self {
            Seg::Old(r) => r.end - r.start,
            Seg::New(b) => b.len() as u64,
        }
    }
}

/// The encodings `pick` takes a byte-order mark for (08 §7.2 step 1), the longest first.
const BOMS: [Encoding; 5] = [
    Encoding::Utf32Le,
    Encoding::Utf32Be,
    Encoding::Utf8,
    Encoding::Utf16Le,
    Encoding::Utf16Be,
];

fn bom_of(head: &[u8]) -> Option<Encoding> {
    BOMS.into_iter().find(|e| head.starts_with(e.bom()))
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The columns `c` takes at column `col`: a tab to the next multiple of [`TAB`], a control
/// character its one CP437 glyph (08 §7.1), else its width.
fn width(c: char, col: u64) -> u64 {
    match c {
        '\t' => TAB - col % TAB,
        c if c.is_control() => 1,
        c => c.width().unwrap_or(1) as u64,
    }
}

/// Case folded for U: a character's lower case where it is one character.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// A character named on a line: itself, or its code point where it shows nothing.
fn name(c: char) -> String {
    if c.is_control() || c.is_whitespace() || c == '\u{FEFF}' {
        format!("U+{:04X}", u32::from(c))
    } else {
        c.to_string()
    }
}

/// How a document is read: the buffer and where its BOM ends. Every place it gives is a
/// character start or a line's end.
#[derive(Clone, Copy)]
struct Doc<'a> {
    b: &'a Buffer<Sel>,
    bom: u64,
}

impl<'a> Doc<'a> {
    fn enc(self) -> Encoding {
        self.b.encoding()
    }

    fn unit(self) -> u64 {
        self.enc().unit() as u64
    }

    fn len(self) -> u64 {
        self.b.len()
    }

    fn read(self, r: Range<u64>) -> Vec<u8> {
        let mut out = Vec::new();
        self.b.read(r, &mut out);
        out
    }

    fn line_of(self, x: u64) -> u64 {
        self.b.line_of(x)
    }

    fn last_line(self) -> u64 {
        self.b.lines() - 1
    }

    fn line_start(self, line: u64) -> u64 {
        if line == 0 {
            self.bom
        } else {
            self.b.line_start(line).unwrap_or(self.len())
        }
    }

    fn next_start(self, line: u64) -> Option<u64> {
        self.b.line_start(line + 1)
    }

    /// Where `line`'s ending starts (its CR, or its LF), or the document's end for the last.
    fn line_end(self, line: u64) -> u64 {
        let Some(next) = self.next_start(line) else {
            return self.len();
        };
        let e = self.enc();
        let lf = next - e.lf().len() as u64;
        let u = self.unit();
        if lf >= self.line_start(line) + u && self.read(lf - u..lf) == e.cr() {
            lf - u
        } else {
            lf
        }
    }

    /// `line`'s ending as it is, if it has one.
    fn ending(self, line: u64) -> Option<Vec<u8>> {
        let next = self.next_start(line)?;
        Some(self.read(self.line_end(line)..next))
    }

    /// The ending a new line splitting `line` takes (§7.2).
    fn new_line(self, line: u64) -> Vec<u8> {
        self.ending(line)
            .or_else(|| self.ending(0))
            .unwrap_or_else(|| self.enc().lf().to_vec())
    }

    /// `x`, or after the LF when `x` is strictly between a CR unit and its LF unit (E35).
    fn fix(self, x: u64) -> u64 {
        let u = self.unit();
        if x < u || x + u > self.len() {
            return x;
        }
        let pair = self.read(x - u..x + u);
        let e = self.enc();
        if pair[..u as usize] == *e.cr() && pair[u as usize..] == *e.lf() {
            x + u
        } else {
            x
        }
    }

    /// The cells of `from..to`, `from` a character start, each with where it starts; and where
    /// the cells end (short of `to` when not `whole`).
    fn cells(self, from: u64, to: u64, whole: bool) -> (Vec<(u64, Cell)>, u64) {
        let bytes = self.read(from..to);
        let (cells, used) = self.enc().decode(&bytes, whole);
        let mut at = from;
        let out = cells
            .into_iter()
            .map(|c| {
                let s = at;
                at += u64::from(c.len);
                (s, c)
            })
            .collect();
        (out, from + used as u64)
    }

    /// A character start at or before `x` on its line: the line's start, or one the encoding's
    /// rules prove within [`WINDOW`] bytes before `x` (08 §7.3 rule 4).
    fn anchor(self, x: u64) -> u64 {
        let ls = self.line_start(self.line_of(x));
        if x <= ls + WINDOW {
            return ls;
        }
        let e = self.enc();
        let g = e.align_down(x - WINDOW);
        let from = g.saturating_sub(e.look_back() as u64).max(ls);
        let bytes = self.read(from..g + AHEAD as u64);
        let near = if from == ls {
            Around::anchored(from, &bytes)
        } else {
            Around::new(from, &bytes)
        };
        match e.left_start(near, g) {
            Some(s) if s >= ls && s < x => s,
            _ => ls,
        }
    }

    /// The place one character before `x`: across a line's ending as one; `None` at the start.
    fn prev(self, x: u64) -> Option<u64> {
        let line = self.line_of(x);
        let ls = self.line_start(line);
        if x <= ls {
            return (line > 0).then(|| self.line_end(line - 1));
        }
        let (cells, _) = self.cells(self.anchor(x), x, true);
        cells.last().map(|&(s, _)| s)
    }

    /// The place one character after `x`; `None` at the end.
    fn next(self, x: u64) -> Option<u64> {
        let line = self.line_of(x);
        let le = self.line_end(line);
        if x >= le {
            return self.next_start(line);
        }
        let to = (x + 8).min(le);
        let (cells, _) = self.cells(x, to, to == le);
        Some(cells.first().map_or(le, |&(s, c)| s + u64::from(c.len)))
    }

    /// The characters from `x` (a character start) on, with where each starts and ends; a line's
    /// ending is one `'\n'`.
    fn chars_from(self, x: u64) -> Fwd<'a> {
        Fwd {
            doc: self,
            x,
            buf: VecDeque::new(),
        }
    }

    /// The characters before `x`, nearest first.
    fn chars_back(self, x: u64) -> Back<'a> {
        Back {
            doc: self,
            x,
            buf: Vec::new(),
        }
    }

    /// The column `x` is at, in cells.
    fn column(self, x: u64) -> u64 {
        let ls = self.line_start(self.line_of(x));
        let mut col = 0;
        for (_, c) in self.cells(ls, x, true).0 {
            col += width(c.ch, col);
        }
        col
    }

    /// The place on `line` at column `goal`, or the line's end.
    fn at_column(self, line: u64, goal: u64) -> u64 {
        let (ls, le) = (self.line_start(line), self.line_end(line));
        let mut col = 0;
        for (s, c) in self.cells(ls, le, true).0 {
            let w = width(c.ch, col);
            if col + w > goal {
                return s;
            }
            col += w;
        }
        le
    }

    fn word_right(self, x: u64) -> u64 {
        let mut in_word = true;
        let mut at = x;
        for (s, c, e) in self.chars_from(x) {
            if !is_word(c) {
                in_word = false;
            } else if !in_word {
                return s;
            }
            at = e;
        }
        at
    }

    fn word_left(self, x: u64) -> u64 {
        let mut seen = false;
        let mut at = x;
        for (s, c, _) in self.chars_back(x) {
            if is_word(c) {
                seen = true;
            } else if seen {
                return at;
            }
            at = s;
        }
        at
    }

    /// A place a click or a drag gives (byte `at`): the start of the character it is in, a
    /// line's end when inside its ending, after the LF when between it and its CR (E35).
    fn place(self, at: u64) -> u64 {
        let len = self.len();
        let at = if at >= len {
            len
        } else {
            self.enc().align_down(at).max(self.bom)
        };
        let line = self.line_of(at);
        let le = self.line_end(line);
        if at >= le {
            let past = self.fix(at);
            return if past > at { past } else { le };
        }
        let (cells, _) = self.cells(self.anchor(at), le, true);
        cells
            .iter()
            .take_while(|(s, _)| *s <= at)
            .last()
            .map_or(at, |&(s, _)| s)
    }

    /// The decoded text of `r`, `r.start` a character start, line endings as they are.
    fn text(self, r: Range<u64>) -> String {
        let mut out = String::new();
        let mut at = r.start;
        while at < r.end {
            let to = (at + CHUNK).min(r.end);
            let (cells, mut used) = self.cells(at, to, to == r.end);
            let cells = if used == at {
                let (cells, u) = self.cells(at, r.end, true);
                used = u;
                cells
            } else {
                cells
            };
            out.extend(cells.iter().map(|(_, c)| c.ch));
            at = used;
        }
        out
    }

    /// The end of the run of spaces and tabs `line` starts with.
    fn indent_end(self, line: u64) -> u64 {
        let ls = self.line_start(line);
        let le = self.line_end(line);
        self.chars_from(ls)
            .take_while(|&(s, c, _)| s < le && (c == ' ' || c == '\t'))
            .last()
            .map_or(ls, |(_, _, e)| e)
    }

    /// Tab's rule (§7.2): whether more indented lines in the file's first [`TABS_SAMPLE`]
    /// bytes start with a tab than with a space.
    fn indents_with_tabs(self) -> bool {
        let (mut tabs, mut spaces, mut start) = (0, 0, true);
        for (s, c, _) in self.chars_from(self.bom) {
            if s >= self.bom + TABS_SAMPLE {
                break;
            }
            if start {
                tabs += u32::from(c == '\t');
                spaces += u32::from(c == ' ');
            }
            start = c == '\n';
        }
        tabs > spaces
    }

    /// The lines a block covers: the line its begin is on to the line its end is on, but a
    /// last line the end only starts.
    fn lines_of(self, r: &Range<u64>) -> Range<u64> {
        let first = self.line_of(r.start);
        let mut last = self.line_of(r.end);
        if last > first && self.line_start(last) == r.end {
            last -= 1;
        }
        first..last + 1
    }

    // -----------------------------------------------------------------------------------------
    // The check before an edit.

    /// The document `changes` (sorted, apart) would make, as segments.
    fn segments<'c>(self, changes: &'c [Change<'_>]) -> Vec<Seg<'c>> {
        let mut segs = Vec::new();
        let mut from = 0;
        for c in changes {
            segs.push(Seg::Old(from..c.at));
            match &c.put {
                Put::Nothing => {}
                Put::Bytes(b) => segs.push(Seg::New(b)),
                Put::Clip(_, b) => segs.push(Seg::New(b)),
                Put::Old(r) => segs.push(Seg::Old(r.clone())),
            }
            from = c.at + c.remove;
        }
        segs.push(Seg::Old(from..self.len()));
        segs.retain(|s| s.len() > 0);
        segs
    }

    /// `r` of the document `segs` make.
    fn read_segs(self, segs: &[Seg], r: Range<u64>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0;
        for s in segs {
            let n = s.len();
            let (a, b) = (r.start.max(at), r.end.min(at + n));
            if a < b {
                match s {
                    Seg::Old(o) => self.b.read(o.start + a - at..o.start + b - at, &mut out),
                    Seg::New(bytes) => {
                        out.extend_from_slice(&bytes[(a - at) as usize..(b - at) as usize])
                    }
                }
            }
            at += n;
        }
        out
    }

    /// The bytes of 0x81–0xFE right before `p` in `segs`' document: CP949's pending lead is an
    /// odd run (`cp949.rs`'s phase rule; a line's start follows an LF, which ends the run).
    fn leads_before(self, segs: &[Seg], p: u64) -> u64 {
        let (mut n, mut q) = (0, p);
        while q > 0 {
            let from = q.saturating_sub(4096);
            let bytes = self.read_segs(segs, from..q);
            let k = bytes
                .iter()
                .rev()
                .take_while(|b| (0x81..=0xFE).contains(*b))
                .count();
            n += k as u64;
            if k < bytes.len() {
                break;
            }
            q = from;
        }
        n
    }

    /// Whether a character starting before seam `p` of `segs`' document (`total` bytes long)
    /// would take bytes after it: the bytes before `p` leave one unfinished (a UTF-8 sequence
    /// cut short, a UTF-16 high surrogate, a CP949 lead) and those after finish it.
    fn joins_at(self, segs: &[Seg], p: u64, total: u64) -> bool {
        let e = self.enc();
        let start = match e {
            Encoding::Utf8 | Encoding::Utf16Le | Encoding::Utf16Be => {
                let from = p.saturating_sub(e.look_back() as u64);
                let before = self.read_segs(segs, from..p);
                e.grid_cut(Around::new(from, &before), p).unwrap_or(p)
            }
            Encoding::Cp949 if self.leads_before(segs, p) % 2 == 1 => p - 1,
            _ => p,
        };
        if start == p {
            return false;
        }
        let to = (p + 8).min(total);
        let (cells, _) = e.decode(&self.read_segs(segs, start..to), to == total);
        let mut at = start;
        for c in cells {
            at += u64::from(c.len);
            if at >= p {
                return at > p;
            }
        }
        true
    }

    /// §7.2, E35 and ED-ops's carried rules over the document `changes` would make: whole units
    /// but at its end, and no typed or pasted text joined with the bytes beside it (carried rule
    /// (a)). Where a delete or a block command brings the file's own bytes together, they read
    /// as they read: a faithful decode, never refused (decision of 2026-11-25). `Ok`: the
    /// byte-order mark it would newly start with, if any.
    fn check(self, changes: &[Change<'_>]) -> Result<Option<Encoding>, Wrong> {
        let segs = self.segments(changes);
        let total: u64 = segs.iter().map(Seg::len).sum();
        let u = self.unit();
        if let Some((i, _)) = segs[..segs.len().saturating_sub(1)]
            .iter()
            .enumerate()
            .find(|(_, s)| s.len() % u != 0)
        {
            let end = matches!(&segs[i], Seg::Old(r) if r.end == self.len())
                && matches!(segs[i + 1], Seg::New(_));
            return Err(Wrong::Unit { end });
        }
        let mut p = 0;
        for w in segs.windows(2) {
            p += w[0].len();
            let typed = matches!(w[0], Seg::New(_)) || matches!(w[1], Seg::New(_));
            if typed && self.joins_at(&segs, p, total) {
                return Err(Wrong::Join);
            }
        }
        let head = |n: u64| self.read(0..n.min(self.len()));
        let new_head = self.read_segs(&segs, 0..4.min(total));
        let made = bom_of(&new_head);
        Ok(made.filter(|_| made != bom_of(&head(4))))
    }

    // -----------------------------------------------------------------------------------------
    // Find.

    /// The first match of `needle` (folded for U, line breaks as `'\n'`) starting at or after
    /// `from` (a character start) and before `before`; `last`: the last such match instead.
    fn scan(
        self,
        needle: &[char],
        o: Options,
        from: u64,
        before: u64,
        last: bool,
    ) -> Option<Range<u64>> {
        let m = needle.len();
        if m == 0 {
            return None;
        }
        let key = |c: char| if o.ignore_case { fold(c) } else { c };
        // The window's characters as a match, with the characters right before and after it.
        let hit = |win: &VecDeque<(u64, char, u64)>, prev: Option<char>, next: Option<char>| {
            if win.len() < m || win[0].0 >= before {
                return None;
            }
            if !win.iter().zip(needle).all(|(w, n)| key(w.1) == *n) {
                return None;
            }
            let outside = (is_word(needle[0]) && prev.is_some_and(is_word))
                || (is_word(needle[m - 1]) && next.is_some_and(is_word));
            if o.whole_words && outside {
                return None;
            }
            Some(win[0].0..win[m - 1].2)
        };
        let mut prev = self.chars_back(from).next().map(|(_, c, _)| c);
        let mut win: VecDeque<(u64, char, u64)> = VecDeque::with_capacity(m + 1);
        let mut found = None;
        for (s, c, e) in self.chars_from(from) {
            if win.len() == m && win[0].0 >= before {
                return found;
            }
            if let Some(r) = hit(&win, prev, Some(c)) {
                if !last {
                    return Some(r);
                }
                found = Some(r);
            }
            win.push_back((s, c, e));
            if win.len() > m {
                prev = win.pop_front().map(|w| w.1);
            }
        }
        hit(&win, prev, None).or(found)
    }

    fn find(self, needle: &[char], o: Options, from: u64) -> Option<Range<u64>> {
        if needle.is_empty() {
            return None;
        }
        if !o.backward {
            return self.scan(needle, o, from, u64::MAX, false);
        }
        let mut bound = from;
        while bound > self.bom {
            let s = if bound - self.bom <= CHUNK {
                self.bom
            } else {
                self.anchor(bound - CHUNK)
            };
            if let Some(r) = self.scan(needle, o, s, bound, true) {
                return Some(r);
            }
            bound = s;
        }
        None
    }
}

/// [`Doc::chars_from`]'s iterator.
struct Fwd<'a> {
    doc: Doc<'a>,
    x: u64,
    buf: VecDeque<(u64, char, u64)>,
}

impl Iterator for Fwd<'_> {
    type Item = (u64, char, u64);

    fn next(&mut self) -> Option<Self::Item> {
        if self.buf.is_empty() {
            let d = self.doc;
            let line = d.line_of(self.x);
            let le = d.line_end(line);
            if self.x < le {
                let to = (self.x + CHUNK).min(le);
                let (mut cells, mut used) = d.cells(self.x, to, to == le);
                if used == self.x {
                    (cells, used) = d.cells(self.x, le, true);
                }
                self.buf
                    .extend(cells.iter().map(|&(s, c)| (s, c.ch, s + u64::from(c.len))));
                self.x = used;
            } else {
                let next = d.next_start(line)?;
                self.buf.push_back((le, '\n', next));
                self.x = next;
            }
        }
        self.buf.pop_front()
    }
}

/// [`Doc::chars_back`]'s iterator.
struct Back<'a> {
    doc: Doc<'a>,
    x: u64,
    buf: Vec<(u64, char, u64)>,
}

impl Iterator for Back<'_> {
    type Item = (u64, char, u64);

    fn next(&mut self) -> Option<Self::Item> {
        if self.buf.is_empty() {
            let d = self.doc;
            let line = d.line_of(self.x);
            let ls = d.line_start(line);
            if self.x > ls {
                let s = d.anchor(self.x);
                let (cells, _) = d.cells(s, self.x, true);
                self.buf
                    .extend(cells.iter().map(|&(s, c)| (s, c.ch, s + u64::from(c.len))));
                self.x = s;
            } else if line > 0 {
                let le = d.line_end(line - 1);
                self.buf.push((le, '\n', self.x));
                self.x = le;
            }
        }
        self.buf.pop()
    }
}

/// Where the places before an edit are after it: each change's removed bytes collapse to its
/// start, a place at a change's start stays before what it puts (the markers' rule).
struct Moved(Vec<(u64, u64, u64)>);

impl Moved {
    fn to(&self, x: u64) -> u64 {
        let mut d: i128 = 0;
        for &(at, remove, put) in &self.0 {
            if x <= at {
                break;
            }
            if x < at + remove {
                return (i128::from(at) + d) as u64;
            }
            d += i128::from(put) - i128::from(remove);
        }
        (i128::from(x) + d) as u64
    }
}

/// An edit held by the encoding question (E37) until it is answered.
#[derive(Debug, Clone)]
enum Pending {
    Type(String),
    Paste(String),
    Replace(Range<u64>, String),
    ReplaceAll(Search, String),
}

/// One editor's document and its state.
pub struct Editor {
    buffer: Buffer<Sel>,
    sel: Sel,
    bom: u64,
    overwrite: bool,
    indent: bool,
    tabs: bool,
    /// The column ↑ ↓ keep, from the first of a run of them.
    goal: Option<u64>,
    page: u64,
    /// Until the question is answered Type: E37's reason, once.
    doubt: Option<Doubt>,
    question: Option<(Pending, String)>,
}

impl Editor {
    /// The editor on `original` as read at F4, in its encoding, `doubt` from the read's pass.
    /// The caret at the file's start, after a BOM; `Insert` and `Indent` on.
    pub fn open(original: Original, doubt: Doubt) -> Editor {
        let e = original.encoding();
        let head = &original.bytes()[..original.bytes().len().min(4)];
        let bom = e.bom_len(head) as u64;
        let mut ed = Editor {
            buffer: Buffer::new(original),
            sel: Sel {
                caret: bom,
                block: None,
            },
            bom,
            overwrite: false,
            indent: true,
            tabs: false,
            goal: None,
            page: 20,
            doubt: doubt.any().then_some(doubt),
            question: None,
        };
        ed.tabs = ed.doc().indents_with_tabs();
        ed
    }

    fn doc(&self) -> Doc<'_> {
        Doc {
            b: &self.buffer,
            bom: self.bom,
        }
    }

    /// F4's status line (E37): `read as UTF-16LE (guessed)`, `editor.rs holds 12 bytes UTF-8
    /// cannot decode — they stay as they are`, both, or `None`.
    pub fn opening_line(&self, name: &str) -> Option<String> {
        let d = self.doubt?;
        let label = self.buffer.encoding().label();
        let mut parts = Vec::new();
        if d.guessed {
            parts.push(format!("read as {label} (guessed)"));
        }
        match d.undecodable {
            0 => {}
            1 => parts.push(format!(
                "{name} holds 1 byte {label} cannot decode — it stays as it is"
            )),
            n => parts.push(format!(
                "{name} holds {n} bytes {label} cannot decode — they stay as they are"
            )),
        }
        Some(parts.join(" · "))
    }

    pub fn buffer(&self) -> &Buffer<Sel> {
        &self.buffer
    }

    pub fn sel(&self) -> Sel {
        self.sel
    }

    pub fn caret(&self) -> u64 {
        self.sel.caret
    }

    /// The block's bytes while it is shown.
    pub fn shown(&self) -> Option<Range<u64>> {
        self.sel.block.and_then(|b| b.shown())
    }

    pub fn bom(&self) -> u64 {
        self.bom
    }

    /// Overwrite, else Insert (the status row's `Insert`).
    pub fn overwrite(&self) -> bool {
        self.overwrite
    }

    /// Autoindent (the status row's `Indent`).
    pub fn set_indent(&mut self, on: bool) {
        self.indent = on;
    }

    pub fn indent(&self) -> bool {
        self.indent
    }

    /// The rows PgUp and PgDn move by: the edit window's text rows.
    pub fn set_page(&mut self, rows: u64) {
        self.page = rows.max(1);
    }

    /// The encoding question while it is up (E37).
    pub fn question(&self) -> Option<String> {
        self.question.as_ref().map(|(_, line)| line.clone())
    }

    // ---------------------------------------------------------------------------------------
    // The caret and the block.

    fn go(&mut self, to: u64, extend: bool) {
        let from = self.sel.caret;
        if extend {
            let anchor = match self.shown() {
                Some(r) if r.end == from => r.start,
                Some(r) if r.start == from => r.end,
                _ => from,
            };
            self.sel.block = Some(Block {
                begin: anchor.min(to),
                end: anchor.max(to),
                hidden: false,
            });
        }
        self.sel.caret = to;
        self.buffer.seal();
    }

    /// A move of the caret (§5.1); `extend`: Shift, the block from its other end.
    pub fn move_caret(&mut self, motion: Motion, extend: bool) {
        let d = Doc {
            b: &self.buffer,
            bom: self.bom,
        };
        let x = self.sel.caret;
        let line = d.line_of(x);
        let vertical = |goal: u64, delta: i64| -> u64 {
            let target = (line as i64 + delta).clamp(0, d.last_line() as i64) as u64;
            if target == line {
                x
            } else {
                d.at_column(target, goal)
            }
        };
        let page = self.page as i64;
        let to = match motion {
            Motion::Left => d.prev(x).unwrap_or(x),
            Motion::Right => d.next(x).unwrap_or(x),
            Motion::Home => d.line_start(line),
            Motion::End => d.line_end(line),
            Motion::WordLeft => d.word_left(x),
            Motion::WordRight => d.word_right(x),
            Motion::FileStart => self.bom,
            Motion::FileEnd => d.len(),
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let goal = *self.goal.get_or_insert_with(|| d.column(x));
                let delta = match motion {
                    Motion::Up => -1,
                    Motion::Down => 1,
                    Motion::PageUp => -page,
                    _ => page,
                };
                let to = vertical(goal, delta);
                self.go(to, extend);
                return;
            }
        };
        self.goal = None;
        self.go(to, extend);
    }

    /// A click (`extend` false) or a drag or Shift+click (true) at byte `at`.
    pub fn place(&mut self, at: u64, extend: bool) {
        let to = self.doc().place(at);
        self.goal = None;
        self.go(to, extend);
    }

    /// Ins.
    pub fn toggle_insert(&mut self) {
        self.overwrite = !self.overwrite;
    }

    /// Select all: the block from after the BOM to the end, the caret at the end.
    pub fn select_all(&mut self) {
        let len = self.buffer.len();
        self.sel.block = Some(Block {
            begin: self.bom,
            end: len,
            hidden: false,
        });
        self.sel.caret = len;
        self.goal = None;
        self.buffer.seal();
    }

    // ---------------------------------------------------------------------------------------
    // Making an edit.

    /// Checks `changes` (sorted, apart) and makes them as one undo step of `group`, or one more
    /// change of its open run; `after` gives the caret and the block from the document made and
    /// where the old places went. `ask`: text the encoding question holds, if it is up.
    fn commit(
        &mut self,
        group: Group,
        changes: Vec<Change<'_>>,
        verb: Verb,
        ask: Option<(char, Pending)>,
        after: impl FnOnce(Doc<'_>, &Moved, Sel) -> Sel,
    ) -> Outcome {
        let changes: Vec<Change<'_>> = changes
            .into_iter()
            .filter(|c| c.remove > 0 || !self.put_is_empty(&c.put))
            .collect();
        if changes.is_empty() {
            return None;
        }
        let label = self.buffer.encoding().label();
        let made = match self.doc().check(&changes) {
            Ok(made) => made,
            Err(Wrong::Unit { end: true }) => {
                return refuse(format!(
                    "the file ends in part of a {label} unit, so nothing goes after it — nothing was {}",
                    verb.word()
                ))
            }
            Err(Wrong::Unit { end: false }) => {
                return refuse(format!(
                    "the block ends in part of a {label} unit, which only the file's end can hold — nothing was {}",
                    verb.word()
                ))
            }
            Err(Wrong::Join) => {
                return refuse(format!(
                    "that would make one character of it and the undecodable bytes beside it — nothing was {}",
                    verb.word()
                ))
            }
        };
        let warned = match made {
            Some(b) if verb.writes_text() => {
                return refuse(format!(
                    "that would start the file with {}'s byte-order mark, which the next read skips — nothing was {}",
                    b.label(),
                    verb.word()
                ))
            }
            Some(b) => Some(Said::Warned(format!(
                "the file now starts with {}'s byte-order mark, which the next read skips",
                b.label()
            ))),
            None => None,
        };
        if let (Some((c, pending)), Some(d)) = (ask, self.doubt) {
            let why = if d.guessed {
                "The encoding was guessed".to_string()
            } else {
                format!("The file holds bytes {label} cannot decode")
            };
            let line = format!("Type {} as {label}? {why}", name(c));
            self.question = Some((pending, line.clone()));
            return Some(Said::Asked(line));
        }
        self.make(group, &changes, after);
        warned
    }

    fn put_is_empty(&self, put: &Put<'_>) -> bool {
        Self::put_len(put) == 0
    }

    fn put_len(put: &Put<'_>) -> u64 {
        match put {
            Put::Nothing => 0,
            Put::Bytes(b) => b.len() as u64,
            Put::Clip(_, b) => b.len() as u64,
            Put::Old(r) => r.end - r.start,
        }
    }

    /// Makes checked `changes`, the last first, each removing then putting at its place — but
    /// putting first where removing would leave a lone CR just before an LF, which the buffer's
    /// guard would put the text past (E35).
    fn make(
        &mut self,
        group: Group,
        changes: &[Change<'_>],
        after: impl FnOnce(Doc<'_>, &Moved, Sel) -> Sel,
    ) {
        let d = self.doc();
        let (cr, lf, u) = (d.enc().cr(), d.enc().lf(), d.unit());
        let plans: Vec<(Vec<Piece>, bool)> = changes
            .iter()
            .map(|c| {
                let pieces = match &c.put {
                    Put::Old(r) => self.buffer.pieces_in(r.clone()),
                    _ => Vec::new(),
                };
                let end = c.at + c.remove;
                let pair = c.remove > 0
                    && c.at >= u
                    && end + u <= d.len()
                    && d.read(c.at - u..c.at) == cr
                    && d.read(end..end + u) == lf;
                (pieces, pair)
            })
            .collect();
        let moved = Moved(
            changes
                .iter()
                .map(|c| (c.at, c.remove, Self::put_len(&c.put)))
                .collect(),
        );
        let before = self.sel;
        let bom = self.bom;
        let mut e = self.buffer.edit(group, before);
        for (c, (pieces, put_first)) in changes.iter().zip(&plans).rev() {
            let end = c.at + c.remove;
            let put = |e: &mut super::buffer::Edit<'_, Sel>, at: u64| match &c.put {
                Put::Nothing => {}
                Put::Bytes(b) => {
                    e.insert(at, b);
                }
                Put::Old(_) => {
                    let _ = e.insert_pieces(at, pieces);
                }
                Put::Clip(p, b) => {
                    if e.insert_pieces(at, p).is_err() {
                        e.insert(at, b);
                    }
                }
            };
            if *put_first {
                put(&mut e, end);
                e.delete(c.at..end);
            } else {
                e.delete(c.at..end);
                put(&mut e, c.at);
            }
        }
        let sel = after(Doc { b: &e, bom }, &moved, before);
        e.end(sel);
        self.sel = sel;
        self.goal = None;
    }

    /// The caret and the block where the edit moved them, each on a character start (where
    /// the file's own bytes now join into one character, its start) or past a CR LF (E35).
    fn kept(d: Doc<'_>, moved: &Moved, before: Sel) -> Sel {
        Sel {
            caret: d.place(moved.to(before.caret)),
            block: before.block.map(|b| Block {
                begin: d.place(moved.to(b.begin)),
                end: d.place(moved.to(b.end)),
                hidden: b.hidden,
            }),
        }
    }

    /// `text` in the file's encoding, each line break as `ending`: the first character it
    /// cannot hold refused.
    fn encode(&self, text: &str, ending: &[u8]) -> Result<Vec<u8>, Refused> {
        let e = self.buffer.encoding();
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(i) = rest.find(['\r', '\n']) {
            out.extend(e.encode(&rest[..i])?);
            out.extend_from_slice(ending);
            let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
            rest = &rest[i + skip..];
        }
        out.extend(e.encode(rest)?);
        Ok(out)
    }

    /// The question's text and edit, when it holds a character past ASCII.
    fn ask(text: &str, pending: Pending) -> Option<(char, Pending)> {
        text.chars().find(|c| !c.is_ascii()).map(|c| (c, pending))
    }

    // ---------------------------------------------------------------------------------------
    // Typing.

    /// Text typed at the caret (Insert), or over the characters there (Overwrite, never over a
    /// line's ending, E35).
    pub fn type_text(&mut self, text: &str) -> Outcome {
        if text.is_empty() {
            return None;
        }
        let d = self.doc();
        let x = self.sel.caret;
        let line = d.line_of(x);
        let bytes = match self.encode(text, &d.new_line(line)) {
            Ok(b) => b,
            Err(r) => return refuse(format!("{r} — nothing was typed")),
        };
        let mut remove = 0;
        if self.overwrite && !text.contains(['\r', '\n']) {
            let le = d.line_end(line);
            let mut end = x;
            for _ in text.chars() {
                match d.next(end).filter(|_| end < le) {
                    Some(n) => end = n,
                    None => break,
                }
            }
            remove = end - x;
        }
        let n = bytes.len() as u64;
        let ask = Self::ask(text, Pending::Type(text.to_string()));
        self.commit(
            Group::Typing,
            vec![change(x, remove, Put::Bytes(bytes))],
            Verb::Typed,
            ask,
            move |d, moved, before| Sel {
                caret: d.fix(x + n),
                ..Self::kept(d, moved, before)
            },
        )
    }

    /// Enter: the line's ending, then the line's indent while `Indent` is on.
    pub fn enter(&mut self) -> Outcome {
        self.new_line(true)
    }

    /// Ctrl+N: a line break at the caret, the caret staying.
    pub fn insert_line(&mut self) -> Outcome {
        self.new_line(false)
    }

    fn new_line(&mut self, enter: bool) -> Outcome {
        let d = self.doc();
        let x = self.sel.caret;
        let line = d.line_of(x);
        let mut bytes = d.new_line(line);
        if enter && self.indent {
            let ls = d.line_start(line);
            bytes.extend(d.read(ls..d.indent_end(line).min(x)));
        }
        let n = bytes.len() as u64;
        let group = if enter { Group::Typing } else { Group::Single };
        self.commit(
            group,
            vec![change(x, 0, Put::Bytes(bytes))],
            Verb::Typed,
            None,
            move |d, moved, before| Sel {
                // Ctrl+N's caret stays, but never between a CR and the LF put after it.
                caret: d.fix(if enter { x + n } else { x }),
                ..Self::kept(d, moved, before)
            },
        )
    }

    /// The indent one Tab, Ctrl+K I and Shift+Tab's unit stand for: a tab, or [`TAB`] spaces.
    fn unit_text(&self) -> &'static str {
        if self.tabs {
            "\t"
        } else {
            "    "
        }
    }

    /// Tab: a tab where the file indents with tabs, else spaces to the next multiple of [`TAB`].
    pub fn tab(&mut self) -> Outcome {
        if self.tabs {
            return self.insert_text("\t");
        }
        let col = self.doc().column(self.sel.caret);
        let spaces = " ".repeat((TAB - col % TAB) as usize);
        self.insert_text(&spaces)
    }

    /// ASCII text inserted at the caret, Overwrite or not.
    fn insert_text(&mut self, text: &str) -> Outcome {
        let overwrite = std::mem::replace(&mut self.overwrite, false);
        let said = self.type_text(text);
        self.overwrite = overwrite;
        said
    }

    // ---------------------------------------------------------------------------------------
    // Deleting.

    fn delete_range(&mut self, group: Group, r: Range<u64>) -> Outcome {
        let start = r.start;
        self.commit(
            group,
            vec![change(start, r.end - r.start, Put::Nothing)],
            Verb::Deleted,
            None,
            // Fragments the delete brings together may read as one character: the caret at its
            // start.
            move |d, moved, before| Sel {
                caret: d.place(start),
                ..Self::kept(d, moved, before)
            },
        )
    }

    /// Del: the block while shown, else the character right of the caret — a line's ending
    /// whole (E35).
    pub fn delete_right(&mut self) -> Outcome {
        if self.shown().is_some() {
            return self.delete_block();
        }
        let x = self.sel.caret;
        match self.doc().next(x) {
            Some(n) => self.delete_range(Group::Delete, x..n),
            None => None,
        }
    }

    /// Backspace: the block while shown, else the character left of the caret — a line's ending
    /// whole (E35); nothing at the file's start, after its BOM.
    pub fn delete_left(&mut self) -> Outcome {
        if self.shown().is_some() {
            return self.delete_block();
        }
        let x = self.sel.caret;
        match self.doc().prev(x) {
            Some(p) => self.delete_range(Group::Backspace, p..x),
            None => None,
        }
    }

    /// Ctrl+Y: the caret's line and its ending.
    pub fn delete_line(&mut self) -> Outcome {
        let d = self.doc();
        let line = d.line_of(self.sel.caret);
        let r = d.line_start(line)..d.next_start(line).unwrap_or(d.len());
        self.delete_range(Group::Single, r)
    }

    /// Ctrl+T: the word right of the caret and the space after it, up to the line's end; at
    /// the line's end, its ending.
    pub fn delete_word(&mut self) -> Outcome {
        let d = self.doc();
        let x = self.sel.caret;
        let le = d.line_end(d.line_of(x));
        let end = if x >= le {
            d.next(x).unwrap_or(x)
        } else {
            d.word_right(x).min(le)
        };
        self.delete_range(Group::Single, x..end)
    }

    /// Ctrl+Del, Ctrl+K Y: the block while shown.
    pub fn delete_block(&mut self) -> Outcome {
        let r = self.shown()?;
        let start = r.start;
        self.commit(
            Group::Single,
            vec![change(start, r.end - r.start, Put::Nothing)],
            Verb::Deleted,
            None,
            move |d, _, _| Sel {
                caret: d.place(start),
                block: None,
            },
        )
    }

    // ---------------------------------------------------------------------------------------
    // Blocks.

    /// Ctrl+K and its second key, but S (a Save, the binding's).
    pub fn block(&mut self, op: BlockOp) -> Outcome {
        let x = self.sel.caret;
        let marker = |b: Option<Block>| {
            b.unwrap_or(Block {
                begin: x,
                end: x,
                hidden: false,
            })
        };
        match op {
            BlockOp::Begin => {
                self.sel.block = Some(Block {
                    begin: x,
                    hidden: false,
                    ..marker(self.sel.block)
                });
            }
            BlockOp::End => {
                self.sel.block = Some(Block {
                    end: x,
                    hidden: false,
                    ..marker(self.sel.block)
                });
            }
            BlockOp::Hide => {
                if let Some(b) = &mut self.sel.block {
                    b.hidden = !b.hidden;
                }
            }
            BlockOp::MarkLine => {
                let d = self.doc();
                let line = d.line_of(x);
                let r = d.line_start(line)..d.next_start(line).unwrap_or(d.len());
                self.mark(r);
            }
            BlockOp::MarkWord => {
                let d = self.doc();
                let start = d
                    .chars_back(x)
                    .take_while(|&(_, c, _)| is_word(c))
                    .last()
                    .map_or(x, |(s, _, _)| s);
                let end = d
                    .chars_from(x)
                    .take_while(|&(_, c, _)| is_word(c))
                    .last()
                    .map_or(x, |(_, _, e)| e);
                if start < end {
                    self.mark(start..end);
                }
            }
            BlockOp::Delete => return self.delete_block(),
            BlockOp::Copy => return self.copy_block(),
            BlockOp::Move => return self.move_block(),
            BlockOp::Indent => return self.indent_lines(true),
            BlockOp::Unindent => return self.indent_lines(false),
        }
        self.buffer.seal();
        None
    }

    fn mark(&mut self, r: Range<u64>) {
        self.sel.block = Some(Block {
            begin: r.start,
            end: r.end,
            hidden: false,
        });
    }

    /// Ctrl+K C: the block's pieces at the caret; the copy is the block, the caret stays.
    fn copy_block(&mut self) -> Outcome {
        let r = self.shown()?;
        let x = self.sel.caret;
        let n = r.end - r.start;
        self.commit(
            Group::Single,
            vec![change(x, 0, Put::Old(r))],
            Verb::Copied,
            None,
            move |d, moved, _| {
                // A copy that starts with an LF right after a lone CR makes them one line end,
                // and its ends may join the bytes beside them: character starts.
                let at = d.place(moved.to(x));
                Sel {
                    caret: at,
                    block: Some(Block {
                        begin: at,
                        end: d.place(moved.to(x) + n),
                        hidden: false,
                    }),
                }
            },
        )
    }

    /// Ctrl+K V: the block's pieces moved to the caret; nothing when the caret is in it.
    fn move_block(&mut self) -> Outcome {
        let r = self.shown()?;
        let x = self.sel.caret;
        if (r.start..=r.end).contains(&x) {
            return None;
        }
        let n = r.end - r.start;
        let (cut, put) = (
            change(r.start, n, Put::Nothing),
            change(x, 0, Put::Old(r.clone())),
        );
        let changes = if x < r.start {
            vec![put, cut]
        } else {
            vec![cut, put]
        };
        let begin = if x < r.start { x } else { x - n };
        self.commit(Group::Single, changes, Verb::Moved, None, move |d, _, _| {
            let at = d.place(begin);
            Sel {
                caret: at,
                block: Some(Block {
                    begin: at,
                    end: d.place(begin + n),
                    hidden: false,
                }),
            }
        })
    }

    /// Ctrl+K I, Ctrl+K U, Shift+Tab: one indent unit put at, or taken from, the start of each
    /// line the block covers (or the caret's line, for Shift+Tab with no block). Indent leaves
    /// empty lines; unindent takes a tab or up to [`TAB`] spaces.
    fn indent_lines(&mut self, indent: bool) -> Outcome {
        let d = self.doc();
        let lines = match self.shown() {
            Some(r) => d.lines_of(&r),
            None if indent => return None,
            None => {
                let l = d.line_of(self.sel.caret);
                l..l + 1
            }
        };
        let unit = match self.encode(self.unit_text(), &[]) {
            Ok(u) => u,
            Err(r) => return refuse(format!("{r} — nothing was indented")),
        };
        let mut changes = Vec::new();
        for line in lines {
            let ls = d.line_start(line);
            if indent {
                if ls < d.line_end(line) {
                    changes.push(change(ls, 0, Put::Bytes(unit.clone())));
                }
                continue;
            }
            let mut end = ls;
            for (i, (_, c, e)) in d.chars_from(ls).enumerate() {
                if c == '\t' && i == 0 {
                    end = e;
                    break;
                }
                if c != ' ' || i as u64 == TAB {
                    break;
                }
                end = e;
            }
            if end > ls {
                changes.push(change(ls, end - ls, Put::Nothing));
            }
        }
        self.commit(Group::Single, changes, Verb::Indented, None, Self::kept)
    }

    /// Shift+Tab: unindent the block's lines, or the caret's line.
    pub fn unindent(&mut self) -> Outcome {
        self.indent_lines(false)
    }

    // ---------------------------------------------------------------------------------------
    // The clipboard (E36).

    /// Ctrl+Ins, Ctrl/⌘+C: the block's text for the clipboard, its pieces and bytes beside it.
    pub fn copy(&self) -> Option<Clip> {
        let r = self.shown()?;
        let d = self.doc();
        Some(Clip {
            text: d.text(r.clone()),
            pieces: self.buffer.pieces_in(r.clone()),
            bytes: d.read(r),
            encoding: self.buffer.encoding(),
        })
    }

    /// Shift+Del, Ctrl/⌘+X: the clip, and the block deleted; no clip when the delete is refused.
    pub fn cut(&mut self) -> (Option<Clip>, Outcome) {
        let Some(clip) = self.copy() else {
            return (None, None);
        };
        match self.delete_block() {
            Some(Said::Refused(line)) => (None, Some(Said::Refused(line))),
            said => (Some(clip), said),
        }
    }

    /// A paste of `text` at the caret, one undo step: the pieces of `clip` when `text` is its
    /// text (their bytes where another buffer made them, in the same encoding); else the text,
    /// refused whole when it holds U+FFFD (E36, before it is encoded) or a character the
    /// encoding cannot hold; its line breaks as the ending of the line it lands in.
    pub fn paste(&mut self, text: &str, clip: Option<&Clip>) -> Outcome {
        if text.is_empty() {
            return None;
        }
        let x = self.sel.caret;
        let e = self.buffer.encoding();
        let (put, ask) = match clip.filter(|c| c.text == text && c.encoding == e) {
            Some(c) => (Put::Clip(&c.pieces, &c.bytes), None),
            None => {
                if text.contains(FFFD) {
                    return refuse(
                        "the paste holds �, which stood for bytes that could not be decoded — nothing was pasted"
                            .to_string(),
                    );
                }
                let ending = self.doc().new_line(self.doc().line_of(x));
                match self.encode(text, &ending) {
                    Ok(b) => (
                        Put::Bytes(b),
                        Self::ask(text, Pending::Paste(text.to_string())),
                    ),
                    Err(r) => return refuse(format!("{r} — nothing was pasted")),
                }
            }
        };
        let n = Self::put_len(&put);
        self.commit(
            Group::Single,
            vec![change(x, 0, put)],
            Verb::Pasted,
            ask,
            move |d, moved, before| Sel {
                caret: d.fix(x + n),
                ..Self::kept(d, moved, before)
            },
        )
    }

    // ---------------------------------------------------------------------------------------
    // Undo, the question.

    /// Undo (§7.3): the caret and the block as they were before the step.
    pub fn undo(&mut self) -> bool {
        self.goal = None;
        match self.buffer.undo() {
            Some(sel) => {
                self.sel = sel;
                true
            }
            None => false,
        }
    }

    /// Redo: the caret and the block as they were after the step.
    pub fn redo(&mut self) -> bool {
        self.goal = None;
        match self.buffer.redo() {
            Some(sel) => {
                self.sel = sel;
                true
            }
            None => false,
        }
    }

    /// The encoding question answered (E37): Cancel types nothing; Type makes the held edit,
    /// and the question is not asked again for this file.
    pub fn answer(&mut self, answer: Answer) -> Outcome {
        let (pending, _) = self.question.take()?;
        if answer == Answer::Cancel {
            return None;
        }
        self.doubt = None;
        match pending {
            Pending::Type(t) => self.type_text(&t),
            Pending::Paste(t) => self.paste(&t, None),
            Pending::Replace(r, t) => self.replace_at(r, &t),
            Pending::ReplaceAll(s, t) => self.replace_all(&s, &t).1,
        }
    }

    // ---------------------------------------------------------------------------------------
    // Find and Replace (E28).

    fn needle(search: &Search) -> Vec<char> {
        let text = search.needle.replace("\r\n", "\n").replace('\r', "\n");
        text.chars()
            .map(|c| {
                if search.options.ignore_case {
                    fold(c)
                } else {
                    c
                }
            })
            .collect()
    }

    /// Find, or Find again (Ctrl+L, `again`): the next match from the caret — backward with B,
    /// from the file's start (end) with G on the first Find. The match becomes the block, the
    /// caret at its end (its start, backward). `None`: not found, nothing moved.
    pub fn find(&mut self, search: &Search, again: bool) -> Option<Range<u64>> {
        let o = search.options;
        let d = self.doc();
        let from = match (o.global && !again, o.backward) {
            (true, false) => self.bom,
            (true, true) => d.len(),
            _ => self.sel.caret,
        };
        let r = d.find(&Self::needle(search), o, from)?;
        self.mark(r.clone());
        self.sel.caret = if o.backward { r.start } else { r.end };
        self.goal = None;
        self.buffer.seal();
        Some(r)
    }

    /// `text` in place of `range` (a match Find gave), one undo step; the caret after it.
    pub fn replace_at(&mut self, range: Range<u64>, text: &str) -> Outcome {
        let d = self.doc();
        let ending = d.new_line(d.line_of(range.start));
        let bytes = match self.encode(text, &ending) {
            Ok(b) => b,
            Err(r) => return refuse(format!("{r} — nothing was replaced")),
        };
        let (at, n) = (range.start, bytes.len() as u64);
        let ask = Self::ask(text, Pending::Replace(range.clone(), text.to_string()));
        self.commit(
            Group::Single,
            vec![change(at, range.end - range.start, Put::Bytes(bytes))],
            Verb::Replaced,
            ask,
            move |d, _, _| Sel {
                caret: d.fix(at + n),
                block: None,
            },
        )
    }

    /// Replace with N (no asking), one undo step: every match from the caret on (backward, before
    /// it), or with G every match in the file. The count replaced.
    pub fn replace_all(&mut self, search: &Search, text: &str) -> (usize, Outcome) {
        let o = search.options;
        let d = self.doc();
        let needle = Self::needle(search);
        let (lo, hi) = match (o.global, o.backward) {
            (true, _) => (self.bom, u64::MAX),
            (false, false) => (self.sel.caret, u64::MAX),
            (false, true) => (self.bom, self.sel.caret),
        };
        let forward = Options {
            backward: false,
            ..o
        };
        let mut found: Vec<Range<u64>> = Vec::new();
        let mut from = lo;
        while let Some(r) = d.scan(&needle, forward, from, hi, false) {
            from = r.end;
            found.push(r);
        }
        if found.is_empty() {
            return (0, None);
        }
        let mut changes: Vec<Change<'_>> = Vec::new();
        for r in &found {
            let ending = d.new_line(d.line_of(r.start));
            let bytes = match self.encode(text, &ending) {
                Ok(b) => b,
                Err(e) => return (0, refuse(format!("{e} — nothing was replaced"))),
            };
            // Matches that touch become one change, so no part goes in next to another's seam.
            match changes.last_mut() {
                Some(c) if c.at + c.remove == r.start => {
                    c.remove += r.end - r.start;
                    if let Put::Bytes(b) = &mut c.put {
                        b.extend(bytes);
                    }
                }
                _ => changes.push(change(r.start, r.end - r.start, Put::Bytes(bytes))),
            }
        }
        let ask = Self::ask(text, Pending::ReplaceAll(search.clone(), text.to_string()));
        let said = self.commit(Group::Single, changes, Verb::Replaced, ask, Self::kept);
        let n = if matches!(said, Some(Said::Refused(_) | Said::Asked(_))) {
            0
        } else {
            found.len()
        };
        (n, said)
    }

    // ---------------------------------------------------------------------------------------
    // Keys.

    /// An editing command of [`Command`] (`edit::arm`), with the caller's last clip (E36): a
    /// Copy or a Cut replaces it. Every other command — Save, Find's field, the menus, leaving —
    /// is the binding's, and does nothing here.
    pub fn apply(&mut self, command: &Command, clip: &mut Option<Clip>) -> Outcome {
        match command {
            Command::Type(t) => self.type_text(t),
            Command::Move(m, extend) => {
                self.move_caret(*m, *extend);
                None
            }
            Command::ToggleInsert => {
                self.toggle_insert();
                None
            }
            Command::NewLine => self.enter(),
            Command::Tab => self.tab(),
            Command::Unindent => self.unindent(),
            Command::DeleteRight => self.delete_right(),
            Command::DeleteLeft => self.delete_left(),
            Command::DeleteLine => self.delete_line(),
            Command::DeleteWord => self.delete_word(),
            Command::InsertLine => self.insert_line(),
            Command::Block(op) => self.block(*op),
            Command::DeleteBlock => self.delete_block(),
            Command::SelectAll => {
                self.select_all();
                None
            }
            Command::Copy => {
                if let Some(c) = self.copy() {
                    *clip = Some(c);
                }
                None
            }
            Command::Cut => {
                let (c, said) = self.cut();
                if c.is_some() {
                    *clip = c;
                }
                said
            }
            Command::Paste(text) => self.paste(text, clip.as_ref()),
            Command::Undo { .. } => {
                self.undo();
                None
            }
            Command::Redo => {
                self.redo();
                None
            }
            _ => None,
        }
    }
}

fn refuse(line: String) -> Outcome {
    Some(Said::Refused(line))
}

#[cfg(test)]
mod tests;
