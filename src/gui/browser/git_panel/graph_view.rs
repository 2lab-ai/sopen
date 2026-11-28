//! The graph (03 §4.2–§4.4) in cells (10 §4, X4-graph): the columns, lanes, nodes and labels of
//! the visible rows only, the bar, the label cursor and the marked rows, written into the
//! panel's buffer, so the window and the terminal show the same cells — every width a count of
//! cells (unicode-width), a Hangul subject or author two cells a glyph, a cut never half a glyph.
//!
//! - **Columns**, in cells (03 §4.2): Graph (2 a lane, at most a third of the panel, the lanes
//!   beyond it cut with `»`), Description (labels, then the subject), Date 16, Author 14, Commit
//!   8. Narrowing drops Author first, then Date, then Commit, so the description keeps
//!   [`MIN_DESCRIPTION`] cells. A history search's flat list has no Graph column.
//! - **Lanes** are `git::graph`'s, as glyphs ([`lanes::rows`], X4-lanes), for the rows on screen
//!   only: each row's node segments and the visible rows' pass-throughs.
//! - **Labels** are built once when the refs, HEAD or the filters change ([`index`]), in 03 §4.4's
//!   order, a local branch and the remotes having it on the same commit merged (`main↔origin,up`);
//!   a row shows as many as fit, then `+N`. The label cursor's label has its colours swapped (03
//!   §4.4's "inverted": no modifier, T37).
//! - **An ambiguous-wide terminal** (10 §10, T38): the panel's own `↔` (a label's joiner) and the
//!   foot's `↑ ↓` are §10's stand-ins, at the source, as the nodes and lanes are through
//!   [`lanes::rows`]; labels, subjects and names are content and stay as written.

use super::filters::{short_name, split_remote, Filters};
use super::{lanes, HeadRef, Label, LabelKind, Paging, RowKey, Scroll, Tracking};
use crate::git::graph::{Graph, Row};
use crate::git::model::{Oid, RefKind, RefLabel};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
use crate::gui::chrome;
use crate::gui::chrome::git::{self as look, LOOK};
use crate::gui::term;
use eframe::egui::{self, Pos2, Sense};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// The cells the description keeps however narrow the panel: Author, Date and Commit go first.
pub const MIN_DESCRIPTION: usize = 16;

/// The Date column: `2026-11-20 10:24`.
const DATE: usize = 16;
/// The Author column.
const AUTHOR: usize = 14;
/// The Commit column: the first 8 hex digits.
const COMMIT: usize = 8;

/// The columns of a panel `chars` cells wide whose graph has `lanes` lanes (0: no Graph column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Columns {
    /// The Graph column's cells.
    pub graph: usize,
    /// The lanes drawn: all of them, or those before the `»` cell when they are cut.
    pub lanes: u32,
    pub cut: bool,
    /// Where each column starts, in cells, and the description's width.
    pub desc: usize,
    pub desc_w: usize,
    pub date: Option<usize>,
    pub author: Option<usize>,
    pub commit: Option<usize>,
}

impl Columns {
    pub fn new(chars: usize, lanes: u32) -> Columns {
        let room = (chars / 3) / 2 * 2;
        let want = 2 * lanes as usize;
        let (graph, shown, cut) = if want <= room {
            (want, lanes, false)
        } else {
            // The last lane's two cells hold `»`.
            (room, (room / 2).saturating_sub(1) as u32, true)
        };
        let rest = chars.saturating_sub(graph);
        let fits = |w: usize| rest >= w + MIN_DESCRIPTION;
        let (date, author, commit) = if fits(3 + DATE + AUTHOR + COMMIT) {
            (true, true, true)
        } else if fits(2 + DATE + COMMIT) {
            (true, false, true)
        } else if fits(1 + COMMIT) {
            (false, false, true)
        } else {
            (false, false, false)
        };
        let right = [(date, DATE), (author, AUTHOR), (commit, COMMIT)]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, w)| w + 1)
            .sum::<usize>();
        let desc_w = rest.saturating_sub(right);
        let mut x = graph + desc_w;
        let mut next = |on: bool, w: usize| {
            on.then(|| {
                let at = x + 1;
                x = at + w;
                at
            })
        };
        let date = next(date, DATE);
        let author = next(author, AUTHOR);
        let commit = next(commit, COMMIT);
        Columns {
            graph,
            lanes: shown,
            cut,
            desc: graph,
            desc_w,
            date,
            author,
            commit,
        }
    }
}

