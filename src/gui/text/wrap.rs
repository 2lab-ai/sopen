//! Word wrap for the text viewer: where one row of a line ends, from the row's first byte — by
//! the width egui lays the row out at (Hangul, CJK and emoji are wider than a cell; Arabic and
//! Hebrew are shaped right to left, an Indic syllable as one), only between grapheme clusters (a
//! letter and its marks, a syllable, an emoji sequence stay whole), after a space when the word
//! that follows fits on the next row, else where the row is full. A few layouts per row, however
//! the script is shaped: what fits is found by galloping and halving from an estimate.
//!
//! A row's end is a function of its first byte alone (and the width and fonts), and a line is
//! wrapped afresh from every [`RESTART`] bytes (the next word start, else cluster start): the rows of a
//! line are the same whether they are reached going down or going back up, and going back up
//! reads only from the restart point before the row — never the whole of a long line.

use super::{chrome, Span, Stale, TextDoc, Window_, LINE_MAX, TAB};
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

/// A character of a line as decoded: what it is (U+FFFD for bytes that are not UTF-8) and the
/// bytes it takes in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cell {
    pub ch: char,
    pub len: u8,
}

/// Decodes `bytes` — a piece of a line that starts at a character — as `from_utf8_lossy` does
/// (each maximal bad sequence one U+FFFD). `whole`: the piece runs to the end of what it is cut
/// from (the line, or a restart point), so a character cut in two at its end is bad bytes;
/// otherwise such a character is left for the next piece. Returns the characters and the bytes
/// they cover.
pub(super) fn decode(bytes: &[u8], whole: bool) -> (Vec<Cell>, usize) {
    let end = if whole {
        bytes.len()
    } else {
        super::cut_char_start(bytes)
    };
    let mut out = Vec::with_capacity(end);
    let mut rest = &bytes[..end];
    while !rest.is_empty() {
        let (valid, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => {
                let ok = e.valid_up_to();
                // Valid up to there, by the error.
                let s = std::str::from_utf8(&rest[..ok]).unwrap_or_default();
                (s, e.error_len().unwrap_or(rest.len() - ok))
            }
        };
        out.extend(valid.chars().map(|ch| Cell {
            ch,
            len: ch.len_utf8() as u8,
        }));
        rest = &rest[valid.len()..];
        if bad > 0 {
            out.push(Cell {
                ch: char::REPLACEMENT_CHARACTER,
                len: bad as u8,
            });
            rest = &rest[bad..];
        }
    }
    (out, end)
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

/// A row's candidates: the grapheme clusters of what is read from the row's first byte, each as
/// painted (a tab as spaces to the next stop counted from the row's start, by the font's glyph
/// widths; a control character as its code page 437 glyph).
struct Clusters {
    /// The text as painted.
    text: String,
    /// After each cluster: where it ends in `text`, its characters so far, the estimated width
    /// so far (the font's glyph widths summed — shaping can differ: only to choose what to lay
    /// out), and whether it is a space or a tab (a row breaks after one).
    ends: Vec<usize>,
    chars: Vec<usize>,
    guess: Vec<f32>,
    space: Vec<bool>,
    /// Each character as painted, with how many times (a tab: spaces).
    shown: Vec<(char, u8)>,
}

impl Clusters {
    fn new(cells: &[Cell], m: &Measure) -> Self {
        use unicode_segmentation::UnicodeSegmentation;
        // Tabs stay tabs here (a cluster of their own), widened below.
        let face: String = cells
            .iter()
            .map(|c| {
                if c.ch == '\t' {
                    '\t'
                } else {
                    chrome::shown_char(c.ch)
                }
            })
            .collect();
        let face_chars: Vec<char> = face.chars().collect();
        let widths = m.estimate(&face_chars);
        let mut out = Clusters {
            text: String::with_capacity(face.len()),
            ends: Vec::new(),
            chars: Vec::new(),
            guess: Vec::new(),
            space: Vec::new(),
            shown: Vec::with_capacity(face_chars.len()),
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
            out.space.push(g == " " || g == "\t");
        }
        out
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
        } else if !c.space[k - 1] {
            // In a word: back to after the last space, if the word fits on the next row (laid
            // out on its own) and the row up to that space fits.
            if let Some(j) = (0..k).rev().find(|&j| c.space[j]) {
                let word_end = (k..n).find(|&e| c.space[e]);
                let word_end = match word_end {
                    Some(e) => Some(e),
                    None if !more && complete => Some(n),
                    // It runs past what was laid out (longer than a row), or past what was read.
                    None => None,
                };
                let fits_next = word_end.is_some_and(|e| {
                    m.layout(c.part(j + 1, e).to_string()).size().x <= width + 0.01
                });
                if fits_next && wide(j + 1) <= width + 0.01 {
                    taken = j + 1;
                    shown = j + 1;
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
    }
}

/// A place in the file: a line, and the byte of it a row starts at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) struct Pos {
    pub line: u64,
    pub off: u64,
}

/// A row on screen: its line, its bytes of it, what it shows, and — for a line short enough to
/// be highlighted — the index of its first character in the line (colours are per character).
#[derive(Debug, Clone)]
pub(super) struct Row {
    pub line: u64,
    pub start: u64,
    pub cut: Rc<Cut>,
    pub first_char: Option<usize>,
}

/// Entries of each kind [`Memo`] keeps before it forgets them all (memory stays bounded).
const MEMO_MAX: usize = 4096;

/// What wrapping found so far, for one index and one width: line places, restart points, rows
/// and each restart segment's row starts. Forgotten when the width or the index changes.
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

/// Rows of the file at one width: reads through the index (bounded, [`Stale`] when the file is
/// not what was indexed), measures with [`Measure`], remembers in [`Memo`].
pub(super) struct Wrapper<'a> {
    pub doc: &'a mut TextDoc,
    pub memo: &'a mut Memo,
    pub m: Measure<'a>,
    pub width: f32,
    /// The most bytes one row is read from.
    pub read: usize,
}

impl Wrapper<'_> {
    /// Where line `line` is (read a few lines around it at a time).
    pub fn span(&mut self, line: u64) -> Result<Span> {
        if let Some(span) = self.memo.spans.get(&line) {
            return Ok(*span);
        }
        let first = line.saturating_sub(15);
        let found = self.doc.spans(first, 32)?;
        // Taken from what was read, not the memo: remembering may have forgotten it already.
        let wanted = found.iter().find(|s| s.line == line).copied();
        for span in found {
            remember(&mut self.memo.spans, span.line, span);
        }
        // The index said the line is there.
        wanted.ok_or_else(|| anyhow::Error::new(Stale))
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
        if let Some(r) = self.memo.restarts.get(&(span.line, k)) {
            return Ok(*r);
        }
        let from = at - RESTART_BEHIND;
        let n = (RESTART_BEHIND + RESTART_AHEAD) as usize;
        let bytes = self
            .doc
            .span_bytes(span, from, n, &mut Window_::default(), false)?;
        // From the first character start in what was read.
        let lead = bytes
            .iter()
            .take(3)
            .take_while(|&&b| b & 0xC0 == 0x80)
            .count();
        let whole = from + bytes.len() as u64 == span.len;
        let (cells, _) = decode(&bytes[lead..], whole);
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
            None => {
                let skip = bytes[(at - from) as usize..]
                    .iter()
                    .take(3)
                    .take_while(|&&b| b & 0xC0 == 0x80);
                at + skip.count() as u64
            }
        };
        let r = r.min(span.len);
        remember(&mut self.memo.restarts, (span.line, k), r);
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

    /// The row of `span` that starts at byte `s`.
    fn cut(&mut self, span: Span, s: u64) -> Result<Rc<Cut>> {
        if let Some(cut) = self.memo.rows.get(&(span.line, s)) {
            return Ok(cut.clone());
        }
        let limit = self.restart_after(span, s)?.min(span.len);
        let want = limit.saturating_sub(s).min(self.read as u64) as usize;
        let bytes = self
            .doc
            .span_bytes(span, s, want, &mut self.memo.window, true)?;
        let complete = s + want as u64 == limit;
        let (cells, _) = decode(&bytes, complete);
        let cut = Rc::new(cut_row(&cells, complete, self.width, &self.m));
        remember(&mut self.memo.rows, (span.line, s), cut.clone());
        Ok(cut)
    }

    /// The row starts of `span` from restart point `a` to the next one (or the line's end).
    fn segment(&mut self, span: Span, a: u64) -> Result<Rc<Vec<u64>>> {
        if let Some(starts) = self.memo.segments.get(&(span.line, a)) {
            return Ok(starts.clone());
        }
        let end = self.restart_after(span, a)?.min(span.len);
        let (mut starts, mut s) = (vec![a], a);
        loop {
            let e = s + self.cut(span, s)?.bytes as u64;
            if e >= end || e == s {
                break;
            }
            starts.push(e);
            s = e;
        }
        let starts = Rc::new(starts);
        remember(&mut self.memo.segments, (span.line, a), starts.clone());
        Ok(starts)
    }

    /// Where the last row of `span` starts.
    fn last_start(&mut self, span: Span) -> Result<u64> {
        if span.len == 0 {
            return Ok(0);
        }
        let a = self.restart_before(span, span.len)?;
        Ok(*self.segment(span, a)?.last().unwrap_or(&a))
    }

    /// The last row of line `line`.
    pub fn last_of(&mut self, line: u64) -> Result<Pos> {
        let span = self.span(line)?;
        Ok(Pos {
            line,
            off: self.last_start(span)?,
        })
    }

    /// The start of the row `p` is in: `p` itself when it is a row start (a place kept across a
    /// change of width, or of the index, may not be).
    pub fn snap(&mut self, p: Pos) -> Result<Pos> {
        if p.off == 0 {
            return Ok(p);
        }
        let span = self.span(p.line)?;
        if p.off >= span.len {
            return self.last_of(p.line);
        }
        let a = self.restart_before(span, p.off + 1)?;
        let starts = self.segment(span, a)?;
        let off = starts.iter().rev().find(|&&s| s <= p.off).copied();
        Ok(Pos {
            line: p.line,
            off: off.unwrap_or(a),
        })
    }

    /// The row after the one at `p`, if there is one.
    pub fn next(&mut self, p: Pos) -> Result<Option<Pos>> {
        let span = self.span(p.line)?;
        let end = p.off + self.cut(span, p.off)?.bytes as u64;
        if end < span.len {
            Ok(Some(Pos {
                line: p.line,
                off: end,
            }))
        } else if p.line + 1 < self.doc.line_count() {
            Ok(Some(Pos {
                line: p.line + 1,
                off: 0,
            }))
        } else {
            Ok(None)
        }
    }

    /// The row before the one at `p`, if there is one: wrapped from the restart point before it
    /// (or, at a line's start, the previous line's last), never from further back.
    pub fn prev(&mut self, p: Pos) -> Result<Option<Pos>> {
        if p.off > 0 {
            let span = self.span(p.line)?;
            let a = self.restart_before(span, p.off)?;
            let starts = self.segment(span, a)?;
            let off = starts.iter().rev().find(|&&s| s < p.off).copied();
            return Ok(Some(Pos {
                line: p.line,
                off: off.unwrap_or(a),
            }));
        }
        if p.line == 0 {
            return Ok(None);
        }
        self.last_of(p.line - 1).map(Some)
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

    /// The top of the last page of `rows` rows: the file's last row at the bottom.
    pub fn end(&mut self, rows: usize) -> Result<Pos> {
        let lines = self.doc.line_count();
        if lines == 0 {
            return Ok(Pos::default());
        }
        let last = self.last_of(lines - 1)?;
        self.back(last, rows.saturating_sub(1) as u64)
    }

    /// The row byte `at` of the file is in.
    pub fn at_byte(&mut self, at: u64) -> Result<Pos> {
        let Some(span) = self.doc.span_at(at)? else {
            return Ok(Pos::default());
        };
        remember(&mut self.memo.spans, span.line, span);
        self.snap(Pos {
            line: span.line,
            off: at.saturating_sub(span.start).min(span.len),
        })
    }

    /// Up to `rows` rows from `p`, and whether they reach the end of the file.
    pub fn page(&mut self, p: Pos, rows: usize) -> Result<(Vec<Row>, bool)> {
        let lines = self.doc.line_count();
        let mut out: Vec<Row> = Vec::new();
        if lines == 0 {
            return Ok((out, true));
        }
        let mut p = p;
        loop {
            let span = self.span(p.line)?;
            // Characters before the first row, for a line short enough to be highlighted.
            let mut chars = if span.len > LINE_MAX as u64 {
                None
            } else if p.off == 0 {
                Some(0)
            } else {
                let window = &mut self.memo.window;
                let before = self.doc.span_bytes(span, 0, p.off as usize, window, true)?;
                Some(decode(&before, true).0.len())
            };
            let mut s = p.off;
            loop {
                let cut = self.cut(span, s)?;
                out.push(Row {
                    line: span.line,
                    start: s,
                    cut: cut.clone(),
                    first_char: chars,
                });
                chars = chars.map(|c| c + cut.taken);
                s += cut.bytes as u64;
                if s >= span.len || out.len() >= rows || cut.bytes == 0 {
                    break;
                }
            }
            let last_line = span.line + 1 >= lines;
            if out.len() >= rows {
                return Ok((out, last_line && s >= span.len));
            }
            if last_line {
                return Ok((out, true));
            }
            p = Pos {
                line: span.line + 1,
                off: 0,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decoding matches `from_utf8_lossy` character for character, and a piece that is not whole
    /// leaves a character cut at its end for the next piece.
    #[test]
    fn decoding_matches_lossy_and_leaves_a_cut_character() {
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
}
