//! Word wrap for the text viewer: where one row of a line ends, from the row's first byte — by
//! the width egui lays the row out at (Hangul, CJK and emoji are wider than a cell; Arabic and
//! Hebrew are shaped right to left, an Indic syllable as one), only between grapheme clusters (a
//! letter and its marks, a syllable, an emoji sequence stay whole), at the last place a row may
//! break — after a space, or next to an ideograph or kana ([`Kind`]) — when what follows fits on
//! the next row, else where the row is full. A few layouts per row, however
//! the script is shaped: what fits is found by galloping and halving from an estimate.
//!
//! A row's end is a function of its first byte alone (and the width and fonts), and a line is
//! wrapped afresh from every [`RESTART`] bytes (the next word start, else cluster start): the rows of a
//! line are the same whether they are reached going down or going back up, and going back up
//! reads only from the restart point before the row — never the whole of a long line.
//!
//! Everything is keyed by byte offset (06 §7.2): a place ([`Pos`]) is where its line starts in
//! the file and the byte of it a row starts at, and the memos are keyed the same way, so rows
//! are found alike inside the line index and past it — where a line's start is found by
//! looking back from a place, or is not known and the line is cut in pieces on the same
//! [`RESTART`] grid ([`Span::loose`]). The CR before a line's end is dropped by the row that
//! holds the end ([`Wrapper::cut`]), not looked for when the line is placed: its last unit may
//! be far away and not fetched.
//!
//! The characters are the document's encoding's (08 §7.3, [`Encoding::decode`]): a restart
//! begins at the first character start its window's rule gives (rule 2), a place inside a line
//! not wrapped at the first one at or after it (rule 4), and every offset is a multiple of the
//! unit. Where no rule proves a character start (CP949, D28) the bytes show as placeholder
//! marks ([`MARK`], dim) up to the first start one proves, never as characters.

use super::{chrome, Span, Stale, TextDoc, Window_, LINE_MAX, REACH, TAB};
use crate::encoding::{Around, Encoding, AHEAD, W};
use anyhow::Result;
use eframe::egui::{self, Color32, FontId};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// A wrapped line starts a row afresh near every multiple of this many bytes (at the next word
/// start, else grapheme cluster start: [`Wrapper::restart`]): going back up one row reads about
/// this much of a line at most.
pub(super) const RESTART: u64 = 16 * 1024;

/// Bytes read before a restart point to find the grapheme cluster it falls in…
const RESTART_BEHIND: u64 = 64;
/// …and after it, for the cluster's end or the next word's start.
const RESTART_AHEAD: u64 = 128;

/// A character of a line as decoded: what it is (U+FFFD for bytes that are not one) and the
/// bytes it takes in the file — the encoding core's, one decoder per encoding.
pub(super) use crate::encoding::Cell;

/// A character as a row paints it: a C1 control (U+0080–U+009F, which Latin-1 makes of
/// 0x80–0x9F) as a dim `·` (08 D32: it is valid, so not U+FFFD, and has no glyph), anything else
/// as the skin shows it ([`chrome::shown_char`]). One cell either way: no byte moves.
pub(super) fn face(c: char) -> char {
    if faint(c) {
        '·'
    } else {
        chrome::shown_char(c)
    }
}

/// Whether `c` is painted dim as its face: a C1 control (D32).
fn faint(c: char) -> bool {
    ('\u{80}'..='\u{9F}').contains(&c)
}

/// What stands for a byte no rule proves a character start before (08 §7.4, D28), painted dim:
/// a mark is honest where a syllable read out of phase would not be.
pub(super) const MARK: char = '░';

/// `bytes` of a line from its byte `s`, where `s` is no character start proven, as characters
/// (`whole`: as [`Encoding::decode`]'s): placeholder marks up to `start`, the first one proven
/// — when `None`, the first a rule proves in them, the window's first sync byte (rule 2,
/// unanchored) — then the characters from there; all marks when none is. Returns the
/// characters, the bytes they cover and how many of them are marks.
fn marked(
    enc: Encoding,
    s: u64,
    bytes: &[u8],
    whole: bool,
    start: Option<u64>,
) -> (Vec<Cell>, usize, usize) {
    let from = start
        .or_else(|| enc.restart_start(Around::new(s, bytes), s))
        .map_or(bytes.len(), |t| ((t - s) as usize).min(bytes.len()));
    let unit = enc.unit();
    let mut cells: Vec<Cell> = (0..from.div_ceil(unit))
        .map(|i| Cell {
            ch: MARK,
            len: unit.min(from - i * unit) as u8,
        })
        .collect();
    let marks = cells.len();
    let (rest, used) = enc.decode(&bytes[from..], whole);
    cells.extend(rest);
    (cells, from + used, marks)
}

/// How a row is measured: egui's own layout of the text, in the viewer's font.
pub(super) struct Measure<'a> {
    ctx: &'a egui::Context,
    font: FontId,
    /// The colour rows are laid out in (the plain one: a painted plain row is the same galley).
    color: Color32,
    /// The advance of a space (a tab is spaces to the next stop).
    pub space: f32,
}

impl<'a> Measure<'a> {
    pub fn new(ctx: &'a egui::Context, font: FontId, color: Color32) -> Self {
        let space = ctx.fonts_mut(|f| f.glyph_width(&font, ' '));
        Self {
            ctx,
            font,
            color,
            space,
        }
    }

    /// Each character's advance by the font's glyph — an estimate (shaping can differ), only to
    /// choose how much text to lay out.
    fn estimate(&self, chars: &[char]) -> Vec<f32> {
        self.ctx.fonts_mut(|f| {
            chars
                .iter()
                .map(|&c| f.glyph_width(&self.font, c))
                .collect()
        })
    }

