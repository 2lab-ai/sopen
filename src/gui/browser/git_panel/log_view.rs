//! The command log in the git panel (03 §3.7, Cmd/Ctrl+L): the host's `git::runner::CommandLog`,
//! newest first; Enter on an entry shows it whole.
//!
//! [`LogView`] holds the entries as `CommandLog::entries` gives them (newest first; the
//! integration hands them again with [`LogView::set_entries`] as commands run, and the bar stays
//! on its entry). It is drawn in the git panel's body ([`draw`]), one line an entry — its time,
//! host and `LogEntry::status_line` — and takes the git context's rows ([`LogView::run`]): ↑ ↓
//! PgUp PgDn Home End move the bar, Enter or F3 (`Details`) shows the entry whole in a dialog
//! (time, host, repository, command line, stdin, ending, duration, stdout size and stderr
//! whole), Esc (`Back`) closes that, then the log. The log is in memory only and its lines are
//! masked already (03 §3.7).

use super::graph_view::printable;
use super::Scroll;
use crate::git::runner::LogEntry;
use crate::gui::browser::keys::{self, Command, Mods};
use crate::gui::cells::{Buffer, Cells, Grid};
use crate::gui::chrome;
use crate::gui::chrome::git::LOOK;
use eframe::egui::{self, Key, Pos2, Rect, RichText, Sense, Vec2};
use std::time::SystemTime;

/// The log's header.
pub const HEADER: &str = "Command log · newest first · in memory only · Enter shows an entry whole";

/// What the log asks of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEnd {
    /// Esc with no entry shown whole: the log closes.
    Close,
}

/// The command log, open.
#[derive(Debug, Clone)]
pub struct LogView {
    entries: Vec<LogEntry>,
    scroll: Scroll,
    /// The entry shown whole, by index.
    whole: Option<usize>,
}