/// The labels of every commit with a ref the filters show, by the commit, each list in 03 §4.4's
/// order: a detached `HEAD`, the checked-out branch (`►`), other local branches — each with the
/// remotes that have a branch of its name on the same commit (`main↔origin,up`) — remote
/// branches, tags. An annotated tag's label is on what it points at; a remote's `HEAD` is never
/// drawn.
pub(super) fn index(
    refs: &[RefLabel],
    remote_names: &[String],
    head: &HeadRef,
    filters: &Filters,
) -> HashMap<Oid, Vec<Label>> {
    let mut by: HashMap<Oid, Vec<&RefLabel>> = HashMap::new();
    for r in refs.iter().filter(|r| filters.shows(r, remote_names)) {
        let at = r.peeled.as_ref().unwrap_or(&r.object);
        by.entry(at.clone()).or_default().push(r);
    }
    let mut out: HashMap<Oid, Vec<Label>> = HashMap::new();
    if let HeadRef::Detached(at) = head {
        out.entry(at.clone()).or_default().push(Label {
            text: "HEAD".to_string(),
            kind: LabelKind::Head,
            lossy: false,
            joined: None,
        });
    }
    for (at, refs) in by {
        let mut remotes: Vec<&RefLabel> = refs
            .iter()
            .filter(|r| r.kind == RefKind::RemoteBranch)
            .copied()
            .collect();
        let mut labels = Vec::new();
        for local in refs.iter().filter(|r| r.kind == RefKind::LocalBranch) {
            let name = short_name(&local.refname);
            let mut with: Vec<&str> = Vec::new();
            remotes.retain(|r| match split_remote(&r.refname, remote_names) {
                Some((remote, branch)) if branch == name => {
                    with.push(remote);
                    false
                }
                _ => true,
            });
            let checked_out = matches!(head, HeadRef::Branch(b) if *b == local.refname);
            let mut text = String::new();
            if checked_out {
                text.push(look::CURRENT);
            }
            text.push_str(name);
            let mut joined = None;
            if !with.is_empty() {
                joined = Some(text.len());
                text.push(look::BOTH);
                text.push_str(&with.join(","));
            }
            labels.push(Label {
                text,
                kind: if checked_out {
                    LabelKind::CheckedOut
                } else {
                    LabelKind::Local
                },
                lossy: lossy(local),
                joined,
            });
        }
        labels.extend(remotes.iter().map(|r| Label {
            text: short_name(&r.refname).to_string(),
            kind: LabelKind::Remote,
            lossy: lossy(r),
            joined: None,
        }));
        labels.extend(
            refs.iter()
                .filter(|r| r.kind == RefKind::Tag)
                .map(|r| Label {
                    text: short_name(&r.refname).to_string(),
                    kind: LabelKind::Tag,
                    lossy: lossy(r),
                    joined: None,
                }),
        );
        let list = out.entry(at).or_default();
        list.extend(labels);
        // Stable: the refs' own order within a kind.
        list.sort_by_key(|l| l.kind);
    }
    out
}

/// Whether a ref's name shows with U+FFFD (the read model's `RefLabel::lossy`): its label is
/// drawn as it is and names no ref.
fn lossy(r: &RefLabel) -> bool {
    r.lossy
}

/// At most `room` characters of `text`, and one more to show it does not fit: what a label lays
/// out, however long the ref's name.
pub(super) fn capped(text: &str, room: usize) -> Cow<'_, str> {
    match text.char_indices().nth(room + 1) {
        Some((at, _)) => {
            copied(at);
            Cow::Owned(text[..at].to_string())
        }
        None => Cow::Borrowed(text),
    }
}

/// A repository's text as a row shows it: its control characters (`\n`, `\r`, `\t`, any other)
/// escaped, so a name or a subject never runs onto another row.
pub fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_control() {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// The labels `row` draws.
pub(super) fn row_labels<'a>(
    row: &'a Row,
    index: &'a HashMap<Oid, Vec<Label>>,
) -> Cow<'a, [Label]> {
    match row {
        Row::Uncommitted { .. } => Cow::Borrowed(&[]),
        Row::Stash(stash) => Cow::Owned(vec![Label {
            text: stash.selector.clone(),
            kind: LabelKind::Stash,
            lossy: false,
            joined: None,
        }]),
        Row::Commit { commit, .. } => index
            .get(&commit.oid)
            .map_or(Cow::Borrowed(&[][..]), |l| Cow::Borrowed(l.as_slice())),
    }
}