    /// `text` laid out on one row, as it is painted.
    pub fn layout(&self, text: String) -> Arc<egui::Galley> {
        #[cfg(test)]
        LAID.with(|l| {
            let (n, chars) = l.get();
            l.set((n + 1, chars + text.chars().count()));
        });
        self.ctx
            .fonts_mut(|f| f.layout_no_wrap(text, self.font.clone(), self.color))
    }
}

#[cfg(test)]
thread_local! {
    static LAID: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Tests: layouts wrapping made on this thread so far, and the characters laid out in them.
#[cfg(test)]
pub(super) fn laid() -> (usize, usize) {
    LAID.with(std::cell::Cell::get)
}

/// The most bytes one row of a window `width` points wide is read from, in cells `cell` points
/// wide: 16 per column and a little (a row of zero-width characters is cut there).
pub(super) fn read_cap(width: f32, cell: f32) -> usize {
    16 * (width / cell.max(1.0)).ceil().max(1.0) as usize + 256
}

/// One row: the characters it takes from its first byte (`taken`, `bytes` long) and those it
/// shows (`shown`: a space the row broke at is taken, not shown) — each as painted, with how
/// many times (a tab is spaces to the next stop, counted from the row's start; a control
/// character its code page 437 glyph).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Cut {
    pub taken: usize,
    pub bytes: usize,
    pub shown: Vec<(char, u8)>,
    /// How many of the first `shown` are placeholder marks ([`MARK`], painted dim).
    pub dim: usize,
    /// Which of `shown` are C1 controls' dim `·` (08 D32), in order.
    pub dots: Vec<usize>,
}

impl Cut {
    /// The row as one string.
    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.shown.len());
        for &(c, n) in &self.shown {
            s.extend(std::iter::repeat_n(c, usize::from(n)));
        }
        s
    }
}

/// What a grapheme cluster is to line breaking (after UAX #14, much simplified).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A space or a tab: a row may break after it.
    Space,
    /// An ideograph, kana, a fullwidth form: a row may break before and after it (Chinese and
    /// Japanese are written without spaces). Hangul is not: Korean words are spaced.
    Wide,
    /// Closing punctuation and the like (。、」), never at the start of a row; `true`: a
    /// Chinese / Japanese one, after which a row may break.
    Close(bool),
    /// Opening punctuation (「（), never at the end of a row.
    Open,
    /// Anything else: part of a word.
    Other,
}

impl Kind {
    fn of(cluster: &str) -> Self {
        let Some(c) = cluster.chars().next() else {
            return Kind::Other;
        };
        match c {
            ' ' | '\t' => Kind::Space,
            '、' | '。' | '〃' | '々' | '〆' | '〉' | '》' | '」' | '』' | '】' | '〕' | '〗'
            | '〙' | '〛' | '〞' | '〟' | '〻' | 'ゝ' | 'ゞ' | '゛' | '゜' | 'ー' | 'ヽ' | 'ヾ'
            | '・' | '！' | '）' | '，' | '．' | '：' | '；' | '？' | '］' | '｝' | '｠' | '｡'
            | '｣' | '､' | '･' | 'ｰ' => Kind::Close(true),
            // ’ and ” too: "don’t" is one word.
            ')' | ']' | '}' | ',' | '.' | ';' | ':' | '!' | '?' | '%' | '’' | '”' => {
                Kind::Close(false)
            }
            '〈' | '《' | '「' | '『' | '【' | '〔' | '〖' | '〘' | '〚' | '〝' | '（' | '［'
            | '｛' | '｟' | '｢' | '‘' | '“' | '(' | '[' | '{' => Kind::Open,
            '\u{2E80}'..='\u{2FFF}'
            | '\u{3000}'..='\u{303F}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3100}'..='\u{312F}'
            | '\u{3190}'..='\u{31FF}'
            | '\u{3200}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{A000}'..='\u{A4CF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FE30}'..='\u{FE4F}'
            | '\u{FF01}'..='\u{FF60}'
            | '\u{FF66}'..='\u{FF9F}'
            | '\u{FFE0}'..='\u{FFE6}'
            | '\u{20000}'..='\u{3FFFF}' => Kind::Wide,
            _ => Kind::Other,
        }
    }
}

/// A row's candidates: the grapheme clusters of what is read from the row's first byte, each as
/// painted (a tab as spaces to the next stop counted from the row's start, by the font's glyph
/// widths; a control character as its code page 437 glyph).
struct Clusters {
    /// The text as painted.
    text: String,
    /// After each cluster: where it ends in `text`, its characters so far, the estimated width
    /// so far (the font's glyph widths summed — shaping can differ: only to choose what to lay
    /// out); and what it is to line breaking.
    ends: Vec<usize>,
    chars: Vec<usize>,
    guess: Vec<f32>,
    kind: Vec<Kind>,
    /// Each character as painted, with how many times (a tab: spaces).
    shown: Vec<(char, u8)>,
    /// Which characters are C1 controls (painted dim, D32).
    dots: Vec<usize>,
}

