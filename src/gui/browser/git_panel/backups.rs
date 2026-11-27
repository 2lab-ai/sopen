//! The Backups list (03 §8.2–§8.3, Cmd/Ctrl+B): the groups under `refs/sopen-backup/` by stamp,
//! newest first, each ref with its kind, short hash and subject; Restore (a group or one item),
//! Show and Delete.
//!
//! The list is drawn in the git panel's body in cells ([`draw`], X4-lists), from the records the
//! integration read (`git::backup::list`, newest first). It has a bar over its groups and refs and
//! no key of its own (03 §9.2): [`BackupsView::run`] takes the git context's rows — ↑ ↓ PgUp PgDn
//! Home End move the bar, Enter and F3 (`Details`) Show, Cmd/Ctrl+Z (`Undo`) restores the group or
//! ref under the bar, F8 (`Remove`) deletes its group, Esc (`Back`) closes the list — and its
//! `[ Restore ]`, `[ Show ]` and `[ Delete ]` buttons do the same. Each of those acts only as the
//! last pass that reached the screen painted the bar's line ([`BackupsView::as_painted`]). What
//! they give back
//! ([`BackupsEnd`]) is the integration's to carry out: a restore through its dialog
//! ([`super::dialogs::ActionDialog::restore`], once the values now are read), a delete through its
//! confirmation (`Action::DeleteBackup`), Show into the graph's revisions with a dim `backup`
//! label until the next refresh.

use super::graph_view::printable;
use super::Scroll;
use crate::git::actions::Action;
use crate::git::backup::Record;
use crate::git::model::{BackupKind, BackupRef};
use crate::gui::browser::keys::{Command, Mods};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
use crate::gui::chrome;
use crate::gui::chrome::git::LOOK;
use eframe::egui::{self, Sense};
use std::sync::Arc;

/// The list's header (03 §8.2: "`git push --mirror` would publish them (the Backups list's
/// header says so)").
pub const HEADER: &str =
    "Backups under refs/sopen-backup/, newest first · ordinary local refs: git push --mirror would publish them";

/// What a key or a button of the list asks the integration for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupsEnd {
    /// Restore group `stamp` — or only the item whose backup ref is `only` (a ref's line) —
    /// through its dialog (03 §8.3): read the values now, build the entries, then
    /// [`super::dialogs::ActionDialog::restore`].
    Restore {
        stamp: String,
        only: Option<BackupRef>,
    },
    /// Show these refs in the graph (03 §8.2): a group's refs but its manifest, or one ref.
    Show(Vec<BackupRef>),
    /// Delete the group under the bar, through its confirmation (`ActionDialog::confirm`).
    Delete(Action),
    /// Esc: the list closes.
    Close,
}

/// A line of the list: a group, or one of its refs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Line {
    Group(usize),
    Ref(usize, usize),
}

/// The Backups list, open.
#[derive(Debug, Clone)]
pub struct BackupsView {
    records: Vec<Record>,
    lines: Vec<Line>,
    scroll: Scroll,
    /// The records' revision: one more each time they are read again.
    revision: u64,
    /// What the bar's line asked for as each pass painted it ([`Ends`]), and so as the last pass
    /// that reached the screen did (`cells::OnScreen`: a terminal prints no pass while input
    /// waits): nothing for a pass that painted no bar's line (an empty list, the bar scrolled off
    /// the rows by the wheel, a panel too short for the list).
    screen: cells::OnScreen<Arc<Ends>>,
    /// The pass the screen was last asked about ([`BackupsView::on_screen`]) and its answer.
    asked: Option<(u64, Option<Arc<Ends>>)>,
    /// The bar's line's [`Ends`] as last made, kept while it is of the same line of the same
    /// records: a pass copies no ref.
    ends: Option<Arc<Ends>>,
}

/// What the line under the bar asks for: its Restore, its Show and its Delete.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ends {
    /// The line they are of and the records' revision: a pass that paints the same line of the
    /// same records keeps them, copying no ref again.
    of: (usize, u64),
    restore: Option<BackupsEnd>,
    show: Option<BackupsEnd>,
    delete: Option<BackupsEnd>,
}

/// One of the three things a line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Restore,
    Show,
    Delete,
}

