//! The command log in the git panel (03 §3.7, Cmd/Ctrl+L): the host's `git::runner::CommandLog`,
//! newest first; Enter on an entry shows it whole.
//!
//! [`LogView`] holds the entries as `CommandLog::entries` gives them (newest first; the
//! integration hands them again with [`LogView::set_entries`] as commands run, and the bar stays
//! on its entry). It is drawn in the git panel's body in cells ([`draw`], X4-lists), one row an
//! entry — its time, host and `LogEntry::status_line` — and takes the git context's rows
//! ([`LogView::run`]): ↑ ↓ PgUp PgDn Home End move the bar, Enter or F3 (`Details`) shows the
//! entry whole in an `ops::dialog` dialog (time, host, repository, command line, stdin, ending,
//! duration, stdout size, and stderr whole in rows that scroll), Esc (`Back`) closes that, then
//! the log. The log is in memory only and its lines are masked already (03 §3.7).

use super::graph_view::printable;
use super::Scroll;
use crate::git::runner::{Ending, LogEntry};
use crate::gui::browser::keys::{self, Command, Mods};
use crate::gui::browser::ops::dialog::{self, Button, Ink, Inputs, Layout, Part};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
use crate::gui::chrome;
use crate::gui::chrome::git::LOOK;
use eframe::egui::{self, Key, Sense};
use std::time::SystemTime;
use unicode_segmentation::UnicodeSegmentation;

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
    /// The first of its stderr's rows the entry shown whole shows.
    top: usize,
    /// The entry shown whole as last laid out ([`Laid`]): its texts are wrapped once, not on each
    /// pass nor on each question of what it covers.
    laid: Option<Laid>,
}

