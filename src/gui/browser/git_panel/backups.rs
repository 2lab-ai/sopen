//! The Backups list (03 §8.2–§8.3, Cmd/Ctrl+B): the groups under `refs/sopen-backup/` by stamp,
//! newest first, each ref with its kind, short hash and subject; Restore (a group or one item),
//! Show and Delete.
//!
//! The list is drawn in the git panel's body ([`draw`]), from the records the integration read
//! (`git::backup::list`, newest first). It has a bar over its groups and refs and no key of its
//! own (03 §9.2): [`BackupsView::run`] takes the git context's rows — ↑ ↓ PgUp PgDn Home End move
//! the bar, Enter and F3 (`Details`) Show, Cmd/Ctrl+Z (`Undo`) restores the group or ref under the
//! bar, F8 (`Remove`) deletes its group, Esc (`Back`) closes the list — and its Restore, Show and
//! Delete buttons do the same. What they give back ([`BackupsEnd`]) is the integration's to carry
//! out: a restore through its dialog ([`super::dialogs::ActionDialog::restore`], once the values
//! now are read), a delete through its confirmation (`Action::DeleteBackup`), Show into the
//! graph's revisions with a dim `backup` label until the next refresh.

use super::graph_view::printable;
use super::Scroll;
use crate::git::actions::Action;
use crate::git::backup::Record;
use crate::git::model::{BackupKind, BackupRef};
use crate::gui::browser::keys::{Command, Mods};
use crate::gui::chrome;
use crate::gui::chrome::git::LOOK;
use eframe::egui::{self, Pos2, Rect, Sense, Vec2};

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
        }
    }

    /// The list read again: the bar stays on its group, or its ref, when it is still there.
    pub fn set_records(&mut self, records: Vec<Record>) {
        let was = self.line().map(|l| self.key(l));
        self.lines = lines_of(&records);
        self.records = records;
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

    /// A row of the git context's key table while the list is open (03 §9.2).
    pub fn run(&mut self, command: Command) -> Option<BackupsEnd> {
        match command {
            Command::Bar(step) => {
                let delta = self.scroll.delta(step);
                self.scroll.step(delta, self.lines.len());
                None
            }
            Command::Details => self.show(),
            Command::Undo => self.restore(),
            Command::Remove => self.delete(),
            Command::Back => Some(BackupsEnd::Close),
            _ => None,
        }
    }
}

/// The hint under the list: what its keys do.
fn hint() -> String {
    format!(
        "Enter Show · {}Z Restore · F8 Delete · Esc closes",
        Mods::Command.prefix()
    )
}

/// The Backups list in `rect` (the git panel's body): the header, the lines with the bar, the
/// scroll bar, and the Restore, Show and Delete buttons with the keys' hint at the foot. A click
/// puts the bar on a line, a double click Shows it.
pub fn draw(
    ui: &mut egui::Ui,
    rect: Rect,
    view: &mut BackupsView,
    active: bool,
) -> Option<BackupsEnd> {
    let ctx = ui.ctx().clone();
    let font = chrome::mono(ui);
    let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = text_h.ceil();
    let painter = ui.painter_at(rect);
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), row_h));
    let foot = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - row_h), rect.max);
    let list = Rect::from_min_max(
        Pos2::new(rect.left(), header.bottom()),
        Pos2::new(rect.right() - chrome::CELL.x, foot.top()),
    );
    let chars = (list.width() / cw).floor().max(0.0) as usize;
    let fit =
        |s: &str, cells: usize| chrome::fit_start(&ctx, &printable(s), &font, cw * cells as f32);
    painter.text(
        header.left_top(),
        egui::Align2::LEFT_TOP,
        fit(HEADER, chars),
        font.clone(),
        LOOK.dim,
    );
    let mut asked = None;
    if view.lines.is_empty() {
        painter.text(
            list.left_top(),
            egui::Align2::LEFT_TOP,
            "no backups",
            font.clone(),
            LOOK.dim,
        );
    }
    let visible = ((list.height() / row_h).floor() as usize).max(1);
    let len = view.lines.len();
    if ui.rect_contains_pointer(list) {
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
        view.scroll.visible = visible;
        view.scroll.wheel(lines, len);
    }
    view.scroll.settle(visible, len);
    let range = view.scroll.range(len);
    // Columns: the stamp or the kind, the hash, the message or the subject.
    const HASH: usize = 26;
    const SUBJECT: usize = 36;
    for (k, line) in view
        .lines
        .iter()
        .enumerate()
        .skip(range.start)
        .take(range.len())
    {
        let y = list.top() + row_h * (k - range.start) as f32;
        let row = Rect::from_min_size(Pos2::new(list.left(), y), Vec2::new(list.width(), row_h));
        let painter = painter.with_clip_rect(row.intersect(list));
        let on_bar = k == view.scroll.bar;
        if on_bar {
            chrome::bar(&painter, row, active);
        }
        let paint = |c| if on_bar && active { LOOK.on_bar } else { c };
        let text = |cell: usize, s: &str, c| {
            let room = chars.saturating_sub(cell);
            painter.text(
                Pos2::new(list.left() + cw * cell as f32, y),
                egui::Align2::LEFT_TOP,
                fit(s, room),
                font.clone(),
                paint(c),
            );
        };
        match *line {
            Line::Group(g) => {
                let group = &view.records[g].group;
                text(0, &stamp_text(&group.stamp), LOOK.text);
                text(HASH, &group.message, LOOK.text);
            }
            Line::Ref(g, r) => {
                let r = &view.records[g].group.refs[r];
                text(2, &kind_text(&r.kind), LOOK.dim);
                text(HASH, r.oid.short(), LOOK.text);
                text(SUBJECT, &r.subject, LOOK.text);
            }
        }
        let resp = ui.interact(
            row,
            ui.scope_id().with(("sopen-git-backups", k)),
            Sense::click(),
        );
        if resp.clicked() || resp.double_clicked() {
            view.scroll.to(k);
        }
        if resp.double_clicked() {
            asked = view.show();
        }
    }
    let max_top = len.saturating_sub(visible);
    let column = Rect::from_min_max(
        Pos2::new(list.right(), list.top()),
        Pos2::new(list.right() + chrome::CELL.x, list.bottom()),
    );
    chrome::scroll_bar(
        &ui.painter_at(column),
        column,
        (max_top > 0).then(|| view.scroll.top as f32 / max_top as f32),
    );
    // The foot: the buttons, then the keys' hint.
    let mut x = foot.left();
    for (word, act) in [
        (
            "Restore",
            BackupsView::restore as fn(&BackupsView) -> Option<BackupsEnd>,
        ),
        ("Show", BackupsView::show),
        ("Delete", BackupsView::delete),
    ] {
        let w = cw * (word.chars().count() + 2) as f32;
        let at = Rect::from_min_size(Pos2::new(x, foot.top()), Vec2::new(w, row_h));
        if ui.put(at, egui::Button::new(word)).clicked() {
            asked = act(view);
        }
        x += w + cw;
    }
    let hint_at = Pos2::new(x + cw, foot.top());
    let room = ((foot.right() - hint_at.x) / cw).floor().max(0.0) as usize;
    painter.text(
        hint_at,
        egui::Align2::LEFT_TOP,
        fit(&hint(), room),
        font,
        LOOK.dim,
    );
    asked
}