/// A label kind's colours.
fn label_look(kind: LabelKind) -> look::Label {
    match kind {
        LabelKind::Head => LOOK.head,
        LabelKind::CheckedOut | LabelKind::Local => LOOK.local,
        LabelKind::Remote => LOOK.remote,
        LabelKind::Tag => LOOK.tag,
        LabelKind::Stash => LOOK.stash,
    }
}

/// The author date as the Date column shows it: local time, `2026-11-20 10:24`.
pub(super) fn date_text(secs: i64) -> String {
    match chrome::local_tm(secs) {
        Some(tm) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        ),
        None => String::new(),
    }
}

/// A row's subject, date, author and commit cells.
pub(super) fn cells(row: &Row, uncommitted: usize) -> (String, String, String, String) {
    match row {
        Row::Uncommitted { .. } => (
            format!("Uncommitted changes ({uncommitted})"),
            String::new(),
            "*".to_string(),
            "*".to_string(),
        ),
        Row::Stash(s) => (
            printable(&s.subject).into_owned(),
            date_text(s.commit_time),
            String::new(),
            s.oid.short().to_string(),
        ),
        Row::Commit { commit, .. } => (
            printable(&commit.subject).into_owned(),
            date_text(commit.author_time),
            printable(&commit.author_name).into_owned(),
            commit.oid.short().to_string(),
        ),
    }
}

/// What the paging line (the foot) says of `n` rows (`noun`: `commits`, `found`), and whether it
/// is an error.
pub(super) fn paging_text(paging: &Paging, n: usize, noun: &str) -> (String, bool) {
    match paging {
        Paging::Loading if n == 0 => ("loading…".to_string(), false),
        Paging::Loading => (format!("{n} {noun} · loading…"), false),
        Paging::More(m) => (format!("{n} {noun} · {m} more"), false),
        Paging::AllLoaded => (format!("{n} {noun} · all loaded"), false),
        Paging::Shallow => (
            format!("{n} {noun} · history ends here (shallow clone)"),
            false,
        ),
        Paging::NoCommits => ("no commits yet".to_string(), false),
        Paging::NoRefsChosen => ("no refs chosen".to_string(), false),
        Paging::Error(e) => (format!("{n} {noun} · {e}"), true),
    }
}

/// The foot's right end: `main ↑2 ↓1 origin/main` (a zero count is left out); in an
/// ambiguous-wide terminal (`wide`) the arrows are §10's `^ v`, the names as written (T38).
pub(super) fn tracking_text(t: &Tracking, wide: bool) -> String {
    let mut s = t.branch.clone();
    if t.ahead > 0 {
        s.push_str(&format!(" {}{}", own(look::AHEAD, wide), t.ahead));
    }
    if t.behind > 0 {
        s.push_str(&format!(" {}{}", own(look::BEHIND, wide), t.behind));
    }
    s.push(' ');
    s.push_str(&t.upstream);
    s
}

/// A list to draw: the graph's rows with their layout, or a history search's flat list.
pub(super) struct List<'a> {
    pub rows: &'a [Row],
    /// `None`: a flat list, no Graph column.
    pub graph: Option<&'a Graph>,
    pub labels: &'a HashMap<Oid, Vec<Label>>,
    pub marks: &'a HashSet<RowKey>,
    pub uncommitted: usize,
    /// The label cursor on the bar's row (0: the commit itself).
    pub label_cursor: usize,
    /// The bar is the active one: the panel has the keyboard and nothing in it holds it.
    pub active: bool,
    /// The rows' interaction ids and their double clicks' pairs are made from it: the panel's own
    /// key with the list's name (`GitView::find_key`), which panel it is whatever its column or
    /// `ui`, so two panels side by side never share one (F-git-ids).
    pub ids: egui::Id,
}

/// What a pass of a list did.
pub(super) struct Drawn {
    pub geometry: Option<Geometry>,
    /// A row clicked, and whether twice.
    pub clicked: Option<(usize, bool)>,
}

/// Where a pass drew the graph's rows: the grid, the rows' cells inside the frame (from the
/// Graph column's first cell to the description's last, the scroll bar on the border after
/// them), and the first drawn row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Geometry {
    pub grid: Grid,
    pub rows: Cells,
    pub first: usize,
}

impl Geometry {
    /// The drawn row whose cells are under `p`, if one is.
    pub fn row_at(&self, p: Pos2) -> Option<usize> {
        let at = self.grid.at(p)?;
        let k = usize::from(at.y.checked_sub(self.rows.y)?);
        self.rows.contains(at).then_some(self.first + k)
    }

