//! The graph (03 §4.2–§4.4): the columns, lanes, nodes and labels of the visible rows only, the
//! bar, the label cursor and the marked rows, and the foot.
//!
//! - **Columns**, in cells (03 §4.2): Graph (2 a lane, at most a third of the panel, the lanes
//!   beyond it cut with `»`), Description (labels, then the subject), Date 16, Author 14, Commit
//!   8. Narrowing drops Author first, then Date, then Commit, so the description keeps
//!   [`MIN_DESCRIPTION`] cells. A history search's flat list has no Graph column.
//! - **Lanes** are `git::graph`'s, drawn for the rows on screen only: each row's node segments,
//!   straight from the node (mid-row, on its lane) to a slot on the row's top or bottom edge, and
//!   the pass-throughs of the visible rows ([`Graph::pass_throughs`], once a pass) as vertical
//!   lines; the uncommitted row's line to HEAD's row dashed and dim (03 §4.4). A lane's x is the
//!   middle of its first cell ([`Geometry`]).
//! - **Labels** are built once when the refs, HEAD or the filters change ([`index`]), in 03 §4.4's
//!   order, a local branch and the remotes having it on the same commit merged (`main↔origin,up`);
//!   a row shows as many as fit, then `+N`.

use super::filters::{short_name, Filters};
use super::{HeadRef, Label, LabelKind, Paging, RowKey, Scroll, Tracking};
use crate::git::graph::{Graph, Node, Row};
use crate::git::model::{Oid, RefKind, RefLabel};
use crate::gui::chrome;
use crate::gui::chrome::git::{self as look, LOOK};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use std::collections::{BTreeSet, HashMap, HashSet};

/// The cells the description keeps however narrow the panel: Author, Date and Commit go first.
pub const MIN_DESCRIPTION: usize = 16;

/// The Date column: `2026-11-20 10:24`.
const DATE: usize = 16;
/// The Author column.
const AUTHOR: usize = 14;
/// The Commit column: the first 8 hex digits.
const COMMIT: usize = 8;

/// Where a pass drew the graph's rows: the Graph column's left edge, a cell's width, the first
/// drawn row's top and index, a row's height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Geometry {
    pub left: f32,
    pub cell: f32,
    pub rows_top: f32,
    pub row_h: f32,
    pub first: usize,
}

impl Geometry {
    /// A lane's x: the middle of the first of its two cells.
    pub fn lane_x(&self, slot: u32) -> f32 {
        self.left + self.cell * (2 * slot) as f32 + self.cell / 2.0
    }

    /// Row `i`'s top edge.
    pub fn y(&self, i: usize) -> f32 {
        self.rows_top + self.row_h * i.saturating_sub(self.first) as f32
    }

    /// Row `i`'s node at `column`: mid-row.
    pub fn node(&self, i: usize, column: u32) -> Pos2 {
        Pos2::new(self.lane_x(column), self.y(i) + self.row_h / 2.0)
    }

    /// `slot` on row `i`'s top edge.
    pub fn top(&self, i: usize, slot: u32) -> Pos2 {
        Pos2::new(self.lane_x(slot), self.y(i))
    }

