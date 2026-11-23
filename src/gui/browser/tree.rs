//! The directory tree as a side's content (05 §1, §3, §6.1): in place of the active panel's list
//! (02 §3.1 O4), its rows, the bar, the keys of the tree's context ([`super::keys`]) and its F-key
//! row (`1Help` and `10Tree`), drawn in the look of [`crate::gui::chrome::tree`] from the model of
//! [`crate::tree`].
//!
//! It draws from an input of its own, not from the model: a [`TreeView`] holding one [`TreeRow`]
//! per drawn row, the bar and the status row's words. The integration (S5-C) builds the rows from
//! the model, reads the keys with [`keys`] before the pass draws, draws with [`draw`] and carries
//! out the [`TreeAction`]s both give. The bar is the view's: its keys, the letters and a click
//! move it here and ask nothing of the model. The browser (`Browser::tree_pass`) is that
//! integration: it maps [`crate::tree::Row`]s to [`TreeRow`]s every pass.

use super::keys::{self, Command, Context, Mods, Step, Trigger};
use crate::gui::chrome::tree::{self as look, LOOK};
use crate::gui::chrome::{self, FKey};
use crate::gui::Status;
use crate::tree as model;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2};
use std::time::Duration;

/// What the tree hands the browser when it is done (02 §7, 05 §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeOutcome {
    /// Enter on a row: list this folder in the active panel and close the tree — on a compact
    /// chain its last folder, on a link its target, resolved (absolute, on the panel's host).
    GoTo(String),
}

/// One drawn row of the tree, as the model hands it over (S5-C builds them from [`crate::tree`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// What the model knows the row by; every [`TreeAction`] on the row names it.
    pub id: usize,
    /// Levels below the root (the root is 0).
    pub depth: usize,
    /// The folder's name; a compact chain's names joined by `/` (`a/b/c`, 05 §3); the root's
    /// `~` or `/` (05 §2). Not read for a [`RowKind::Hidden`] row.
    pub name: String,
    pub kind: RowKind,
    pub fold: Fold,
    /// A hidden folder's own row (a dot name or `UF_HIDDEN`, 05 §3): drawn dimmed.
    pub hidden: bool,
    /// A mount point of a disk filesystem (R3) or a macOS `nobrowse` volume (R7): its size
    /// slot shows the filesystem's figure ([`Info::fs`]) until a deep scan gives its own, and
    /// the status row says what the filesystem holds while the bar is on it (05 §6.1).
    pub boundary: bool,
    pub info: Info,
}

/// What a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A folder, or a compact chain of them.
    Folder,
    /// A link to a folder: `→`, and it never opens in place (05 §3).
    Link,
    /// A folder's one dimmed row for its N hidden subfolders, `· N hidden folders`; → or Enter
    /// opens it into a row each (05 §3).
    Hidden(usize),
}

/// Whether a folder's rows are drawn below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    /// Opened by the owner, or always open (the root and the path, 05 §5 rule 1): `▼`.
    Open,
    /// Opened by itself (05 §5 rules 3–5): `▼` as well; ← closes it as any open folder.
    AutoOpened,
    /// Closed with subfolders, or with subfolders not known yet (a listing stopped early, or
    /// none yet, 05 §6.3): `►`.
    Closed,
    /// Listed, with no subfolders: no glyph.
    Leaf,
}

/// A row's info line (05 §6.1): three right-aligned columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Info {
    /// Subfolders (links to folders included); `None` before the folder's listing.
    pub dirs: Option<Count>,
    /// Files (regular files and links to files); `None` before the folder's listing.
    pub files: Option<Count>,
    pub size: Size,
    /// A boundary row's filesystem, from one `statvfs` (05 §6.1), once it has answered.
    pub fs: Option<FsUse>,
    /// A deep scan of the row is running: the moving mark after its size (05 §6.3).
    pub scanning: bool,
}

/// A count of the info line: `19 d`; `9 d+` when the listing stopped early (05 §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Count {
    pub n: u64,
    /// A lower bound: more may be there.
    pub more: bool,
}