/// A stamp as the list shows it: `20261123T102405Z-2` is `2026-11-23 10:24:05 UTC -2` (the UTC
/// time of Run, 03 §8.2); anything else as it is.
pub fn stamp_text(stamp: &str) -> String {
    let b = stamp.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    if b.len() < 16 || !digits(0..8) || b[8] != b'T' || !digits(9..15) || b[15] != b'Z' {
        return stamp.to_string();
    }
    let s = |r: std::ops::Range<usize>| &stamp[r];
    let mut text = format!(
        "{}-{}-{} {}:{}:{} UTC",
        s(0..4),
        s(4..6),
        s(6..8),
        s(9..11),
        s(11..13),
        s(13..15)
    );
    if let Some(n) = stamp[16..].strip_prefix('-') {
        text.push_str(&format!(" -{n}"));
    }
    text
}

/// A ref's kind as its name under the stamp (03 §8.2's table).
pub fn kind_text(kind: &BackupKind) -> String {
    match kind {
        BackupKind::Head(b) => format!("heads/{b}"),
        BackupKind::Detached => "detached".to_string(),
        BackupKind::Tag(t) => format!("tags/{t}"),
        BackupKind::Stash => "stash".to_string(),
        BackupKind::Worktree => "worktree".to_string(),
        BackupKind::Files => "files".to_string(),
        BackupKind::Pushed(p) => format!("pushed/{p}"),
        BackupKind::Remote(r) => format!("remotes/{r}"),
        BackupKind::Manifest => "manifest".to_string(),
    }
}

fn lines_of(records: &[Record]) -> Vec<Line> {
    let mut lines = Vec::new();
    for (g, r) in records.iter().enumerate() {
        lines.push(Line::Group(g));
        lines.extend((0..r.group.refs.len()).map(|k| Line::Ref(g, k)));
    }
    lines
}

impl BackupsView {
    /// The list of `records`, newest first as `backup::list` gives them, the bar on the first.
    pub fn new(records: Vec<Record>) -> BackupsView {
        BackupsView {
            lines: lines_of(&records),
            records,
            scroll: Scroll::default(),
            revision: 0,
            screen: cells::OnScreen::default(),
            asked: None,
            ends: None,
        }
    }