    /// `slot` on row `i`'s bottom edge.
    pub fn bottom(&self, i: usize, slot: u32) -> Pos2 {
        Pos2::new(self.lane_x(slot), self.y(i) + self.row_h)
    }
}

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
    head: &HeadRef,
    filters: &Filters,
) -> HashMap<Oid, Vec<Label>> {
    let mut by: HashMap<Oid, Vec<&RefLabel>> = HashMap::new();
    for r in refs.iter().filter(|r| filters.shows(r)) {
        let at = r.peeled.as_ref().unwrap_or(&r.object);
        by.entry(at.clone()).or_default().push(r);
    }
    let mut out: HashMap<Oid, Vec<Label>> = HashMap::new();
    if let HeadRef::Detached(at) = head {
        out.entry(at.clone()).or_default().push(Label {
            text: "HEAD".to_string(),
            kind: LabelKind::Head,
            lossy: false,
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
            remotes.retain(|r| match short_name(&r.refname).split_once('/') {
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
            if !with.is_empty() {
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
            });
        }
        labels.extend(remotes.iter().map(|r| Label {
            text: short_name(&r.refname).to_string(),
            kind: LabelKind::Remote,
            lossy: lossy(r),
        }));
        labels.extend(
            refs.iter()
                .filter(|r| r.kind == RefKind::Tag)
                .map(|r| Label {
                    text: short_name(&r.refname).to_string(),
                    kind: LabelKind::Tag,
                    lossy: lossy(r),
                }),
        );
        let list = out.entry(at).or_default();
        list.extend(labels);
        // Stable: the refs' own order within a kind.
        list.sort_by_key(|l| l.kind);
    }
    out
}

/// Whether a ref's name shows with U+FFFD: S2-B's `RefLabel::lossy` once its read model lands
/// (the one line to change then).
fn lossy(_r: &RefLabel) -> bool {
    false
}

/// The labels `row` draws.
pub(super) fn row_labels(row: &Row, index: &HashMap<Oid, Vec<Label>>) -> Vec<Label> {
    match row {
        Row::Uncommitted { .. } => Vec::new(),
        Row::Stash(stash) => vec![Label {
            text: stash.selector.clone(),
            kind: LabelKind::Stash,
            lossy: false,
        }],
        Row::Commit { commit, .. } => index.get(&commit.oid).cloned().unwrap_or_default(),
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
            s.subject.clone(),
            date_text(s.commit_time),
            String::new(),
            s.oid.short().to_string(),
        ),
        Row::Commit { commit, .. } => (
            commit.subject.clone(),
            date_text(commit.author_time),
            commit.author_name.clone(),
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

/// The foot's right end: `main ↑2 ↓1 origin/main` (a zero count is left out).
pub(super) fn tracking_text(t: &Tracking) -> String {
    let mut s = t.branch.clone();
    if t.ahead > 0 {
        s.push_str(&format!(" {}{}", look::AHEAD, t.ahead));
    }
    if t.behind > 0 {
        s.push_str(&format!(" {}{}", look::BEHIND, t.behind));
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
    /// The rows' interaction ids are made from it.
    pub salt: &'static str,
}

/// What a pass of a list did.
pub(super) struct Drawn {
    pub geometry: Option<Geometry>,
    /// A row clicked, and whether twice.
    pub clicked: Option<(usize, bool)>,
}

/// Text cells on a painter: each left-aligned at a cell of a row.
struct Cells<'a> {
    painter: &'a egui::Painter,
    font: &'a FontId,
    /// The cells' left edge and width.
    left: f32,
    cw: f32,
}

impl Cells<'_> {
    /// `text` at cell `x` on the row at `y`.
    fn put(&self, x: usize, y: f32, text: &str, c: Color32) {
        self.painter.text(
            Pos2::new(self.left + self.cw * x as f32, y),
            egui::Align2::LEFT_TOP,
            text,
            self.font.clone(),
            c,
        );
    }
}

/// `list` in `area`, its column heads in `head` (none while Find's field is there), the scroll
/// bar on the right border beside the rows. Only the rows on screen are painted; the wheel
/// scrolls them, the bar stays.
pub(super) fn draw(
    ui: &mut egui::Ui,
    head: Option<Rect>,
    area: Rect,
    scroll: &mut Scroll,
    list: &List,
) -> Drawn {
    let ctx = ui.ctx().clone();
    let font = chrome::mono(ui);
    let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = text_h.ceil();
    let chars = (area.width() / cw).floor().max(0.0) as usize;
    let lanes = list.graph.map_or(0, |g| g.width().max(1));
    let cols = Columns::new(chars, lanes);
    let left = area.left();
    if let Some(head) = head {
        let painter = ui.painter_at(head);
        let grid = Cells {
            painter: &painter,
            font: &font,
            left,
            cw,
        };
        let y = head.top();
        grid.put(cols.desc, y, "Description", LOOK.dim);
        for (at, word) in [
            (cols.date, "Date"),
            (cols.author, "Author"),
            (cols.commit, "Commit"),
        ] {
            if let Some(at) = at {
                grid.put(at, y, word, LOOK.dim);
            }
        }
    }
    let len = list.rows.len();
    let visible = ((area.height() / row_h).floor() as usize).max(1);
    if ui.rect_contains_pointer(area) {
        let lines: f32 = ctx.input(|i| {
            i.events
                .iter()
                .map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, .. } => match unit {
                        egui::MouseWheelUnit::Point => -delta.y / row_h,
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
    let painter = ui.painter_at(area);
    let grid = Cells {
        painter: &painter,
        font: &font,
        left,
        cw,
    };
    let geo = Geometry {
        left,
        cell: cw,
        rows_top: area.top(),
        row_h,
        first: range.start,
    };
    let row_rect = |i: usize| {
        Rect::from_min_size(
            Pos2::new(area.left(), geo.y(i)),
            Vec2::new(area.width(), row_h),
        )
    };
    if range.contains(&scroll.bar) {
        chrome::bar(&painter, row_rect(scroll.bar), list.active);
    }
    if let Some(graph) = list.graph {
        lanes_of(&painter, graph, &range, &geo, &cols, &font);
    }
    let mut clicked = None;
    for (i, row) in list
        .rows
        .iter()
        .enumerate()
        .skip(range.start)
        .take(range.len())
    {
        let y = geo.y(i);
        let on_bar = i == scroll.bar;
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
        let (subject, date, author, commit) = cells(row, list.uncommitted);
        // The description: the labels, then the subject in what is left.
        let desc_left = left + cw * cols.desc as f32;
        let desc_right = desc_left + cw * cols.desc_w as f32;
        let mut x = desc_left;
        let labels = row_labels(row, list.labels);
        let cursor = if on_bar { list.label_cursor } else { 0 };
        for (k, label) in labels.iter().enumerate() {
            let mut colours = label_look(label.kind);
            if cursor == k + 1 {
                colours = colours.inverted();
            }
            let galley = painter.layout_no_wrap(label.text.clone(), font.clone(), colours.text);
            let w = galley.size().x;
            let more = labels.len() - k - 1;
            let reserve = if more > 0 {
                cw * (2 + more.to_string().len()) as f32
            } else {
                0.0
            };
            if x + w + reserve > desc_right {
                let rest = format!("+{}", labels.len() - k);
                if x + cw * rest.len() as f32 <= desc_right {
                    painter.text(
                        Pos2::new(x, y),
                        egui::Align2::LEFT_TOP,
                        rest,
                        font.clone(),
                        LOOK.dim,
                    );
                }
                x = desc_right;
                break;
            }
            painter.rect_filled(
                Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, row_h)),
                0.0,
                colours.back,
            );
            painter.galley(Pos2::new(x, y), galley, colours.text);
            x += w + cw;
        }
        if x < desc_right {
            let shown = chrome::fit_start(&ctx, &subject, &font, desc_right - x);
            painter.text(
                Pos2::new(x, y),
                egui::Align2::LEFT_TOP,
                shown,
                font.clone(),
                colour,
            );
        }
        if let Some(at) = cols.date {
            if !date.is_empty() {
                grid.put(at, y, &date, colour);
            }
        }
        if let Some(at) = cols.author {
            if !author.is_empty() {
                let shown = chrome::fit_start(&ctx, &author, &font, cw * AUTHOR as f32);
                grid.put(at, y, &shown, colour);
            }
        }
        if let Some(at) = cols.commit {
            grid.put(at, y, &commit, colour);
        }
        let resp = ui.interact(
            row_rect(i),
            ui.scope_id().with((list.salt, i)),
            Sense::click(),
        );
        if resp.double_clicked() {
            clicked = Some((i, true));
        } else if resp.clicked() {
            clicked = Some((i, false));
        }
    }
    let max_top = len.saturating_sub(visible);
    let column = Rect::from_min_max(
        Pos2::new(area.right(), area.top()),
        Pos2::new(area.right() + chrome::CELL.x, area.bottom()),
    );
    chrome::scroll_bar(
        &ui.painter_at(column),
        column,
        (max_top > 0).then(|| scroll.top as f32 / max_top as f32),
    );
    Drawn {
        geometry: list.graph.map(|_| geo),
        clicked,
    }
}

/// The lanes of the rows in `range`: the pass-throughs, each row's node segments and its node,
/// clipped to the Graph column; `»` on a row whose lanes run past the cut.
fn lanes_of(
    painter: &egui::Painter,
    graph: &Graph,
    range: &std::ops::Range<usize>,
    geo: &Geometry,
    cols: &Columns,
    font: &FontId,
) {
    let column = Rect::from_min_max(
        Pos2::new(geo.left, geo.rows_top),
        Pos2::new(
            geo.left + geo.cell * (2 * cols.lanes) as f32,
            geo.rows_top + geo.row_h * range.len() as f32,
        ),
    );
    let lanes = painter.with_clip_rect(column.intersect(painter.clip_rect()));
    let line = |from: Pos2, to: Pos2, colour: u32, dashed: bool| {
        if dashed {
            let stroke = Stroke::new(1.0, LOOK.uncommitted);
            lanes.extend(Shape::dashed_line(&[from, to], stroke, 2.0, 2.0));
        } else {
            lanes.line_segment([from, to], Stroke::new(1.0, look::lane(colour)));
        }
    };
    // Rows with something past the cut.
    let mut cut: BTreeSet<usize> = BTreeSet::new();
    for p in graph.pass_throughs(range.clone()) {
        if p.slot >= cols.lanes {
            cut.extend(p.rows.clone());
            continue;
        }
        line(
            geo.top(p.rows.start, p.slot),
            geo.bottom(p.rows.end - 1, p.slot),
            p.colour,
            p.dashed,
        );
    }
    for i in range.clone() {
        let node = graph.row(i);
        for s in graph.segments(i) {
            if s.slot >= cols.lanes || node.column >= cols.lanes {
                cut.insert(i);
            }
            let to = match s.end {
                crate::git::graph::End::Top => geo.top(i, s.slot),
                crate::git::graph::End::Bottom => geo.bottom(i, s.slot),
            };
            line(geo.node(i, node.column), to, s.colour, s.dashed);
        }
        if node.column >= cols.lanes {
            cut.insert(i);
            continue;
        }
        let (glyph, colour) = match node.node {
            Node::Commit => (look::COMMIT, look::lane(node.colour)),
            Node::CheckedOut => (look::CHECKED_OUT, look::lane(node.colour)),
            Node::Stash => (look::STASH, look::lane(node.colour)),
            Node::Uncommitted => (look::UNCOMMITTED, LOOK.uncommitted),
        };
        lanes.text(
            Pos2::new(geo.left + geo.cell * (2 * node.column) as f32, geo.y(i)),
            egui::Align2::LEFT_TOP,
            glyph,
            font.clone(),
            colour,
        );
    }
    if cols.cut {
        let at = geo.left + geo.cell * (2 * cols.lanes) as f32;
        for &i in &cut {
            painter.text(
                Pos2::new(at, geo.y(i)),
                egui::Align2::LEFT_TOP,
                look::CUT,
                font.clone(),
                LOOK.cut,
            );
        }
    }
}