/// The size slot of the info line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Size {
    /// Nothing known yet (or a boundary row's filesystem figure, [`Info::fs`]).
    #[default]
    Unknown,
    /// The bytes of the regular files counted so far (05 §6.1).
    Bytes {
        n: u64,
        /// `+`: more may be below (a lower bound).
        more: bool,
        /// `~`: from a scan finished earlier in this process, not re-checked.
        cached: bool,
    },
    /// A tag in place of the size.
    Tag(Tag),
}

/// The tags of 05 §6.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// `<LINK>` (R1).
    Link,
    /// `<SYS>` (R4).
    Sys,
    /// `<NET>` (R5).
    Net,
    /// `<AUTO>` (R6).
    Auto,
    /// `<SYSVOL>` (R7).
    SysVol,
    /// `?`: unreadable (R2).
    Unreadable,
    /// `not answering` (05 §6.2).
    NotAnswering,
}

/// What a filesystem holds, from one `statvfs`: `(f_blocks − f_bfree) × f_frsize` and
/// `f_files − f_ffree` (05 §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsUse {
    pub bytes: u64,
    pub files: u64,
}

/// What a key or a click on the tree asks of the model; S5-C carries it out. The bar keys move
/// the view's own bar and give none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeAction {
    /// F1: the key help of the tree.
    Help,
    /// F10, Alt+F10, Esc: close the tree, the panel as it was.
    Close,
    /// Enter (or a double click) on a folder or a link: list it in the active panel and close
    /// the tree ([`TreeOutcome::GoTo`]; a chain's last folder, a link's target, resolved).
    GoTo(usize),
    /// → on a folder that is not open (listing it if it never was); → or Enter on a hidden
    /// folders' row (it opens into a row each).
    Expand(usize),
    /// ← on an open folder.
    Collapse(usize),
    /// Space: size the row now (05 §6.3).
    SizeNow(usize),
    /// Cmd/Ctrl+R: list the row again.
    Reread(usize),
    /// A typed letter. Already done: the view's bar is on the next row starting with it, if
    /// any — nothing is left for the model to do.
    Jump(char),
}

/// The tree on one side: the rows, the bar and what the status row says.
#[derive(Debug, Clone, Default)]
pub struct TreeView {
    /// The frame's title: the root's `host:/abs` (05 §1).
    pub title: String,
    pub rows: Vec<TreeRow>,
    /// The row under the bar (an index into `rows`).
    pub bar: usize,
    /// The first row on screen.
    pub top: usize,
    /// What the whole tree says on the status row (`4 calls not answering; …`, 05 §6.2), over
    /// what the bar's row says.
    pub status: Option<Status>,
    /// Rows the last pass had room for.
    visible: usize,
    /// The bar moved since the last pass: that pass scrolls it into view.
    moved: bool,
    /// The first pass with rows puts the bar's row in the middle (05 §1).
    center: bool,
}

impl TreeView {
    /// The tree titled `title` with the bar on row `bar`: the first pass that has rows scrolls
    /// it to the middle of the tree (05 §1).
    pub fn new(title: impl Into<String>, rows: Vec<TreeRow>, bar: usize) -> Self {
        Self {
            title: title.into(),
            rows,
            bar,
            center: true,
            ..Default::default()
        }
    }

    /// The rows anew (a listing, a size, a folder opened or closed): the bar stays on its row,
    /// by id, where it was on screen — rows added or gone above it scroll with it; its row gone,
    /// the bar stays in the rows.
    pub fn set_rows(&mut self, rows: Vec<TreeRow>) {
        let was = self.bar_row().map(|r| r.id);
        self.rows = rows;
        match was.and_then(|id| self.rows.iter().position(|r| r.id == id)) {
            Some(at) => {
                self.top = self.top.saturating_add(at).saturating_sub(self.bar);
                self.bar = at;
            }
            None => self.bar = self.bar.min(self.rows.len().saturating_sub(1)),
        }
        if self.visible > 0 {
            self.top = self.top.min(self.rows.len().saturating_sub(self.visible));
        }
    }