    /// The list read again: the bar stays on its group, or its ref, when it is still there.
    pub fn set_records(&mut self, records: Vec<Record>) {
        let was = self.line().map(|l| self.key(l));
        self.lines = lines_of(&records);
        self.records = records;
        self.revision += 1;
        let at = was.and_then(|k| self.lines.iter().position(|&l| self.key(l) == k));
        self.scroll
            .to(at.unwrap_or(0).min(self.lines.len().saturating_sub(1)));
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// A line's identity across a re-read: its stamp and its ref's name.
    fn key(&self, line: Line) -> (String, Option<String>) {
        match line {
            Line::Group(g) => (self.records[g].group.stamp.clone(), None),
            Line::Ref(g, k) => (
                self.records[g].group.stamp.clone(),
                Some(self.records[g].group.refs[k].refname.clone()),
            ),
        }
    }

    fn line(&self) -> Option<Line> {
        self.lines.get(self.scroll.bar).copied()
    }

    /// Restore of the group or the ref under the bar (a manifest's line: its group).
    pub fn restore(&self) -> Option<BackupsEnd> {
        let (g, only) = match self.line()? {
            Line::Group(g) => (g, None),
            Line::Ref(g, k) => {
                let r = &self.records[g].group.refs[k];
                (g, (r.kind != BackupKind::Manifest).then(|| r.clone()))
            }
        };
        Some(BackupsEnd::Restore {
            stamp: self.records[g].group.stamp.clone(),
            only,
        })
    }

    /// Show of the group's refs (but its manifest) or of the ref under the bar.
    pub fn show(&self) -> Option<BackupsEnd> {
        let refs = match self.line()? {
            Line::Group(g) => self.records[g]
                .group
                .refs
                .iter()
                .filter(|r| r.kind != BackupKind::Manifest)
                .cloned()
                .collect(),
            Line::Ref(g, k) => vec![self.records[g].group.refs[k].clone()],
        };
        Some(BackupsEnd::Show(refs))
    }

    /// Delete of the group under the bar (or of the ref's group): `Action::DeleteBackup`.
    pub fn delete(&self) -> Option<BackupsEnd> {
        let (Line::Group(g) | Line::Ref(g, _)) = self.line()?;
        Some(BackupsEnd::Delete(Action::DeleteBackup {
            stamp: self.records[g].group.stamp.clone(),
        }))
    }

    /// A row of the git context's key table while the list is open (03 §9.2), in a pass of `ctx`:
    /// Show, Restore and Delete as the last pass that reached the screen painted the bar's line
    /// ([`BackupsView::as_painted`]); the bar's moves and Esc always.
    pub fn run(&mut self, ctx: &egui::Context, command: Command) -> Option<BackupsEnd> {
        match command {
            Command::Bar(step) => {
                let delta = self.scroll.delta(step);
                self.scroll.step(delta, self.lines.len());
                None
            }
            Command::Details => self.as_painted(ctx, Act::Show),
            Command::Undo => self.as_painted(ctx, Act::Restore),
            Command::Remove => self.as_painted(ctx, Act::Delete),
            Command::Back => Some(BackupsEnd::Close),
            _ => None,
        }
    }

    /// What the bar's line asks for now; it is line `of.0` of revision `of.1`.
    fn ends(&self, of: (usize, u64)) -> Ends {
        Ends {
            of,
            restore: self.restore(),
            show: self.show(),
            delete: self.delete(),
        }
    }

    /// What the screen shows of the bar's line in this pass of `ctx`: what the last pass that
    /// reached it painted (`cells::OnScreen::start`, taken once a pass — the keys and the draw ask
    /// in turn). A pass that did not draw the list at all (not shown, or in a panel too short for
    /// it) left no bar on screen: after one, nothing until a pass has painted again.
    fn on_screen(&mut self, ctx: &egui::Context) -> Option<Arc<Ends>> {
        let now = ctx.cumulative_pass_nr();
        match &self.asked {
            Some((pass, shown)) if *pass == now => return shown.clone(),
            Some((pass, _)) if *pass + 1 == now => {}
            _ => self.screen = cells::OnScreen::default(),
        }
        let shown = self.screen.start(ctx).cloned();
        self.asked = Some((now, shown.clone()));
        shown
    }

    /// `act` on the line under the bar, in a pass of `ctx`, only if it asks for exactly what it
    /// asked for as the last pass that reached the screen painted it — what the owner saw when
    /// they pressed. Else nothing, and the next pass shows the list as it is: a re-read that
    /// landed since (a delete's, a restore's), the bar moved since (`[↓, Cmd/Ctrl+Z]` in one pass,
    /// or in one read of a terminal, restores nothing — unless the line it lands on asks for the
    /// same thing: F8 on a group's ref deletes its group, as on the group's line), a pass on
    /// screen that painted no bar's line (the list's first, the bar off the rows, a panel too
    /// short for the list). A re-read that changed nothing under the bar keeps the press.
    fn as_painted(&mut self, ctx: &egui::Context, act: Act) -> Option<BackupsEnd> {
        let painted = self.on_screen(ctx)?;
        let (now, then) = match act {
            Act::Restore => (self.restore(), &painted.restore),
            Act::Show => (self.show(), &painted.show),
            Act::Delete => (self.delete(), &painted.delete),
        };
        now.filter(|end| then.as_ref() == Some(end))
    }
}

/// The hint under the list: what its keys do.
fn hint() -> String {
    format!(
        "Enter Show · {}Z Restore · F8 Delete · Esc closes",
        Mods::Command.prefix()
    )
}

/// The keys' hint in `room` cells: as many of its keys as fit, whole and in order — none rather
/// than one cut short (`En…` on an 80-column terminal's side).
fn hint_in(room: usize) -> String {
    let mut shown = String::new();
    for key in hint().split(" · ") {
        let more = match shown.is_empty() {
            true => key.to_string(),
            false => format!("{shown} · {key}"),
        };
        if cells::width(&more) > room {
            break;
        }
        shown = more;
    }
    shown
}

/// Where a click on a blank of a list at cells `area` of `grid` goes (an `interact` rect under
/// its rows and buttons): its cells, the frame's border cell either side (the panel's own) and
/// egui's reach below them (`interact_radius`, into the frame's rule) — every point a click on
/// which egui would otherwise give to the nearest of the list's widgets.
pub(super) fn blank_clicks(ui: &egui::Ui, grid: &Grid, area: Cells) -> egui::Rect {
    let left = area.x.saturating_sub(1);
    let borders = Cells::new(left, area.y, area.right() + 1 - left, area.height);
    let reach = ui.style().interaction.interact_radius;
    let rect = grid.rect(borders);
    egui::Rect::from_min_max(rect.min, rect.max + egui::vec2(0.0, reach))
}

/// The foot's buttons, in their order, each with what it asks for.
const BUTTONS: [(&str, Act); 3] = [
    ("Restore", Act::Restore),
    ("Show", Act::Show),
    ("Delete", Act::Delete),
];

/// Where a ref's kind starts, two cells in under its group's stamp.
const KIND: usize = 2;
/// Where a group's reflog message and a ref's short hash start: a blank after the widest stamp,
/// `2026-11-23 10:24:05 UTC -NN`, 27 cells.
const HASH: usize = 28;
/// Where a ref's subject starts: a blank after its short hash.
const SUBJECT: usize = 38;

/// Whether [`draw`] paints the list's cells (10 §17, X4-graph's seam): it does (X4-lists), so the
/// panel leaves none of them to egui.
pub(super) const PAINTS_CELLS: bool = true;

/// The Backups list at cells `area` of `grid` (the git panel's body), written into `buf`, the
/// panel's buffer (10 §4, X4-lists): the header dim in the heads' row; the lines — a group's
/// stamp and reflog message, under it each ref two cells in, its kind dim, its short hash and
/// subject — each column cut by cells a blank before the next, the bar on its line, and the scroll
/// bar on the border right of `area`; at the foot `[ Restore ]`, `[ Show ]` and `[ Delete ]` two
/// blank cells apart in the dialog's colours (10 §6's buttons; never focused: the list's keys are
/// the git context's), then as many of the keys' hint's keys, whole and in order, as fit, dim
/// ([`hint_in`]). Only the lines on screen are drawn; the wheel scrolls them by `CELL.y`, the bar
/// stays. Every control is an `interact` rect at its cells, its id salted with the panel's column
/// (two repositories' panels draw into one `ui`), sensing clicks alone (`Sense::CLICK`: egui's
/// `Sense::click()` takes the focus, so Tab, then Space, would press a button), over one more under
/// them all ([`blank_clicks`]: the list's cells, the frame's border either side, egui's reach below),
/// so a click on a blank presses nothing. A click puts the bar on a line, a double click on one line
/// Shows it (the browser's `second_click`, the pair the line itself), a button asks for its
/// action — each of those as the last pass that reached the screen painted the bar's line
/// ([`BackupsView::as_painted`]), what this pass paints kept for the passes after it
/// (`cells::OnScreen`). A list of one or two rows has no foot.
pub fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    area: Cells,
    buf: &mut Buffer,
    view: &mut BackupsView,
    active: bool,
) -> Option<BackupsEnd> {
    let ctx = ui.ctx().clone();
    let p = chrome::PALETTE;
    let width = usize::from(area.width);
    // Cell `c` of the area's rows, on the grid (every `c` here is below `width`).
    let x = |c: usize| area.x.saturating_add(u16::try_from(c).unwrap_or(u16::MAX));
    let dim = Pen::new(LOOK.dim, p.back);
    cells::fill(buf, area, Pen::new(LOOK.text, p.back));
    // The header, the hint and each column over blanks in their own colours: an ambiguous-wide
    // terminal lays the `·` of sopen's own text in two cells by taking a blank that ends its run's
    // word (`term::lay_run`), and cuts a run that has none with `~`.
    if area.height > 0 {
        let header = cells::fit_start(HEADER, width);
        cells::fill(buf, Cells::new(area.x, area.y, area.width, 1), dim);
        cells::put(buf, area.x, area.y, &header, width, dim);
    }
    let foot = (area.height >= 3).then(|| area.bottom() - 1);
    let list = Cells::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1 + u16::from(foot.is_some())),
    );
    let len = view.lines.len();
    if len == 0 && list.height > 0 {
        cells::put(buf, list.x, list.y, "no backups", width, dim);
    }
    let visible = usize::from(list.height).max(1);
    if super::super::pointer_on_pane(ui, grid.rect(list)) {
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
        view.scroll.visible = visible;
        view.scroll.wheel(lines, len);
    }
    view.scroll.settle(visible, len);
    let range = view.scroll.range(len);
    let shown = range.start..range.end.min(range.start + usize::from(list.height));
    let id = |part: &str, k: usize| ui.scope_id().with(("sopen-git-backups", area.x, part, k));
    // Whether this pass paints the bar's line (what it asks for made again only where the last
    // made is not of the same line of the same records).
    let mut bar_drawn = false;
    // What the screen shows (asked before anything of this pass is painted).
    let _ = view.on_screen(&ctx);
    let mut clicked = None;
    // Under the lines and the buttons, the list's cells, the frame's border either side and egui's
    // reach below them ([`blank_clicks`]): a click on a blank of them — between two buttons, beside
    // one, on the border, on a row with no line, just under the foot — presses nothing.
    ui.interact(blank_clicks(ui, grid, area), id("under", 0), Sense::CLICK);
    for (n, k) in shown.enumerate() {
        let y = list.y + n as u16;
        let row = Cells::new(list.x, y, list.width, 1);
        let line = view.lines[k];
        let on_bar = k == view.scroll.bar;
        if on_bar {
            let of = (k, view.revision);
            bar_drawn = true;
            if !matches!(&view.ends, Some(e) if e.of == of) {
                view.ends = Some(Arc::new(view.ends(of)));
            }
        }
        let back = match (on_bar, active) {
            (false, _) => p.back,
            (true, true) => p.bar,
            (true, false) => p.bar_idle,
        };
        let fg = |colour| {
            if on_bar && active {
                LOOK.on_bar
            } else {
                colour
            }
        };
        let ink = |colour| Pen::new(fg(colour), back);
        cells::fill(buf, row, ink(LOOK.text));
        // A column from cell `from` to `to` (the next one's start less a blank, or the row's end).
        let mut column = |from: usize, to: usize, text: &str, colour| {
            let to = to.min(width);
            if from < to {
                let shown = cells::fit_start(&printable(text), to - from);
                let room = Cells::new(x(from), y, (to - from) as u16, 1);
                cells::fill(buf, room, ink(colour));
                cells::put(buf, x(from), y, &shown, to - from, ink(colour));
            }
        };
        match line {
            Line::Group(g) => {
                let group = &view.records[g].group;
                column(0, HASH - 1, &stamp_text(&group.stamp), LOOK.text);
                column(HASH, width, &group.message, LOOK.text);
            }
            Line::Ref(g, r) => {
                let r = &view.records[g].group.refs[r];
                column(KIND, HASH - 1, &kind_text(&r.kind), LOOK.dim);
                column(HASH, SUBJECT - 1, r.oid.short(), LOOK.text);
                column(SUBJECT, width, &r.subject, LOOK.text);
            }
        }
        let resp = ui.interact(grid.rect(row), id("line", k), Sense::CLICK);
        if resp.clicked() {
            let pair = egui::Id::unique(("sopen-git-backups-click", area.x, view.key(line)));
            clicked = Some((k, super::super::second_click(&resp, pair)));
        }
    }
    // The scroll bar on the border, beside the lines.
    if list.height > 0 {
        let max_top = len.saturating_sub(visible);
        let at = (max_top > 0).then(|| view.scroll.top as f32 / max_top as f32);
        cells::scroll_bar(buf, area.right(), list.y, list.height, at);
    }
    // The foot: the buttons that fit whole, then the keys' hint where all three do.
    let mut pressed = None;
    if let Some(y) = foot {
        let button = Pen::new(p.dialog_text, p.dialog_back);
        let mut c = 0;
        let mut all = true;
        for (k, &(word, act)) in BUTTONS.iter().enumerate() {
            let label = format!("[ {word} ]");
            let w = cells::width(&label);
            if c + w > width {
                all = false;
                break;
            }
            cells::put(buf, x(c), y, &label, w, button);
            let at = grid.rect(Cells::new(x(c), y, w as u16, 1));
            if ui.interact(at, id("button", k), Sense::CLICK).clicked() {
                pressed = Some(act);
            }
            c += w + 2;
        }
        if all && c < width {
            let shown = hint_in(width - c);
            cells::fill(buf, Cells::new(x(c), y, (width - c) as u16, 1), dim);
            cells::put(buf, x(c), y, &shown, width - c, dim);
        }
    }
    // What the clicks ask for, as the last pass on screen painted the bar's line: a click moves
    // the bar first, so a double click Shows only the line the bar was on there.
    let mut asked = None;
    if let Some((k, twice)) = clicked {
        view.scroll.to(k);
        if twice {
            asked = view.as_painted(&ctx, Act::Show);
        }
    }
    if let Some(act) = pressed {
        asked = view.as_painted(&ctx, act);
    }
    if let (true, Some(ends)) = (bar_drawn, &view.ends) {
        view.screen.painted(ends.clone());
    }
    asked
}