impl Clusters {
    fn new(cells: &[Cell], m: &Measure) -> Self {
        use unicode_segmentation::UnicodeSegmentation;
        // Tabs stay tabs here (a cluster of their own), widened below.
        let face: String = cells
            .iter()
            .map(|c| if c.ch == '\t' { '\t' } else { face(c.ch) })
            .collect();
        let face_chars: Vec<char> = face.chars().collect();
        let widths = m.estimate(&face_chars);
        let mut out = Clusters {
            text: String::with_capacity(face.len()),
            ends: Vec::new(),
            chars: Vec::new(),
            guess: Vec::new(),
            kind: Vec::new(),
            shown: Vec::with_capacity(face_chars.len()),
            dots: (0..cells.len()).filter(|&i| faint(cells[i].ch)).collect(),
        };
        let (mut x, mut i) = (0f32, 0usize);
        for g in face.graphemes(true) {
            for c in g.chars() {
                if c == '\t' {
                    let col = (x / m.space + 0.01).floor() as usize;
                    let wide = TAB - col % TAB;
                    out.text.extend(std::iter::repeat_n(' ', wide));
                    out.shown.push((' ', wide as u8));
                    x += wide as f32 * m.space;
                } else {
                    out.text.push(c);
                    out.shown.push((c, 1));
                    x += widths[i];
                }
                i += 1;
            }
            out.ends.push(out.text.len());
            out.chars.push(i);
            out.guess.push(x);
            out.kind.push(Kind::of(g));
        }
        out
    }

    /// A row may break before cluster `i` (0 < `i` < the clusters read): after a space, or
    /// next to an ideograph or kana — never before closing punctuation or a space, never after
    /// opening punctuation.
    fn breaks_before(&self, i: usize) -> bool {
        let (before, here) = (self.kind[i - 1], self.kind[i]);
        if matches!(here, Kind::Close(_) | Kind::Space) || before == Kind::Open {
            return false;
        }
        matches!(before, Kind::Space | Kind::Wide | Kind::Close(true)) || here == Kind::Wide
    }

    fn len(&self) -> usize {
        self.ends.len()
    }

    /// The first `k` clusters as painted.
    fn prefix(&self, k: usize) -> &str {
        &self.text[..if k == 0 { 0 } else { self.ends[k - 1] }]
    }

    /// Clusters `from..to` as painted.
    fn part(&self, from: usize, to: usize) -> &str {
        &self.text[self.prefix(from).len()..self.prefix(to).len()]
    }

    /// The estimated width of the first `k` clusters.
    fn guess(&self, k: usize) -> f32 {
        if k == 0 {
            0.0
        } else {
            self.guess[k - 1]
        }
    }
}

/// The row of `cells` (a line's characters from a row start) that fits `width` points, broken
/// only between grapheme clusters. `complete`: the cells run to where the row must end anyway
/// (the line's end or a restart point); otherwise more of the line follows them.
///
/// Widths are what egui lays out — a prefix of the row is laid out as a whole, so its glyphs'
/// order and shaping (right-to-left scripts come out in visual order, an Indic syllable is shaped
/// as one) never matter. The most clusters that fit are found from the estimate by galloping and
/// halving: a few layouts per row (two when the estimate is right, as in a monospace font).
pub(super) fn cut_row(cells: &[Cell], complete: bool, width: f32, m: &Measure) -> Cut {
    if cells.is_empty() {
        return Cut {
            taken: 0,
            bytes: 0,
            shown: Vec::new(),
            dim: 0,
            dots: Vec::new(),
        };
    }
    let c = Clusters::new(cells, m);
    let total = c.len();
    let mut widths: Vec<Option<f32>> = vec![None; total + 1];
    let mut wide = |k: usize| -> f32 {
        *widths[k].get_or_insert_with(|| m.layout(c.prefix(k).to_string()).size().x)
    };
    // Laid out: twice the row and a little, so a word that does not end in it is longer than a
    // row; more when it all fits after all (the estimate was too wide).
    let mut budget = 2.0 * width + 8.0 * m.space;
    let (n, k, more) = loop {
        let n = (1..=total)
            .take_while(|&k| c.guess(k - 1) <= budget)
            .last()
            .unwrap_or(1);
        let more = n < total;
        let mut fits = |k: usize| wide(k) <= width + 0.01;
        // The estimate's choice, then galloping to the last that fits and halving between.
        let guess = (1..=n)
            .take_while(|&k| c.guess(k) <= width + 0.01)
            .last()
            .unwrap_or(1);
        let (mut lo, mut hi) = if fits(guess) {
            let (mut lo, mut step) = (guess, 1);
            let mut hi = None;
            while lo < n {
                let probe = (lo + step).min(n);
                if fits(probe) {
                    lo = probe;
                    step *= 2;
                } else {
                    hi = Some(probe);
                    break;
                }
            }
            match hi {
                Some(hi) => (lo, hi),
                None if more => {
                    budget *= 2.0;
                    continue;
                }
                None => break (n, n, more),
            }
        } else {
            let (mut hi, mut step) = (guess, 1);
            let mut lo = None;
            while hi > 1 {
                let probe = hi.saturating_sub(step).max(1);
                if fits(probe) {
                    lo = Some(probe);
                    break;
                }
                hi = probe;
                step *= 2;
            }
            match lo {
                Some(lo) => (lo, hi),
                // Not even one cluster fits (a window narrower than a glyph): one anyway.
                None => break (n, 1, more),
            }
        };
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if fits(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        break (n, lo, more);
    };
    let (mut taken, mut shown) = (k, k);
    if k < n {
        if c.part(k, k + 1) == " " {
            // The space the row is full at is taken, not shown: the next row starts with the
            // next word.
            taken = k + 1;
        } else if !c.breaks_before(k) {
            // Inside a word (or before a closing mark): back to the last place a row may break,
            // if what follows it up to the next such place — spaces after it not counted — fits
            // on the next row (laid out on its own) and the row up to it fits.
            if let Some(b) = (1..k).rev().find(|&b| c.breaks_before(b)) {
                let word_end = (k + 1..n).find(|&e| c.breaks_before(e));
                let word_end = match word_end {
                    Some(e) => Some(e),
                    None if !more && complete => Some(n),
                    // It runs past what was laid out (longer than a row), or past what was read.
                    None => None,
                };
                let fits_next = word_end.is_some_and(|e| {
                    let e = (b + 1..=e)
                        .rev()
                        .find(|&e| c.kind[e - 1] != Kind::Space)
                        .unwrap_or(e);
                    m.layout(c.part(b, e).to_string()).size().x <= width + 0.01
                });
                if fits_next && wide(b) <= width + 0.01 {
                    taken = b;
                    shown = b;
                }
            }
        }
    }
    let chars = |k: usize| if k == 0 { 0 } else { c.chars[k - 1] };
    let (taken, shown) = (chars(taken), chars(shown));
    Cut {
        taken,
        bytes: cells[..taken].iter().map(|c| usize::from(c.len)).sum(),
        shown: c.shown[..shown].to_vec(),
        dim: 0,
        dots: c.dots.iter().copied().filter(|&i| i < shown).collect(),
    }
}

/// A place in the file, by bytes (06 §7.2): where its line starts — or, for a line whose start
/// is not known, the piece of it that begins on the 16 KiB grid (`loose`) — and the byte of
/// that a row starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) struct Pos {
    pub at: u64,
    pub off: u64,
    pub loose: bool,
}