    /// The row under the bar.
    pub fn bar_row(&self) -> Option<&TreeRow> {
        self.rows.get(self.bar)
    }

    /// The rows the last pass drew (empty before the first): what 05 §6.3's phases 2 and 3
    /// read around (the browser tells the model `top` and [`TreeView::height`]).
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn on_screen(&self) -> std::ops::Range<usize> {
        let end = self.top.saturating_add(self.visible).min(self.rows.len());
        self.top.min(end)..end
    }

    /// The rows the last pass had room for (0 before the first): the screen's height the model
    /// is told (`crate::tree::Tree::view`).
    pub fn height(&self) -> usize {
        self.visible
    }

    /// The next pass puts the bar's row in the middle again, as the first pass with rows did
    /// (rows that opened by themselves above it since, 05 §5).
    pub fn centre(&mut self) {
        self.center = true;
    }

    /// Puts the bar on the row `id` names, scrolled into view by the next pass (← on a closed
    /// row: its parent's, as the model names it); nothing when no row has it.
    pub fn put_bar(&mut self, id: usize) {
        if let Some(at) = self.rows.iter().position(|r| r.id == id) {
            self.bar = at;
            self.moved = true;
        }
    }

    /// The status row while the tree is shown: what the whole tree says; else, the bar on a
    /// boundary row, what its filesystem holds (05 §6.1); else what the keys do.
    pub fn status_line(&self) -> Status {
        if let Some(said) = &self.status {
            return said.clone();
        }
        match self.bar_row() {
            Some(row) if row.boundary => match row.info.fs {
                Some(fs) => Status::info(format!(
                    "{} is a mount point: {} and {} files used on its filesystem",
                    row.name,
                    size_text(fs.bytes, false, false),
                    fs.files
                )),
                None => Status::info(HINT),
            },
            _ => Status::info(HINT),
        }
    }

    /// Carries out `command` on the view — the bar keys and the letters move the bar here —
    /// and gives what is left for the model, naming the bar's row. Of the rest it decides only
    /// what 05 says: → on a link does nothing (05 §3), → and Enter open the hidden folders' row
    /// (05 §3), ← on a row that is not open takes the bar to its parent (05 §1).
    fn run(&mut self, command: Command) -> Option<TreeAction> {
        use TreeAction::*;
        // A bar the caller left past the end (the fields are public; rows replaced without
        // `set_rows`) is on the last row before any key acts — not only once a pass draws.
        self.clamp_bar();
        let page = self.visible.saturating_sub(1).max(1) as isize;
        match command {
            Command::Help => return Some(Help),
            Command::CloseTree => return Some(Close),
            Command::Bar(step) => {
                match step {
                    Step::Up => self.step(-1),
                    Step::Down => self.step(1),
                    Step::PageUp => self.step(-page),
                    Step::PageDown => self.step(page),
                    Step::Home => self.step(isize::MIN),
                    Step::End => self.step(isize::MAX),
                }
                return None;
            }
            Command::Jump(c) => {
                self.jump(c);
                return Some(Jump(c));
            }
            _ => {}
        }
        let row = self.bar_row()?;
        let (id, kind) = (row.id, row.kind);
        let open = matches!(row.fold, Fold::Open | Fold::AutoOpened);
        match (command, kind) {
            (Command::GoTo | Command::Expand, RowKind::Hidden(_)) => Some(Expand(id)),
            (Command::GoTo, _) => Some(GoTo(id)),
            (Command::Expand, RowKind::Link) => None,
            (Command::Expand, _) if open => None,
            (Command::Expand, _) => Some(Expand(id)),
            (Command::Collapse, RowKind::Folder) if open => Some(Collapse(id)),
            (Command::Collapse, _) => {
                self.bar_to_parent();
                None
            }
            (Command::SizeNow, _) => Some(SizeNow(id)),
            (Command::Reread, _) => Some(Reread(id)),
            _ => None,
        }
    }

    /// Moves the bar by `delta` rows, stopping at the first and the last.
    fn step(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.bar = self.bar.saturating_add_signed(delta).min(last);
        self.moved = true;
    }