    /// Row `i`'s cells, where it is drawn.
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn row(&self, i: usize) -> Option<Cells> {
        let k = u16::try_from(i.checked_sub(self.first)?).ok()?;
        (k < self.rows.height).then(|| Cells::new(self.rows.x, self.rows.y + k, self.rows.width, 1))
    }
}

/// `glyph` as this pass's screen shows it: in a terminal that draws a glyph of ambiguous width in
/// two cells (`wide`), 10 §10's stand-in (`term::narrow`: `↔` `-`, `↑` `^`, `↓` `v`), put here
/// where the glyph is known to be the panel's own, never a name's (T38).
fn own(glyph: char, wide: bool) -> String {
    let mut utf8 = [0u8; 4];
    let s: &str = glyph.encode_utf8(&mut utf8);
    match term::narrow(s).filter(|_| wide) {
        Some(stand_in) => stand_in.to_string(),
        None => s.to_string(),
    }
}

/// A label as a row shows it in at most `room` cells' worth of characters ([`capped`]), its
/// control characters escaped: in an ambiguous-wide terminal the `↔` that joins a local branch to
/// its remotes is §10's stand-in — the panel's own glyph — while the names stay as written. Only
/// the shown start is built, the joiner substituted there when it is in it, so a pass costs the
/// room, never a copy of the whole name (review round 1).
pub(super) fn label_text(label: &Label, room: usize, wide: bool) -> String {
    // `capped` cuts at a character and the joiner is one: it is whole in `shown` or not in it.
    let shown = capped(&label.text, room);
    let shown = match label.joined.filter(|&at| wide && at < shown.len()) {
        Some(at) => {
            let mut s = shown.into_owned();
            copied(s.len());
            s.replace_range(at..at + look::BOTH.len_utf8(), &own(look::BOTH, wide));
            Cow::Owned(s)
        }
        None => shown,
    };
    let out = printable(&shown).into_owned();
    copied(out.len());
    out
}