/// Unix seconds of a time (0 before the epoch).
fn secs(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `HH:MM:SS`, local time.
pub fn time_text(t: SystemTime) -> String {
    match chrome::local_tm(secs(t)) {
        Some(tm) => format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
        None => String::new(),
    }
}

/// `YYYY-MM-DD HH:MM:SS`, local time.
fn date_time_text(t: SystemTime) -> String {
    match chrome::local_tm(secs(t)) {
        Some(tm) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => String::new(),
    }
}

/// An entry's line in the log: `10:24:05  host  git push origin main · exit 0 · 1.2 s`.
pub fn row_text(entry: &LogEntry) -> String {
    format!(
        "{}  {}  {}",
        time_text(entry.time),
        entry.host,
        entry.status_line()
    )
}

impl LogView {
    /// The log of `entries`, newest first, the bar on the newest.
    pub fn new(entries: Vec<LogEntry>) -> LogView {
        LogView {
            entries,
            scroll: Scroll::default(),
            whole: None,
        }
    }

    /// The entries again (commands ran meanwhile): the bar, and the entry shown whole, stay on
    /// theirs when they are still in the log.
    pub fn set_entries(&mut self, entries: Vec<LogEntry>) {
        let find = |at: Option<usize>, entries: &[LogEntry]| {
            let was = at.and_then(|k| self.entries.get(k))?;
            entries.iter().position(|e| e == was)
        };
        let bar = find(Some(self.scroll.bar), &entries);
        let whole = find(self.whole, &entries);
        self.entries = entries;
        self.whole = whole;
        self.scroll.to(bar.unwrap_or(0));
    }

    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    /// The entry shown whole.
    pub fn whole(&self) -> Option<&LogEntry> {
        self.whole.and_then(|k| self.entries.get(k))
    }

    /// The cells of `grid` (the panels') the entry shown whole covers, its box and its shadow,
    /// for the panel and the other side to blank (10 §17, X4-graph's seam): none while egui draws
    /// it, until X4-lists.
    pub fn covers(&self, _grid: &Grid) -> Vec<Cells> {
        Vec::new()
    }

    /// A row of the git context's key table while the log is open (03 §9.2).
    pub fn run(&mut self, command: Command) -> Option<LogEnd> {
        match command {
            Command::Back => match self.whole.take() {
                Some(_) => None,
                None => Some(LogEnd::Close),
            },
            Command::Details if self.whole.is_none() && !self.entries.is_empty() => {
                self.whole = Some(self.scroll.bar.min(self.entries.len() - 1));
                None
            }
            Command::Bar(step) if self.whole.is_none() => {
                let delta = self.scroll.delta(step);
                self.scroll.step(delta, self.entries.len());
                None
            }
            _ => None,
        }
    }
}

fn whole_id() -> egui::Id {
    egui::Id::unique("sopen-git-log-entry")
}

/// The entry shown whole (03 §3.7) on `grid` (the panels', where it stands once it is in
/// cells): every field, stderr whole and scrollable. Enter, Esc or a click outside closes it.
fn whole_ui(ctx: &egui::Context, _grid: Grid, entry: &LogEntry) -> bool {
    let close = ctx.input_mut(|i| {
        keys::take(i, Key::Enter, Mods::None) | keys::take(i, Key::Escape, Mods::None)
    });
    let width = super::dialogs::width(ctx, 88.0);
    let room = (ctx.content_rect().height() * 0.5).max(6.0 * chrome::CELL.y);
    let mut ok = false;
    let response = chrome::dialog(ctx, whole_id(), "Command", |ui| {
        ui.set_width(width);
        let stdin = entry.stdin.clone().unwrap_or_else(|| "nothing".to_string());
        let fields = [
            ("Time", date_time_text(entry.time)),
            ("Host", entry.host.clone()),
            ("Repository", entry.repo.clone()),
            ("Command", entry.line.clone()),
            ("Stdin", stdin),
            ("Ending", entry.ending.to_string()),
            ("Duration", format!("{:.1} s", entry.duration.as_secs_f64())),
            ("Stdout", format!("{} bytes", entry.stdout_bytes)),
        ];
        egui::Grid::new(whole_id().with("fields"))
            .num_columns(2)
            .show(ui, |ui| {
                for (name, value) in fields {
                    ui.label(RichText::new(name).weak());
                    ui.add(egui::Label::new(value).wrap());
                    ui.end_row();
                }
            });
        ui.label(RichText::new("Stderr").weak());
        egui::ScrollArea::vertical()
            .id_salt("sopen-git-log-stderr")
            .max_height(room)
            .show(ui, |ui| {
                let text = if entry.stderr.is_empty() {
                    "nothing"
                } else {
                    entry.stderr.as_str()
                };
                ui.add(egui::Label::new(text).wrap());
            });
        ui.add_space(chrome::CELL.y / 2.0);
        ok = ui.button("OK").clicked();
        ui.label(RichText::new("Enter or Esc closes").weak());
    });
    close || ok || response.should_close()
}

/// Whether [`draw`] paints the log's cells (10 §17, X4-graph's seam): not yet — egui draws it at
/// `grid.rect(cells)`, its scroll bar in its own last column, and the panel leaves
/// those cells to `term::compose` (a hand-off box in a terminal) until X4-lists makes it `true`.
pub(super) const PAINTS_CELLS: bool = false;

/// The log at cells `cells` of `grid` (the git panel's body): the header, one line an entry with
/// the bar, the scroll bar; and the entry shown whole, over everything, on `grid`. `buf` is the
/// panel's buffer, for the log's cells once it paints them ([`PAINTS_CELLS`]). A click puts the
/// bar on an entry, a double click shows it whole.
pub fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    cells: Cells,
    _buf: &mut Buffer,
    view: &mut LogView,
    active: bool,
) {
    let rect = grid.rect(cells);
    let ctx = ui.ctx().clone();
    let font = chrome::mono(ui);
    let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = text_h.ceil();
    let painter = ui.painter_at(rect);
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), row_h));
    let list = Rect::from_min_max(
        Pos2::new(rect.left(), header.bottom()),
        Pos2::new(rect.right() - chrome::CELL.x, rect.bottom()),
    );
    let chars = (list.width() / cw).floor().max(0.0) as usize;
    let fit = |s: &str| chrome::fit_start(&ctx, &printable(s), &font, cw * chars as f32);
    painter.text(
        header.left_top(),
        egui::Align2::LEFT_TOP,
        fit(HEADER),
        font.clone(),
        LOOK.dim,
    );
    if view.entries.is_empty() {
        painter.text(
            list.left_top(),
            egui::Align2::LEFT_TOP,
            "no command has run",
            font.clone(),
            LOOK.dim,
        );
    }
    let visible = ((list.height() / row_h).floor() as usize).max(1);
    let len = view.entries.len();
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
    for k in range.clone() {
        let y = list.top() + row_h * (k - range.start) as f32;
        let row = Rect::from_min_size(Pos2::new(list.left(), y), Vec2::new(list.width(), row_h));
        let painter = painter.with_clip_rect(row.intersect(list));
        let on_bar = k == view.scroll.bar;
        if on_bar {
            chrome::bar(&painter, row, active);
        }
        let entry = &view.entries[k];
        let failed = !matches!(entry.ending, crate::git::runner::Ending::Exit(0));
        let colour = match (on_bar && active, failed) {
            (true, _) => LOOK.on_bar,
            (false, true) => LOOK.error,
            (false, false) => LOOK.text,
        };
        painter.text(
            row.left_top(),
            egui::Align2::LEFT_TOP,
            fit(&row_text(entry)),
            font.clone(),
            colour,
        );
        let resp = ui.interact(
            row,
            ui.scope_id().with(("sopen-git-log", k)),
            if active {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        if resp.clicked() || resp.double_clicked() {
            view.scroll.to(k);
        }
        if resp.double_clicked() {
            view.whole = Some(k);
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
    if let Some(entry) = view.whole().cloned() {
        if whole_ui(&ctx, *grid, &entry) {
            view.whole = None;
        }
    }
}