    /// Moves the bar to the next row after it whose name starts with `c` (any case), around the
    /// end, as the list's letters do (01 §3a); the hidden folders' row has no name to start.
    fn jump(&mut self, c: char) {
        self.clamp_bar();
        let n = self.rows.len();
        let wanted: String = c.to_lowercase().collect();
        // After the bar to the end, then from the first row to the bar: `bar < n` here, so
        // nothing is added past `n`.
        let bar = self.bar;
        for at in (bar + 1..n).chain(0..=bar.min(n.saturating_sub(1))).take(n) {
            let row = &self.rows[at];
            if !matches!(row.kind, RowKind::Hidden(_))
                && row.name.to_lowercase().starts_with(&wanted)
            {
                self.bar = at;
                self.moved = true;
                return;
            }
        }
    }

    /// The bar to the row's parent: the nearest row above it a level up.
    fn bar_to_parent(&mut self) {
        let Some(depth) = self.bar_row().map(|r| r.depth) else {
            return;
        };
        if let Some(at) = self.rows[..self.bar].iter().rposition(|r| r.depth < depth) {
            self.bar = at;
            self.moved = true;
        }
    }

    /// The bar on a row: on the last one when it is past the end, on 0 with no rows.
    fn clamp_bar(&mut self) {
        self.bar = self.bar.min(self.rows.len().saturating_sub(1));
    }

    /// Before a pass that has room for `visible` rows: the first pass with rows puts the bar's
    /// row in the middle; later, a bar that moved is scrolled into view; the rows never end
    /// above the bottom while there are more.
    fn settle(&mut self, visible: usize) {
        self.visible = visible;
        let len = self.rows.len();
        self.clamp_bar();
        if self.center && len > 0 {
            self.center = false;
            self.moved = false;
            self.top = self.bar.saturating_sub(visible / 2);
        } else if std::mem::take(&mut self.moved) {
            if self.bar < self.top {
                self.top = self.bar;
            } else if self.bar >= self.top.saturating_add(visible) {
                self.top = self.bar + 1 - visible;
            }
        }
        self.top = self.top.min(len.saturating_sub(visible));
    }
}

/// What the status row says while nothing else is to be said.
const HINT: &str =
    "Enter list it · → open · ← close · Space size it · a letter jumps · Esc back · F1 the keys";

/// The info line's columns, in cells (05 §6.1): the room each piece is right-aligned in —
/// `1368 d`, `9991 f+`, `=87G fs` or `~9.8M+` — two cells between them, then a blank cell and
/// the scan mark's. A wider piece (`not answering`) moves its row's pieces to its left.
const DIRS: isize = 6;
const FILES: isize = 7;
const SIZE: isize = 7;
const GAP: isize = 2;
const MARK: isize = 2;

/// The cells a name keeps however deep its row: deeper rows stop moving right (`node_modules`
/// fits).
const NAME: isize = 12;

/// Below this many cells a side drops the counts and keeps the size, as the list drops its Date
/// and Time columns below the same width (`Browser::panel_ui`).
const COUNTED: usize = 48;

/// The tree's keys this frame, through the key table's tree context (04 §3: exact modifiers),
/// in order: each carried out on the view, what is left for the model returned.
pub fn keys(ctx: &egui::Context, view: &mut TreeView) -> Vec<TreeAction> {
    let mut asked = Vec::new();
    while let Some(command) = ctx.input_mut(|i| keys::lookup(i, Context::Tree)) {
        asked.extend(view.run(command));
    }
    asked
}

/// The tree's F-key row (05 §1): the key table's plain F-keys of the tree context, by number —
/// `1Help` and `10Tree`.
pub fn fkeys() -> Vec<FKey> {
    let mut row: Vec<FKey> = keys::rows(Context::Tree, keys::LANDED_SLICES)
        .filter_map(|r| match r.trigger {
            Trigger::Key(key, Mods::None) if !r.label.is_empty() => {
                chrome::fkey_number(key).map(|n| FKey { n, label: r.label })
            }
            _ => None,
        })
        .collect();
    row.sort_by_key(|k| k.n);
    row
}