#[cfg(test)]
thread_local! {
    /// The bytes [`label_text`] has copied on this thread, [`capped`]'s cut among them: its work,
    /// which the tests bound by the room a label has, however long its name.
    pub(super) static COPIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts `bytes` copied ([`COPIED`], in the tests only).
fn copied(bytes: usize) {
    #[cfg(test)]
    COPIED.with(|c| c.set(c.get() + bytes));
    #[cfg(not(test))]
    let _ = bytes;
}

/// `list` in cells `area` of `buf` (on `grid`): its column heads in `head` (none while Find's
/// field stands there), each row a cell tall — the lanes ([`lanes::rows`]), the labels in their
/// colours (the label cursor's swapped, 03 §4.4) then `+N` where they stop, the subject, Date,
/// Author and Commit, cut by cells — the bar, and the scroll bar on the border right of `area`.
/// Only the rows on screen are drawn; the wheel scrolls them, the bar stays. Each row is an
/// `interact` rect at its cells: a click puts the bar there, a double click on one row is Enter
/// (the browser's `second_click`, as the list's and the tree's: egui alone misses a double click
/// right after a click on another row).
pub(super) fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    buf: &mut Buffer,
    head: Option<Cells>,
    area: Cells,
    scroll: &mut Scroll,
    list: &List,
) -> Drawn {
    let ctx = ui.ctx().clone();
    let p = chrome::PALETTE;
    let wide = cells::ambiguous_wide(&ctx);
    let chars = usize::from(area.width);
    let lanes = list.graph.map_or(0, |g| g.width().max(1));
    let cols = Columns::new(chars, lanes);
    // Cell `c` of the room, on the grid (every `c` here is below `chars`).
    let x = |c: usize| area.x.saturating_add(u16::try_from(c).unwrap_or(u16::MAX));
    if let Some(head) = head {
        let dim = Pen::new(LOOK.dim, p.back);
        cells::put(buf, x(cols.desc), head.y, "Description", cols.desc_w, dim);
        for (at, word, w) in [
            (cols.date, "Date", DATE),
            (cols.author, "Author", AUTHOR),
            (cols.commit, "Commit", COMMIT),
        ] {
            if let Some(at) = at {
                cells::put(buf, x(at), head.y, word, w, dim);
            }
        }
    }
    let len = list.rows.len();
    let visible = usize::from(area.height).max(1);
    if super::super::pointer_on_pane(ui, grid.rect(area)) {
        let lines: f32 = ctx.input(|i| {
            i.events
                .iter()
                .map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => match unit {
                        egui::MouseWheelUnit::Point => -delta.y / chrome::CELL.y,
                        _ => -delta.y * 3.0,
                    },
                    _ => 0.0,
                })
                .sum()
        });
        scroll.visible = visible;
        scroll.wheel(lines, len);
    }
    scroll.settle(visible, len);
    let range = scroll.range(len);
    let shown = range.start..range.end.min(range.start + usize::from(area.height));
    let graph_cells = list
        .graph
        .map(|g| lanes::rows(g, shown.clone(), &cols, wide));
    let desc_right = cols.desc + cols.desc_w;
    let mut clicked = None;
    for (k, (i, row)) in list
        .rows
        .iter()
        .enumerate()
        .skip(shown.start)
        .take(shown.len())
        .enumerate()
    {
        let y = area.y + k as u16;
        let row_cells = Cells::new(area.x, y, area.width, 1);
        let on_bar = i == scroll.bar;
        let back = match (on_bar, list.active) {
            (false, _) => p.back,
            (true, true) => p.bar,
            (true, false) => p.bar_idle,
        };
        let marked = list.marks.contains(&RowKey::of(row));
        let muted = match row {
            Row::Commit { commit, .. } => commit.parents.len() > 1,
            Row::Stash(_) | Row::Uncommitted { .. } => true,
        };
        let colour = if marked {
            LOOK.marked
        } else if on_bar && list.active {
            LOOK.on_bar
        } else if muted {
            LOOK.dim
        } else {
            LOOK.text
        };
        let pen = Pen::new(colour, back);
        cells::fill(buf, row_cells, pen);
        // The lanes, each cell in its own colour on the row's back.
        if let Some(row_lanes) = graph_cells.as_ref().and_then(|g| g.get(k)) {
            for (c, cell) in row_lanes.iter().enumerate() {
                if let Some(cell) = cell {
                    let glyph = cell.glyph.to_string();
                    cells::put(buf, x(c), y, &glyph, 1, Pen::new(cell.colour, back));
                }
            }
        }
        let (subject, date, author, commit) = cells(row, list.uncommitted);
        // The description: the labels, then the subject in what is left.
        let mut at = cols.desc;
        let labels = row_labels(row, list.labels);
        let cursor = if on_bar { list.label_cursor } else { 0 };
        for (n, label) in labels.iter().enumerate() {
            let mut colours = label_look(label.kind);
            if cursor == n + 1 {
                colours = colours.inverted();
            }
            // No more of a name than the description holds is laid out.
            let text = label_text(label, cols.desc_w, wide);
            let w = cells::width(&text);
            let more = labels.len() - n - 1;
            let reserve = if more > 0 {
                2 + more.to_string().len()
            } else {
                0
            };
            if at + w + reserve > desc_right {
                let rest = format!("+{}", labels.len() - n);
                if at + rest.len() <= desc_right {
                    let dim = Pen::new(LOOK.dim, back);
                    cells::put(buf, x(at), y, &rest, rest.len(), dim);
                }
                at = desc_right;
                break;
            }
            cells::put(
                buf,
                x(at),
                y,
                &text,
                w,
                Pen::new(colours.text, colours.back),
            );
            at += w + 1;
        }
        if at < desc_right {
            let room = desc_right - at;
            cells::put(buf, x(at), y, &cells::fit_start(&subject, room), room, pen);
        }
        if let Some(at) = cols.date {
            cells::put(buf, x(at), y, &date, DATE, pen);
        }
        if let Some(at) = cols.author {
            cells::put(
                buf,
                x(at),
                y,
                &cells::fit_start(&author, AUTHOR),
                AUTHOR,
                pen,
            );
        }
        if let Some(at) = cols.commit {
            cells::put(buf, x(at), y, &commit, COMMIT, pen);
        }
        // Clicks alone (`Sense::CLICK`): the keyboard moves the bar by its own keys, and egui's
        // focusable `Sense::click()` would let Tab reach a row and its Space or Enter click it.
        let resp = ui.interact(grid.rect(row_cells), list.ids.with(i), Sense::CLICK);
        if resp.clicked() {
            // The row by what it draws: rows come and go above it between two clicks (03 §5).
            let pair = list.ids.with(("click", RowKey::of(row)));
            clicked = Some((i, super::super::second_click(&resp, pair)));
        }
    }
    // The scroll bar on the border, beside the rows.
    let max_top = len.saturating_sub(visible);
    let at = (max_top > 0).then(|| scroll.top as f32 / max_top as f32);
    if area.height > 0 {
        cells::scroll_bar(buf, area.right(), area.y, area.height, at);
    }
    Drawn {
        geometry: list.graph.map(|_| Geometry {
            grid: *grid,
            rows: area,
            first: range.start,
        }),
        clicked,
    }
}