impl Pos {
    /// The first row of `span`.
    pub fn of(span: &Span) -> Self {
        Self {
            at: span.start,
            off: 0,
            loose: span.loose,
        }
    }
}

/// A row on screen: where its line starts in the file (`loose`: only a piece of a line whose
/// start is not known), the line's number when the index reaches it, its bytes of it, what it
/// shows, and — for a line short enough to be highlighted — the index of its first character
/// in the line (colours are per character).
#[derive(Debug, Clone)]
pub(super) struct Row {
    pub at: u64,
    pub line: Option<u64>,
    pub loose: bool,
    pub start: u64,
    pub cut: Rc<Cut>,
    pub first_char: Option<usize>,
}

/// Entries of each kind [`Memo`] keeps before it forgets them all (memory stays bounded).
const MEMO_MAX: usize = 4096;

/// What wrapping found so far, for one index and one width, keyed by where a line starts in the
/// file: line places, restart points, rows and each restart segment's row starts. Forgotten
/// when the width or the index changes.
#[derive(Default)]
pub(super) struct Memo {
    width: f32,
    spans: HashMap<u64, Span>,
    restarts: HashMap<(u64, u64), u64>,
    rows: HashMap<(u64, u64), Rc<Cut>>,
    segments: HashMap<(u64, u64), Rc<Vec<u64>>>,
    window: Window_,
}

impl Memo {
    /// Forgets everything (another index); rows are then wrapped for `width`.
    pub fn reset(&mut self, width: f32) {
        *self = Memo {
            width,
            ..Memo::default()
        };
    }

    /// Whether the rows found so far are for `width` (else it forgets them: true then).
    pub fn rewidth(&mut self, width: f32) -> bool {
        if self.width == width {
            return false;
        }
        self.reset(width);
        true
    }
}

/// Inserts into a memo map, forgetting it whole first when it is full.
fn remember<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, V>, key: K, value: V) {
    if map.len() >= MEMO_MAX {
        map.clear();
    }
    map.insert(key, value);
}

/// Rows of the file at one width: reads through the index, or past it by bytes (bounded,
/// [`Stale`] when the file is not what was indexed), measures with [`Measure`], remembers in
/// [`Memo`].
pub(super) struct Wrapper<'a> {
    pub doc: &'a mut TextDoc,
    pub memo: &'a mut Memo,
    pub m: Measure<'a>,
    pub width: f32,
    /// The most bytes one row is read from.
    pub read: usize,
}