/// A size as the info line says it (05 §6.1): in the list's units, powers of 1000, the smallest
/// unit that fits in the letters of 05's examples — `60B`, `79K`, `9.8M` (one decimal under
/// ten), `87G` — after `~` when `cached`, before `+` when `more`. Never more than six cells.
pub fn size_text(bytes: u64, more: bool, cached: bool) -> String {
    const UNITS: [char; 7] = ['B', 'K', 'M', 'G', 'T', 'P', 'E'];
    let mut number = bytes.to_string();
    let mut unit = 0;
    if bytes >= 1000 {
        let mut value = bytes as f64;
        loop {
            value /= 1000.0;
            unit += 1;
            if value < 9.95 {
                number = format!("{value:.1}");
                break;
            }
            if value < 999.5 || unit + 1 == UNITS.len() {
                number = format!("{value:.0}");
                break;
            }
        }
    }
    let cached = if cached { "~" } else { "" };
    let more = if more { "+" } else { "" };
    format!("{cached}{number}{}{more}", UNITS[unit])
}

impl Count {
    /// `19 d`, `9 d+`.
    pub fn text(self, letter: char) -> String {
        let more = if self.more { "+" } else { "" };
        format!("{} {letter}{more}", self.n)
    }
}

impl Tag {
    /// The tag as the info line shows it.
    pub fn text(self) -> &'static str {
        match self {
            Tag::Link => "<LINK>",
            Tag::Sys => "<SYS>",
            Tag::Net => "<NET>",
            Tag::Auto => "<AUTO>",
            Tag::SysVol => "<SYSVOL>",
            Tag::Unreadable => "?",
            Tag::NotAnswering => "not answering",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Tag::Unreadable => LOOK.unreadable,
            Tag::NotAnswering => LOOK.stalled,
            _ => LOOK.tag,
        }
    }
}

/// A row's info pieces, `[dirs, files, size]`, each with its colour (`info` for counts and
/// sizes: the row's own). The size slot: a tag, else the bytes counted, else a boundary's
/// filesystem (`=87G fs`) until a scan gives the folder's own.
fn info_pieces(row: &TreeRow, info: Color32) -> [Option<(String, Color32)>; 3] {
    let i = &row.info;
    let size = match i.size {
        Size::Tag(tag) => Some((tag.text().to_string(), tag.color())),
        Size::Bytes { n, more, cached } => Some((size_text(n, more, cached), info)),
        Size::Unknown => {
            i.fs.filter(|_| row.boundary)
                .map(|fs| (format!("={} fs", size_text(fs.bytes, false, false)), info))
        }
    };
    [
        i.dirs.map(|c| (c.text('d'), info)),
        i.files.map(|c| (c.text('f'), info)),
        size,
    ]
}