/// The dialog of the entry shown whole, laid out on `grid` for entry `at` in the panel at column
/// `column` (its button's id).
#[derive(Debug, Clone)]
struct Laid {
    grid: Grid,
    at: usize,
    column: u16,
    layout: Layout,
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
            top: 0,
            laid: None,
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
        // Laid out by index: the entry at it may be another now.
        self.laid = None;
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
    /// for the panel and the other side to blank (10 §17, X4-graph's seam): the box [`draw`] laid
    /// out last, or — on a grid it has not drawn on yet — the one it will lay out.
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        let Some(at) = self.whole.filter(|&k| k < self.entries.len()) else {
            return Vec::new();
        };
        // Where the box stands is the grid's and the entry's alone, whatever the panel's column.
        match &self.laid {
            Some(l) if l.grid == *grid && l.at == at => l.layout.covers(),
            _ => entry_layout(*grid, &self.entries[at], 0).covers(),
        }
    }

    /// The entry shown whole, `at`, laid out on `grid` for the panel at column `column`, once for
    /// all three ([`Laid`]).
    fn lay_out(&mut self, grid: Grid, at: usize, column: u16) {
        let kept = matches!(&self.laid, Some(l) if (l.grid, l.at, l.column) == (grid, at, column));
        if !kept {
            let layout = entry_layout(grid, &self.entries[at], column);
            self.laid = Some(Laid {
                grid,
                at,
                column,
                layout,
            });
        }
    }

    /// A row of the git context's key table while the log is open (03 §9.2).
    pub fn run(&mut self, command: Command) -> Option<LogEnd> {
        match command {
            Command::Back => match self.whole.take() {
                Some(_) => None,
                None => Some(LogEnd::Close),
            },
            Command::Details if self.whole.is_none() && !self.entries.is_empty() => {
                self.show_whole(self.scroll.bar.min(self.entries.len() - 1));
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

    /// Entry `at` shown whole, its stderr from its first row.
    fn show_whole(&mut self, at: usize) {
        self.whole = Some(at);
        self.top = 0;
    }
}

/// How many cells wide the entry shown whole is inside its box: egui's 88, within the grid by the
/// kit's rule (`dialog::inside_width`), for its long command lines and stderr.
const WHOLE_WIDTH: usize = 88;

/// A field's name and the blanks after it: the widest, `Repository`, and two, so each value is a
/// text of its own (10 §6's two blank cells between two texts).
const NAME: usize = 12;

/// The modal of the entry shown whole, in the panel at column `column`: two repositories' panels
/// stand side by side in one `ui`, each with its own.
fn whole_id(column: u16) -> egui::Id {
    egui::Id::unique("sopen-git-log-entry").with(column)
}

/// Its OK button.
fn ok_id(column: u16) -> egui::Id {
    whole_id(column).with("ok")
}

/// A line of stderr as the dialog shows it: tabs to the next multiple of eight cells, a carriage
/// return at its end dropped, other control characters escaped.
fn stderr_line(line: &str) -> String {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut out = String::with_capacity(line.len());
    // The cells so far, counted as the line is read (not measured again per tab).
    let mut column = 0;
    for (k, part) in line.split('\t').enumerate() {
        if k > 0 {
            let n = 8 - column % 8;
            out.extend(std::iter::repeat_n(' ', n));
            column += n;
        }
        let part = printable(part);
        column += cells::width(&part);
        out.push_str(&part);
    }
    out
}

/// `line` in rows of at most `room` cells, as `dialog::wrap` breaks a text — at its last blank
/// that fits (the blank dropped), or after its last whole glyph — in one walk of the line:
/// `dialog::wrap` measures the rest of a line again for each row, and stderr's last 64 KiB can be
/// one line (03 §3.7). Each row it gives fits, so the kit's wrap keeps it whole.
pub(super) fn cut(line: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut used = 0;
    // The row's last blank after a glyph: its byte and the cells before it.
    let mut blank: Option<(usize, usize)> = None;
    for g in line.graphemes(true) {
        let w = cells::width(g);
        if used + w > room && !row.is_empty() {
            if g == " " {
                // The row ends at this blank, which goes.
                rows.push(std::mem::take(&mut row));
                (used, blank) = (0, None);
                continue;
            }
            match blank.take() {
                Some((at, before)) => {
                    let rest = row.split_off(at + 1);
                    row.truncate(at);
                    rows.push(std::mem::replace(&mut row, rest));
                    used -= before + 1;
                }
                None => {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
            }
        }
        if g == " " && !row.is_empty() {
            blank = Some((row.len(), used));
        }
        row.push_str(g);
        used += w;
    }
    if !row.trim().is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// The dialog of `entry` shown whole (03 §3.7) on `grid`, its OK button the one of the panel at
/// column `column`: each field a text, `Name       value`, wrapped by cells; then `Stderr` and
/// its lines in rows that scroll, at most half the grid's rows and six at least (egui's half the
/// window), `nothing` in the weak colour when there is none; then `[ OK ]` and the keys' hint.
fn entry_layout(grid: Grid, entry: &LogEntry, column: u16) -> Layout {
    let stdin = entry.stdin.as_deref().unwrap_or("nothing");
    let fields = [
        ("Time", date_time_text(entry.time)),
        ("Host", entry.host.clone()),
        ("Repository", entry.repo.clone()),
        ("Command", entry.line.clone()),
        ("Stdin", stdin.to_string()),
        ("Ending", entry.ending.to_string()),
        ("Duration", format!("{:.1} s", entry.duration.as_secs_f64())),
        ("Stdout", format!("{} bytes", entry.stdout_bytes)),
    ];
    let mut parts: Vec<Part> = (fields.into_iter())
        .map(|(name, value)| Part::Text(format!("{name:<NAME$}{}", printable(&value)), Ink::Text))
        .collect();
    // The box's inside: egui's 88 cells, or the grid less eight, whatever its texts want (88 is
    // above the kit's widest, 76), so its rows of stderr are cut to it here, once.
    let width = dialog::inside_width(grid.cols, WHOLE_WIDTH, 0);
    let rows: Vec<(String, Ink)> = match entry.stderr.as_str() {
        "" => vec![("nothing".to_string(), Ink::Weak)],
        all => (all.strip_suffix('\n').unwrap_or(all).split('\n'))
            .flat_map(|line| cut(&stderr_line(line), width))
            .map(|row| (row, Ink::Text))
            .collect(),
    };
    parts.extend([
        Part::Text("Stderr".to_string(), Ink::Text),
        Part::Scroll {
            rows,
            most: (grid.rows / 2).max(6),
            top: 0,
        },
        Part::Blank,
        Part::Buttons(vec![Button::new("OK", ok_id(column))]),
        Part::Text("Enter or Esc closes".to_string(), Ink::Weak),
    ]);
    #[cfg(test)]
    LAID.with(|n| n.set(n.get() + 1));
    dialog::layout(grid, "Command", width, &parts)
}

#[cfg(test)]
thread_local! {
    /// How many times [`entry_layout`] laid out an entry on this thread: its work, which the
    /// tests bound to once for a grid, however many passes show it.
    pub(super) static LAID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The entry shown whole (03 §3.7) on `grid` (the panels'), in the panel at column `column`: an
/// `ops::dialog` dialog ([`entry_layout`]), its stderr scrolled by the wheel and the box's scroll
/// bar. Enter, Esc, OK or a click outside closes it: true then.
fn whole_ui(ctx: &egui::Context, grid: Grid, view: &mut LogView, column: u16) -> bool {
    let Some(at) = view.whole.filter(|&k| k < view.entries.len()) else {
        return false;
    };
    let close = ctx.input_mut(|i| {
        keys::take(i, Key::Enter, Mods::None) | keys::take(i, Key::Escape, Mods::None)
    });
    view.lay_out(grid, at, column);
    let (Some(laid), top) = (&view.laid, &mut view.top) else {
        return close;
    };
    let inputs = Inputs {
        top: Some(top),
        ..Inputs::default()
    };
    let shown = dialog::show(ctx, whole_id(column), &laid.layout, inputs);
    close || shown.clicked(ok_id(column)) || shown.modal.should_close()
}

/// Whether [`draw`] paints the log's cells (10 §17, X4-graph's seam): it does (X4-lists), so the
/// panel leaves none of them to egui.
pub(super) const PAINTS_CELLS: bool = true;

/// The log at cells `area` of `grid` (the git panel's body), written into `buf`, the panel's
/// buffer (10 §4, X4-lists): the header dim in the heads' row, then one row an entry — a failed
/// one in the error colour — the bar on its entry, cut by cells, and the scroll bar on the
/// border right of `area`; and the entry shown whole, over everything, on `grid`. Only the rows
/// on screen are drawn; the wheel scrolls them by `CELL.y`, the bar stays. Each row is an
/// `interact` rect at its cells, its id salted with the panel's column (two repositories' panels
/// draw into one `ui`), sensing clicks alone (`Sense::CLICK`, never the keyboard's focus): a
/// click puts the bar on its entry, a double click on one entry shows it whole (the browser's
/// `second_click`, the pair the entry itself, never its place: an entry logged between the two
/// clicks moves the rows). At no cells (a panel too short for its parts) only the entry shown
/// whole is drawn, so its keys still reach it.
pub fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    area: Cells,
    buf: &mut Buffer,
    view: &mut LogView,
    active: bool,
) {
    let ctx = ui.ctx().clone();
    let p = chrome::PALETTE;
    let width = usize::from(area.width);
    let body = Pen::new(LOOK.text, p.back);
    cells::fill(buf, area, body);
    if area.height > 0 {
        // Over blanks in its own colours, which an ambiguous-wide terminal's `·` may take.
        let dim = Pen::new(LOOK.dim, p.back);
        let header = cells::fit_start(HEADER, width);
        cells::fill(buf, Cells::new(area.x, area.y, area.width, 1), dim);
        cells::put(buf, area.x, area.y, &header, width, dim);
    }
    // The entries under the header.
    let list = Cells::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(1),
    );
    let len = view.entries.len();
    if len == 0 && list.height > 0 {
        let dim = Pen::new(LOOK.dim, p.back);
        cells::put(buf, list.x, list.y, "no command has run", width, dim);
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
    let mut clicked = None;
    // Under the rows, all of the log's cells and egui's reach below them (the frame's rule): a click
    // on a blank of them moves no bar (egui gives a click a few points off a widget to it where
    // nothing is under the pointer).
    let under = ui.scope_id().with(("sopen-git-log-under", area.x));
    ui.interact(
        super::backups::below(ui, grid.rect(area)),
        under,
        Sense::CLICK,
    );
    for (n, k) in shown.enumerate() {
        let y = list.y + n as u16;
        let row = Cells::new(list.x, y, list.width, 1);
        let entry = &view.entries[k];
        let on_bar = k == view.scroll.bar;
        let back = match (on_bar, active) {
            (false, _) => p.back,
            (true, true) => p.bar,
            (true, false) => p.bar_idle,
        };
        let failed = !matches!(entry.ending, Ending::Exit(0));
        let fg = match (on_bar && active, failed) {
            (true, _) => LOOK.on_bar,
            (false, true) => LOOK.error,
            (false, false) => LOOK.text,
        };
        let pen = Pen::new(fg, back);
        cells::fill(buf, row, pen);
        let text = cells::fit_start(&printable(&row_text(entry)), width);
        cells::put(buf, list.x, y, &text, width, pen);
        let id = ui.scope_id().with(("sopen-git-log", area.x, k));
        let resp = ui.interact(grid.rect(row), id, Sense::CLICK);
        if resp.clicked() {
            let pair = egui::Id::unique(("sopen-git-log-click", area.x, entry.time, &entry.line));
            clicked = Some((k, super::super::second_click(&resp, pair)));
        }
    }
    if let Some((k, twice)) = clicked.filter(|_| view.whole.is_none()) {
        view.scroll.to(k);
        if twice {
            view.show_whole(k);
        }
    }
    // The scroll bar on the border, beside the rows.
    if list.height > 0 {
        let max_top = len.saturating_sub(visible);
        let at = (max_top > 0).then(|| view.scroll.top as f32 / max_top as f32);
        cells::scroll_bar(buf, area.right(), list.y, list.height, at);
    }
    if whole_ui(&ctx, *grid, view, area.x) {
        view.whole = None;
    }
}