impl Wrapper<'_> {
    /// The line (or piece) `p` is in: where it is in the file.
    pub fn span(&mut self, p: Pos) -> Result<Span> {
        if let Some(span) = self.memo.spans.get(&p.at) {
            return Ok(*span);
        }
        let span = self
            .doc
            .span_starting(p.at, p.loose, &mut self.memo.window)?;
        if let Some(line) = span.line {
            // A few lines around it at a time, through the index.
            for near in self.doc.spans(line.saturating_sub(15), 32)? {
                remember(&mut self.memo.spans, near.start, near);
            }
        }
        remember(&mut self.memo.spans, span.start, span);
        Ok(span)
    }

    /// Where line `line` of the index is.
    pub fn line(&mut self, line: u64) -> Result<Span> {
        let found = self.doc.spans(line.saturating_sub(15), 32)?;
        // Taken from what was read, not the memo: remembering may have forgotten it already.
        let wanted = found.iter().find(|s| s.line == Some(line)).copied();
        for span in found {
            remember(&mut self.memo.spans, span.start, span);
        }
        // The index said the line is there.
        wanted.ok_or_else(|| anyhow::Error::new(Stale))
    }

    /// What follows `span` in the file, if anything does.
    fn after(&mut self, span: Span) -> Result<Option<Span>> {
        if let Some(next) = self
            .memo
            .spans
            .get(&span.next)
            .filter(|_| span.next > span.start)
        {
            return Ok(Some(*next));
        }
        let next = self.doc.span_after(&span, &mut self.memo.window)?;
        if let Some(next) = next {
            remember(&mut self.memo.spans, next.start, next);
        }
        Ok(next)
    }

    /// What comes before `span` in the file, if anything does.
    fn before(&mut self, span: Span) -> Result<Option<Span>> {
        let prev = self.doc.span_before(&span, &mut self.memo.window)?;
        if let Some(prev) = prev {
            remember(&mut self.memo.spans, prev.start, prev);
        }
        Ok(prev)
    }

    /// Restart point `k` of `span`: the first word start (after a space) at or after `k` ×
    /// [`RESTART`] within [`RESTART_AHEAD`] bytes, else the first grapheme cluster start there
    /// (clusters as found from [`RESTART_BEHIND`] bytes before it), else — a cluster longer than
    /// that — the first character start; or the line's end. A word is split there only when no
    /// space is that near.
    fn restart(&mut self, span: Span, k: u64) -> Result<u64> {
        use unicode_segmentation::UnicodeSegmentation;
        let at = k.saturating_mul(RESTART);
        if k == 0 || at >= span.len {
            return Ok(at.min(span.len));
        }
        if let Some(r) = self.memo.restarts.get(&(span.start, k)) {
            return Ok(*r);
        }
        let enc = self.doc.enc;
        let from = at - RESTART_BEHIND;
        let n = (RESTART_BEHIND + RESTART_AHEAD) as usize;
        let bytes = self
            .doc
            .span_bytes(span, from, n, &mut Window_::default(), false)?;
        // From the first character start in what was read (08 §7.3 rule 2). None proven there
        // (CP949, D28): the restart is at the point itself, its row shown from the first start
        // a rule proves ([`Wrapper::cells`]).
        let near = Around::new(from, &bytes);
        let Some(lead) = enc.restart_start(near, from) else {
            let r = at.min(span.len);
            remember(&mut self.memo.restarts, (span.start, k), r);
            return Ok(r);
        };
        let lead = ((lead - from) as usize).min(bytes.len());
        let whole = from + bytes.len() as u64 == span.len;
        let (cells, _) = enc.decode(&bytes[lead..], whole);
        let text: String = cells.iter().map(|c| c.ch).collect();
        // Each cluster's start in the line, and whether it is a space; the line's end, if read.
        let mut starts: Vec<(u64, bool)> = Vec::new();
        let mut o = from + lead as u64;
        let mut cell = 0usize;
        for g in text.graphemes(true) {
            starts.push((o, g == " " || g == "\t"));
            let chars = g.chars().count();
            o += cells[cell..cell + chars]
                .iter()
                .map(|c| u64::from(c.len))
                .sum::<u64>();
            cell += chars;
        }
        if whole {
            starts.push((o, false));
        }
        let word = starts
            .windows(2)
            .find(|w| w[1].0 >= at && w[0].1)
            .map(|w| w[1].0);
        let cluster = starts.iter().find(|g| g.0 >= at).map(|g| g.0);
        let r = match word.or(cluster) {
            Some(r) => r,
            // A cluster longer than the window: the first character start at the point itself.
            None => enc.restart_start(near, at).unwrap_or(at),
        };
        let r = r.min(span.len);
        remember(&mut self.memo.restarts, (span.start, k), r);
        Ok(r)
    }

    /// The first restart point after byte `s` of `span`, or its end.
    fn restart_after(&mut self, span: Span, s: u64) -> Result<u64> {
        let k = s / RESTART;
        if k > 0 {
            let r = self.restart(span, k)?;
            if r > s {
                return Ok(r);
            }
        }
        self.restart(span, k + 1)
    }

    /// The last restart point before byte `x` (> 0) of `span`.
    fn restart_before(&mut self, span: Span, x: u64) -> Result<u64> {
        let mut k = (x - 1) / RESTART;
        loop {
            let r = self.restart(span, k)?;
            if r < x || k == 0 {
                return Ok(r);
            }
            k -= 1;
        }
    }

    /// The row of `span` that starts at byte `s`. The CR before a line's end is looked for
    /// here, in the row that holds the end, when the index did not read it (06 §7.2): it is
    /// taken by the row and not shown.
    fn cut(&mut self, span: Span, s: u64) -> Result<Rc<Cut>> {
        if let Some(cut) = self.memo.rows.get(&(span.start, s)) {
            return Ok(cut.clone());
        }
        let enc = self.doc.enc;
        let unit = enc.unit();
        let limit = self.restart_after(span, s)?.min(span.len);
        let want = limit.saturating_sub(s).min(self.read as u64) as usize;
        let mut bytes = self
            .doc
            .span_bytes(span, s, want, &mut self.memo.window, true)?;
        let complete = s + want as u64 == limit;
        let cr = span.cr && s + want as u64 == span.len && enc.ends_with_cr(&bytes, s);
        if cr {
            bytes.truncate(bytes.len() - unit);
        }
        // A row starts where the row before it ended — a character start — but in an encoding
        // that needs its start proven (D28).
        let (cells, _, dim) = if enc.needs_phase() {
            self.cells(span, s, &bytes, complete)?
        } else {
            let (cells, used) = enc.decode(&bytes, complete);
            (cells, used, 0)
        };
        let mut cut = cut_row(&cells, complete, self.width, &self.m);
        cut.dim = dim.min(cut.shown.len());
        if cr && cut.bytes == bytes.len() {
            cut.bytes += unit;
        }
        let cut = Rc::new(cut);
        remember(&mut self.memo.rows, (span.start, s), cut.clone());
        Ok(cut)
    }

    /// 08 §7.3 rule 4: the first character start of `span` at or after its byte `x` (a place
    /// inside the line, a row start), from [`Encoding::look_back`] bytes before it — or, in an
    /// encoding that needs a proof (CP949, §7.4), from its line start within [`REACH`], else
    /// [`W`] bytes before it. `None`: nothing proves one.
    fn start_at(&mut self, span: Span, x: u64) -> Result<Option<u64>> {
        let enc = self.doc.enc;
        if x == 0 && !span.blind {
            return Ok(Some(0));
        }
        let back = if !enc.needs_phase() {
            enc.look_back() as u64
        } else if !span.blind && x <= REACH {
            x
        } else {
            W as u64
        };
        let from = enc.align_down(x).saturating_sub(back);
        let n = (x - from) as usize + AHEAD;
        let window = &mut self.memo.window;
        let bytes = self.doc.span_bytes(span, from, n, window, false)?;
        let near = Around {
            at: from,
            bytes: &bytes,
            anchored: from == 0 && !span.blind,
        };
        Ok(enc.left_start(near, x))
    }

    /// `bytes` of `span` from its byte `s` as characters, decoded from the first character
    /// start at or after `s` ([`Wrapper::start_at`]) with placeholder marks before it, all of
    /// them marks when none is proven ([`marked`]). Returns the characters, the bytes they
    /// cover and how many of them are marks.
    fn cells(
        &mut self,
        span: Span,
        s: u64,
        bytes: &[u8],
        whole: bool,
    ) -> Result<(Vec<Cell>, usize, usize)> {
        let enc = self.doc.enc;
        Ok(match self.start_at(span, s)? {
            Some(t) if t == s => {
                let (cells, used) = enc.decode(bytes, whole);
                (cells, used, 0)
            }
            start => marked(enc, s, bytes, whole, start),
        })
    }

    /// `e` is where the rows of `span` end: its end, or the CR unit before it alone.
    fn ended(&mut self, span: Span, e: u64) -> Result<bool> {
        if e >= span.len {
            return Ok(true);
        }
        let unit = self.doc.enc.unit() as u64;
        Ok(span.cr && e + unit == span.len && self.cut(span, e)?.shown.is_empty())
    }

    /// The row starts of `span` from restart point `a` to the next one (or the line's end).
    fn segment(&mut self, span: Span, a: u64) -> Result<Rc<Vec<u64>>> {
        if let Some(starts) = self.memo.segments.get(&(span.start, a)) {
            return Ok(starts.clone());
        }
        let end = self.restart_after(span, a)?.min(span.len);
        let (mut starts, mut s) = (vec![a], a);
        loop {
            let e = s + self.cut(span, s)?.bytes as u64;
            if e >= end || e == s || self.ended(span, e)? {
                break;
            }
            starts.push(e);
            s = e;
        }
        let starts = Rc::new(starts);
        remember(&mut self.memo.segments, (span.start, a), starts.clone());
        Ok(starts)
    }

    /// Where the last row of `span` starts.
    fn last_start(&mut self, span: Span) -> Result<u64> {
        if span.len == 0 {
            return Ok(0);
        }
        let mut a = self.restart_before(span, span.len)?;
        if a > 0 && self.ended(span, a)? {
            // The restart point is on the CR before the line's end: no row starts there
            // (going down never makes one), so the last row is of the segment before.
            a = self.restart_before(span, a)?;
        }
        Ok(*self.segment(span, a)?.last().unwrap_or(&a))
    }

    /// The last row of `span`.
    pub fn last_of(&mut self, span: Span) -> Result<Pos> {
        Ok(Pos {
            off: self.last_start(span)?,
            ..Pos::of(&span)
        })
    }

    /// The start of the row `p` is in: `p` itself when it is a row start (a place kept across a
    /// change of width, or of the index, may not be).
    pub fn snap(&mut self, p: Pos) -> Result<Pos> {
        if p.off == 0 {
            return Ok(p);
        }
        let span = self.span(p)?;
        if p.off >= span.len || self.ended(span, p.off)? {
            // Past the line's rows, or on the CR before its end: its last row.
            return self.last_of(span);
        }
        let a = self.restart_before(span, p.off + 1)?;
        let starts = self.segment(span, a)?;
        let off = starts.iter().rev().find(|&&s| s <= p.off).copied();
        Ok(Pos {
            off: off.unwrap_or(a),
            ..p
        })
    }

    /// The row after the one at `p`, if there is one.
    pub fn next(&mut self, p: Pos) -> Result<Option<Pos>> {
        let span = self.span(p)?;
        let end = p.off + self.cut(span, p.off)?.bytes as u64;
        if !self.ended(span, end)? {
            return Ok(Some(Pos { off: end, ..p }));
        }
        Ok(self.after(span)?.map(|next| Pos::of(&next)))
    }

    /// The row before the one at `p`, if there is one: wrapped from the restart point before it
    /// (or, at a line's start, the previous line's last), never from further back.
    pub fn prev(&mut self, p: Pos) -> Result<Option<Pos>> {
        let span = self.span(p)?;
        if p.off > 0 {
            let a = self.restart_before(span, p.off)?;
            let starts = self.segment(span, a)?;
            let off = starts.iter().rev().find(|&&s| s < p.off).copied();
            return Ok(Some(Pos {
                off: off.unwrap_or(a),
                ..p
            }));
        }
        match self.before(span)? {
            Some(prev) => self.last_of(prev).map(Some),
            None => Ok(None),
        }
    }

    /// `n` rows on from `p` (fewer at the end of the file).
    pub fn forward(&mut self, mut p: Pos, n: u64) -> Result<Pos> {
        for _ in 0..n {
            match self.next(p)? {
                Some(q) => p = q,
                None => break,
            }
        }
        Ok(p)
    }

    /// `n` rows back from `p` (fewer at the start of the file).
    pub fn back(&mut self, mut p: Pos, n: u64) -> Result<Pos> {
        for _ in 0..n {
            match self.prev(p)? {
                Some(q) => p = q,
                None => break,
            }
        }
        Ok(p)
    }

    /// `n` lines on from the line `p` is in (fewer at the end of the file), at its start.
    pub fn lines_on(&mut self, p: Pos, n: u64) -> Result<Pos> {
        let mut span = self.span(p)?;
        for _ in 0..n {
            match self.after(span)? {
                Some(next) => span = next,
                None => break,
            }
        }
        Ok(Pos::of(&span))
    }

    /// `n` lines back from the line `p` is in (fewer at the start of the file), at its start.
    pub fn lines_back(&mut self, p: Pos, n: u64) -> Result<Pos> {
        let mut span = self.span(p)?;
        for _ in 0..n {
            match self.before(span)? {
                Some(prev) => span = prev,
                None => break,
            }
        }
        Ok(Pos::of(&span))
    }

    /// Not wrapped: the byte of `span` a row starts at for the line's end to be at its right
    /// edge. 0 for a line that fits, as it paints from its first byte. Else as many of
    /// its last characters as are painted in `columns` columns and in the row's width, a tab
    /// counted as wide as it can be (where it stops depends on where the row starts).
    pub fn end_left(&mut self, span: Span, columns: usize) -> Result<u64> {
        let enc = self.doc.enc;
        let unit = enc.unit();
        let n = span.len.min(self.read as u64);
        let from = span.len - n;
        // From a character's start (rule 4); a place none is proven at shows no character.
        let Some(lead) = self.start_at(span, from)? else {
            return Ok(from);
        };
        let lead = (lead - from).min(n) as usize;
        let window = &mut self.memo.window;
        let mut bytes = self.doc.span_bytes(span, from, n as usize, window, true)?;
        let mut end = span.len;
        if span.cr && enc.ends_with_cr(&bytes, from) {
            bytes.truncate(bytes.len() - unit);
            end -= unit as u64;
        }
        let (cells, _) = enc.decode(&bytes[lead.min(bytes.len())..], true);
        let faces: Vec<char> = cells.iter().map(|c| face(c.ch)).collect();
        let widths = self.m.estimate(&faces);
        if from == 0 {
            // The whole line is here: as it paints from its first byte, each tab to its
            // stop. A line that fits is shown from its start.
            let (mut cols, mut wide) = (0usize, 0f32);
            for (cell, width) in cells.iter().zip(&widths) {
                let (c, w) = match cell.ch {
                    '\t' => (TAB - cols % TAB, (TAB - cols % TAB) as f32 * self.m.space),
                    _ => (1, *width),
                };
                (cols, wide) = (cols + c, wide + w);
            }
            if cols <= columns && wide <= self.width + 0.01 {
                return Ok(0);
            }
        }
        let (mut left, mut cols, mut wide) = (end, 0usize, 0f32);
        for (cell, width) in cells.iter().zip(widths).rev() {
            let (c, w) = match cell.ch {
                '\t' => (TAB, TAB as f32 * self.m.space),
                _ => (1, width),
            };
            if cols + c > columns || wide + w > self.width + 0.01 {
                break;
            }
            (cols, wide) = (cols + c, wide + w);
            left -= u64::from(cell.len);
        }
        // (All of a line that fits was taken: 0.)
        Ok(left)
    }

    /// The top of the last page of `rows` rows: the file's last row at the bottom — `known`:
    /// the last row known, while the index pass is on its way.
    pub fn end(&mut self, rows: usize, known: bool) -> Result<Pos> {
        let Some(last) = self.doc.last_span(known, &mut self.memo.window)? else {
            return Ok(Pos::default());
        };
        remember(&mut self.memo.spans, last.start, last);
        let last = self.last_of(last)?;
        self.back(last, rows.saturating_sub(1) as u64)
    }

    /// The top of the last page of `rows` lines, one row each: the file's last line at the
    /// bottom (`known` as [`Wrapper::end`]'s).
    pub fn end_lines(&mut self, rows: usize, known: bool) -> Result<Pos> {
        let Some(last) = self.doc.last_span(known, &mut self.memo.window)? else {
            return Ok(Pos::default());
        };
        remember(&mut self.memo.spans, last.start, last);
        self.lines_back(Pos::of(&last), rows.saturating_sub(1) as u64)
    }

    /// The line (or piece) byte `at` of the file is in, and the byte of it that is.
    pub fn holding(&mut self, at: u64) -> Result<Option<Pos>> {
        // A jump's place (08 §7.3 rule 3): rounded down to the unit, in UTF-16 back across a
        // pair's low half. It picks the row; the row's own start is the line's, a grid cut's
        // or a restart's.
        let at = self.doc.jump_place(at, &mut self.memo.window)?;
        let Some(span) = self.doc.span_holding(at, &mut self.memo.window)? else {
            return Ok(None);
        };
        remember(&mut self.memo.spans, span.start, span);
        Ok(Some(Pos {
            off: at.saturating_sub(span.start).min(span.len),
            ..Pos::of(&span)
        }))
    }

    /// The row byte `at` of the file is in.
    pub fn at_byte(&mut self, at: u64) -> Result<Pos> {
        match self.holding(at)? {
            Some(p) => self.snap(p),
            None => Ok(Pos::default()),
        }
    }

    /// Characters of `span` before its byte `off`, for a line short enough to be highlighted
    /// and numbered by the index.
    fn chars_before(&mut self, span: Span, off: u64) -> Result<Option<usize>> {
        if span.len > LINE_MAX as u64 || span.line.is_none() {
            return Ok(None);
        }
        if off == 0 {
            return Ok(Some(0));
        }
        let window = &mut self.memo.window;
        let before = self.doc.span_bytes(span, 0, off as usize, window, true)?;
        Ok(Some(self.doc.enc.decode(&before, true).0.len()))
    }

    /// Up to `rows` rows from `p`, and whether they reach the end of the file.
    pub fn page(&mut self, p: Pos, rows: usize) -> Result<(Vec<Row>, bool)> {
        let mut out: Vec<Row> = Vec::new();
        if self.doc.total() == 0 || self.doc.line_count() == 0 {
            return Ok((out, true));
        }
        let mut p = p;
        loop {
            let span = self.span(p)?;
            let mut chars = self.chars_before(span, p.off)?;
            let mut s = p.off;
            let ended = loop {
                let cut = self.cut(span, s)?;
                out.push(Row {
                    at: span.start,
                    line: span.line,
                    loose: span.loose,
                    start: s,
                    cut: cut.clone(),
                    first_char: chars,
                });
                chars = chars.map(|c| c + cut.taken);
                s += cut.bytes as u64;
                let ended = self.ended(span, s)?;
                if ended || out.len() >= rows || cut.bytes == 0 {
                    break ended;
                }
            };
            if out.len() >= rows {
                let last = ended && self.after(span)?.is_none();
                return Ok((out, last));
            }
            match self.after(span)? {
                Some(next) => p = Pos::of(&next),
                None => return Ok((out, true)),
            }
        }
    }

    /// Up to `rows` lines from the one `p` is in, one row each (not wrapped), each from its
    /// byte `left` on — a row is read under the same bound as a wrapped one, so a line of any
    /// length is shown from any byte of it (06 §7.2) — and whether they reach the end of the
    /// file.
    pub fn flat(&mut self, p: Pos, left: u64, rows: usize) -> Result<(Vec<Row>, bool)> {
        let mut out: Vec<Row> = Vec::new();
        if self.doc.total() == 0 || self.doc.line_count() == 0 {
            return Ok((out, true));
        }
        let enc = self.doc.enc;
        let mut span = self.span(p)?;
        loop {
            let at = left.min(span.len);
            // From a character's start (the place within a line is a byte of it: rule 4); a
            // place none is proven at (CP949, D28) shows placeholder marks from there.
            let proven = self.start_at(span, at)?;
            let start = proven.map_or(at, |s| s.min(span.len));
            let want = span.len.saturating_sub(start).min(self.read as u64) as usize;
            let window = &mut self.memo.window;
            let mut bytes = self.doc.span_bytes(span, start, want, window, true)?;
            let whole = start + want as u64 >= span.len;
            if span.cr && whole && enc.ends_with_cr(&bytes, start) {
                bytes.truncate(bytes.len() - enc.unit());
            }
            let (cells, used, dim) = match proven {
                Some(_) => {
                    let (cells, used) = enc.decode(&bytes, whole);
                    (cells, used, 0)
                }
                None => marked(enc, start, &bytes, whole, None),
            };
            let mut shown = Vec::with_capacity(cells.len());
            let mut dots = Vec::new();
            let mut col = 0usize;
            for cell in &cells {
                if cell.ch == '\t' {
                    let wide = TAB - col % TAB;
                    shown.push((' ', wide as u8));
                    col += wide;
                } else {
                    if faint(cell.ch) {
                        dots.push(shown.len());
                    }
                    shown.push((face(cell.ch), 1));
                    col += 1;
                }
            }
            let first_char = self.chars_before(span, start)?;
            out.push(Row {
                at: span.start,
                line: span.line,
                loose: span.loose,
                start,
                cut: Rc::new(Cut {
                    taken: cells.len(),
                    bytes: used,
                    dim: dim.min(shown.len()),
                    shown,
                    dots,
                }),
                first_char,
            });
            let next = self.after(span)?;
            match next {
                Some(next) if out.len() < rows => span = next,
                Some(_) => return Ok((out, false)),
                None => return Ok((out, true)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTF-8 rows decode as they did (`from_utf8_lossy`, character for character), and a piece
    /// that is not whole leaves a character cut at its end for the next piece.
    #[test]
    fn decoding_matches_lossy_and_leaves_a_cut_character() {
        let decode = |b: &[u8], whole| Encoding::Utf8.decode(b, whole);
        let bytes = b"a\xff\xe2\x82b\xed\xa0\x80\xf0\x9f\x98\x80c";
        let (cells, used) = decode(bytes, true);
        assert_eq!(used, bytes.len());
        let text: String = cells.iter().map(|c| c.ch).collect();
        assert_eq!(text, String::from_utf8_lossy(bytes));
        assert_eq!(
            cells.iter().map(|c| usize::from(c.len)).sum::<usize>(),
            bytes.len()
        );
        // "한" cut after two of its three bytes: left out when more follows, bad when whole.
        let cut = [b'x', 0xed, 0x95];
        assert_eq!(decode(&cut, false).1, 1);
        assert_eq!(decode(&cut, true).0.len(), 2);
    }

    /// Where no character start is proven, the bytes are placeholder marks up to the first
    /// one a rule proves — all of them when none does — and never characters (D28).
    #[test]
    fn unproven_bytes_are_marks_up_to_a_proven_start() {
        let bytes = b"abcdef";
        let (cells, used, marks) = marked(Encoding::Utf8, 10, bytes, true, Some(13));
        assert_eq!((used, marks), (6, 3));
        let text: String = cells.iter().map(|c| c.ch).collect();
        assert_eq!(text, "░░░def");
        // CP949's slot proves nothing (VM-4 fills it): every byte a mark.
        let (cells, used, marks) = marked(Encoding::Cp949, 10, bytes, true, None);
        assert_eq!((used, marks), (6, 6));
        assert!(cells.iter().all(|c| c.ch == MARK && c.len == 1));
    }
}