/// The tree in `rect` (05 §1, §3, §6.1): its frame titled with the root, a row a cell tall each —
/// the glyph, the name two cells deeper a level, the info line's columns at the right — the bar,
/// and the scroll bar on the right border. A click puts the bar on a row, a double click is Enter
/// there and the wheel scrolls the rows, as in the list; what they ask of the model is returned.
pub fn draw(ui: &mut egui::Ui, rect: Rect, view: &mut TreeView, active: bool) -> Vec<TreeAction> {
    let ctx = ui.ctx().clone();
    let painter = ui.painter_at(rect);
    let inner = chrome::panel_frame(&painter, rect, &view.title, active);
    let font = chrome::mono(ui);
    let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    // Rows touch, a cell apart, as on a text screen.
    let row_h = text_h.ceil();
    let visible = ((inner.height() / row_h).floor() as usize).max(1);
    // The wheel scrolls the rows (the bar stays where it is).
    if ui.rect_contains_pointer(rect) {
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
        let max_top = view.rows.len().saturating_sub(visible);
        view.top = (view.top as f32 + lines).round().clamp(0.0, max_top as f32) as usize;
    }
    view.settle(visible);
    let chars = (inner.width() / cw).floor().max(0.0) as isize;
    let counted = chars >= COUNTED as isize;
    let x = |cells: isize| inner.left() + cw * cells as f32;
    let now = ctx.input(|i| i.time);
    let step = (now / look::SCAN_STEP).floor();
    let mark = look::SCAN_MARKS[step as usize % look::SCAN_MARKS.len()];
    let mut scanning = false;
    let mut clicked = None;
    for (k, at) in (view.top..view.rows.len()).take(visible).enumerate() {
        let row = &view.rows[at];
        let y = inner.top() + row_h * k as f32;
        let row_rect =
            Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), row_h));
        let on_bar = at == view.bar;
        if on_bar {
            chrome::bar(&painter, row_rect, active);
        }
        let paint = |c: Color32| if on_bar && active { LOOK.on_bar } else { c };
        let dim = row.hidden || matches!(row.kind, RowKind::Hidden(_));
        let text = |cell: isize, align: egui::Align2, s: &str, c: Color32| {
            painter.text(Pos2::new(x(cell), y), align, s, font.clone(), paint(c));
        };
        // The info line, from the right: each piece right-aligned in its column, a wider one
        // moving the pieces to its left.
        let [dirs, files, size] = info_pieces(row, if dim { LOOK.hidden } else { LOOK.info });
        let slots = [(SIZE, size), (FILES, files), (DIRS, dirs)];
        let mut edge = chars - MARK;
        let mut info_left = edge;
        for (width, piece) in slots.into_iter().take(if counted { 3 } else { 1 }) {
            let mut left = edge - width;
            if let Some((s, c)) = piece {
                left = left.min(edge - s.chars().count() as isize);
                text(edge, egui::Align2::RIGHT_TOP, &s, c);
            }
            info_left = left;
            edge = left - GAP;
        }
        if row.info.scanning {
            scanning = true;
            text(
                chars - 1,
                egui::Align2::LEFT_TOP,
                &mark.to_string(),
                LOOK.mark,
            );
        }
        // The glyph and the name, a blank cell before the info line at least; a row deeper
        // than the side has room for sits at the deepest indent that leaves its name its cells.
        let name_end = info_left - 1;
        let depth = isize::try_from(row.depth).unwrap_or(isize::MAX);
        let indent = depth.saturating_mul(2).min((name_end - 2 - NAME).max(0));
        let name_color = if dim { LOOK.hidden } else { LOOK.name };
        let clip = Rect::from_min_max(
            Pos2::new(x(indent).min(x(name_end)), y),
            Pos2::new(x(name_end), y + row_h),
        );
        let (name, name_at) = match row.kind {
            RowKind::Hidden(n) => {
                let s = if n == 1 { "" } else { "s" };
                (format!("{} {n} hidden folder{s}", look::HIDDEN), indent)
            }
            _ => (row.name.clone(), indent + 2),
        };
        let glyph = match (row.kind, row.fold) {
            (RowKind::Link, _) => Some(look::LINK),
            (RowKind::Hidden(_), _) | (_, Fold::Leaf) => None,
            (_, Fold::Open | Fold::AutoOpened) => Some(look::OPEN),
            (_, Fold::Closed) => Some(look::CLOSED),
        };
        let room = (x(name_end) - x(name_at)).max(0.0);
        let shown = chrome::fit_start(&ctx, &name, &font, room);
        let clipped = painter.with_clip_rect(clip.intersect(rect));
        for (cell, s) in glyph
            .map(|g| (indent, g.to_string()))
            .into_iter()
            .chain([(name_at, shown)])
        {
            clipped.text(
                Pos2::new(x(cell), y),
                egui::Align2::LEFT_TOP,
                s,
                font.clone(),
                paint(name_color),
            );
        }
        let resp = ui.interact(
            row_rect,
            ui.scope_id().with(("sopen-tree", at)),
            Sense::click(),
        );
        if resp.clicked() {
            // The row by its id: rows come and go above it between two clicks (05 §6.3).
            let pair = egui::Id::unique(("sopen-tree-row", row.id));
            clicked = Some((at, super::second_click(&resp, pair)));
        }
    }
    // The scroll bar on the right border, beside the rows.
    let max_top = view.rows.len().saturating_sub(visible);
    let column = Rect::from_min_max(
        Pos2::new(inner.right(), inner.top()),
        Pos2::new(inner.right() + chrome::CELL.x, inner.bottom()),
    );
    chrome::scroll_bar(
        &painter,
        column,
        (max_top > 0).then(|| view.top as f32 / max_top as f32),
    );
    if scanning {
        // The mark's next step.
        let next = (step + 1.0) * look::SCAN_STEP - now;
        ctx.request_repaint_after(Duration::from_secs_f64(next.max(0.001)));
    }
    let mut asked = Vec::new();
    if let Some((at, double)) = clicked {
        view.bar = at;
        view.moved = true;
        if double {
            asked.extend(view.run(Command::GoTo));
        }
    }
    asked
}

/// The view's id of a model row (S5-C's mapping): a folder's id doubled, its `· N hidden folders`
/// row's one more — stable while the tree is open, as the model's ids are.
pub fn view_id(id: model::RowId) -> usize {
    match id {
        model::RowId::Folder(n) => (n as usize) << 1,
        model::RowId::Hidden(n) => (n as usize) << 1 | 1,
    }
}

/// The model's row of a view id ([`view_id`]'s inverse).
pub fn model_id(id: usize) -> model::RowId {
    let n = (id >> 1) as u32;
    if id & 1 == 0 {
        model::RowId::Folder(n)
    } else {
        model::RowId::Hidden(n)
    }
}

/// A model row as the view draws it (05 §3, §6.1). `fs`: the filesystem figure last seen of a
/// boundary row — the model gives it only until a scan gives the folder's own size, and the
/// status row goes on saying it while the bar is there.
pub fn view_row(row: &model::Row, fs: Option<u64>) -> TreeRow {
    let i = &row.info;
    let (size, scanning, used) = match i.size {
        model::Size::Unknown => (Size::Unknown, false, None),
        model::Size::Bytes {
            bytes,
            more,
            cached,
            scanning,
        } => (
            Size::Bytes {
                n: bytes,
                more,
                cached,
            },
            scanning,
            None,
        ),
        model::Size::Fs { used_bytes } => (Size::Unknown, false, Some(used_bytes)),
        model::Size::Tag(tag) => (Size::Tag(tag_of(tag)), false, None),
    };
    let count = |c: Option<model::Count>| {
        c.map(|c| Count {
            n: c.n,
            more: c.more,
        })
    };
    TreeRow {
        id: view_id(row.id),
        depth: row.depth,
        name: row.name.clone(),
        kind: match row.kind {
            model::RowKind::Folder => RowKind::Folder,
            model::RowKind::Link => RowKind::Link,
            model::RowKind::Hidden(n) => RowKind::Hidden(n),
        },
        fold: match row.glyph {
            model::Glyph::Open if row.auto => Fold::AutoOpened,
            model::Glyph::Open => Fold::Open,
            model::Glyph::None => Fold::Leaf,
            model::Glyph::Closed | model::Glyph::Link => Fold::Closed,
        },
        hidden: row.dimmed,
        // R3 (the model reads its filesystem's used files there); R7 is its `<SYSVOL>` tag.
        boundary: i.fs_files.is_some(),
        info: Info {
            dirs: count(i.dirs),
            files: count(i.files),
            size,
            fs: i
                .fs_files
                .zip(used.or(fs))
                .map(|(files, bytes)| FsUse { bytes, files }),
            scanning,
        },
    }
}

fn tag_of(tag: model::Tag) -> Tag {
    match tag {
        model::Tag::Link => Tag::Link,
        model::Tag::Sys => Tag::Sys,
        model::Tag::Net => Tag::Net,
        model::Tag::Auto => Tag::Auto,
        model::Tag::SysVol => Tag::SysVol,
        model::Tag::Unreadable => Tag::Unreadable,
        model::Tag::NotAnswering => Tag::NotAnswering,
    }
}

#[cfg(test)]
mod tests;
