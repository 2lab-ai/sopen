//! The details pane (03 §6.1): a commit's, a stash's or the uncommitted row's summary and files,
//! a comparison of two commits (§6.4), and the diffs and files at a revision it opens (§6.2–§6.3).
//!
//! The pane is one list of lines — the summary's fields (`Commit`, `Parents`, `Author`, `Author
//! date`, `Committer` and `Commit date` when they differ, `In`), the whole message, then each
//! group's head and file rows — with a bar of its own. Enter or F3 on a commit row opens it with
//! the keyboard on its first file; Esc gives the keyboard back to the graph, and the pane then
//! follows the graph's bar; Esc again closes it (03 §6.1). Its reads are the integration's: the
//! pane shows `reading…` until [`super::GitView::set_details`] hands over the details of exactly
//! the subject it shows (a result for another is dropped).
//!
//! The pane is cells (10 §4, X4-pane): each line a row of the panel's buffer, every width a count
//! of cells (unicode-width) — the message wrapped by cells at a blank ([`Pane::wrap`],
//! `dialog::wrap`'s rule), its tabs at every eighth cell — so the window and the terminal show
//! the same cells. In an ambiguous-wide terminal (10 §10, T38) the pane's own `√` and a rename's
//! `→` are §10's stand-ins, at the source; paths and messages are content and stay as written.

use super::graph_view::{date_text, printable};
use super::{Scroll, Subject};
use crate::git::model::Oid;
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
use crate::gui::chrome;
use crate::gui::chrome::git::{self as look, LOOK};
use crate::gui::term;
use eframe::egui::{self, Color32, Sense};
use unicode_segmentation::UnicodeSegmentation;

/// What a details read found (03 §6.1), for [`super::GitView::set_details`]. The field names are
/// S2-B's `CommitDetails` and `ChangedFile`'s, so S2-F maps one to the other field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    /// What it was read for: the pane drops it unless it still shows this.
    pub subject: Subject,
    /// A commit's or a stash's summary (`log -1 -z --format=…`); `None` for the uncommitted row
    /// and a comparison.
    pub summary: Option<Summary>,
    /// `In`: the branches and tags that contain the commit, once `for-each-ref --contains` has
    /// answered; `None` until then (`…`).
    pub contains: Option<Vec<String>>,
    /// The files, by group, in the order drawn.
    pub groups: Vec<FileGroup>,
    /// A read that failed: its message, in the pane.
    pub error: Option<String>,
}

/// A commit's summary: S2-B's `CommitDetails`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub oid: Oid,
    /// The first parent first.
    pub parents: Vec<Oid>,
    pub author_name: String,
    pub author_email: String,
    /// Unix seconds.
    pub author_time: i64,
    pub committer_name: String,
    pub committer_email: String,
    pub commit_time: i64,
    /// `%B`, whole.
    pub message: String,
}

/// The files of one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileGroup {
    pub group: Group,
    pub files: Vec<FileRow>,
}

/// Which files a group holds: what a file row's two endpoints are (03 §6.2's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    /// A commit's files against its first parent (none for a root), a stash's tracked files, a
    /// comparison's files.
    Files,
    /// The uncommitted row's unmerged paths, first during a merge, rebase, cherry-pick or revert.
    Conflicts,
    /// The uncommitted row's staged changes.
    Staged,
    /// The uncommitted row's changes not staged.
    NotStaged,
    /// The uncommitted row's untracked files (a folder is one `dir/` row), or a stash's (its
    /// third parent, `?`).
    Untracked,
}

impl Group {
    /// Its head in the pane.
    pub fn head(self) -> &'static str {
        match self {
            Group::Files => "Files",
            Group::Conflicts => "Conflicts",
            Group::Staged => "Staged",
            Group::NotStaged => "Not staged",
            Group::Untracked => "Untracked",
        }
    }
}

/// One changed path: S2-B's `ChangedFile` (and a status entry for the uncommitted row).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileRow {
    /// The change letter: `A` `M` `D` `R` `T` `C` `U` or `?` (03 §6.1).
    pub letter: char,
    /// The path (a rename's new one); an untracked folder's ends in `/`.
    pub path: String,
    /// A rename's old path, shown `old → new`.
    pub old_path: Option<String>,
    /// `+adds -dels` for text; `None` for a binary file (blank).
    pub lines: Option<(u64, u64)>,
    /// `path` or `old_path` is not valid UTF-8: it shows with U+FFFD and opens nothing.
    pub lossy: bool,
}

impl FileRow {
    /// An untracked folder's row: → or Enter lists its files (03 §6.1).
    pub fn is_folder(&self) -> bool {
        self.path.ends_with('/')
    }

    /// The path as its row shows it: `old → new` for a rename.
    pub fn shown(&self) -> String {
        match &self.old_path {
            Some(old) => format!("{old} → {}", self.path),
            None => self.path.clone(),
        }
    }
}

/// One line of the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Line {
    /// A summary field: its name, dim, and its value.
    Field {
        name: &'static str,
        value: String,
    },
    /// A parent's hash (`Parents` on the first): Enter puts the graph's bar on it when loaded.
    Parent {
        first: bool,
        oid: Oid,
    },
    /// A line of the message.
    Text(String),
    Blank,
    /// A group's head.
    Head(&'static str),
    /// A file row: its group and its place in it.
    File {
        group: usize,
        file: usize,
    },
    /// The read is under way.
    Reading,
    /// The read failed.
    Error(String),
}

/// The value column of the summary's fields, in cells: after `Author date`.
const VALUE: usize = 13;

/// The open details pane: what it shows and its keyboard.
#[derive(Debug, Clone)]
pub(super) struct Pane {
    pub subject: Subject,
    /// `None` while the read runs.
    pub details: Option<Details>,
    /// The pane's list has the keyboard; else it follows the graph's bar (03 §6.1).
    pub keyboard: bool,
    pub scroll: Scroll,
    /// A comparison's `From` and `To` (03 §6.4), as the rows say them.
    header: Vec<(&'static str, String)>,
    pub lines: Vec<Line>,
    /// The bar went to the first file once the details came.
    placed: bool,
    /// The width, in cells, the message's lines were wrapped to (`None`: not yet, for these
    /// lines).
    wrapped_for: Option<usize>,
    /// For each line, the line of the unwrapped list it is part of.
    origin: Vec<usize>,
}

/// Where the pane's bar is, in terms that survive the lines being made again (details delivered
/// again, the message wrapped to another width).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spot {
    /// On a file row: its group and path.
    File(Group, String),
    /// On another line: the unwrapped line, and the row of it when it wrapped.
    Line(usize, usize),
}

impl Pane {
    /// A pane for `subject`, its read under way; `header` its first lines (a comparison's ends).
    pub fn new(subject: Subject, keyboard: bool, header: Vec<(&'static str, String)>) -> Pane {
        let mut pane = Pane {
            subject,
            details: None,
            keyboard,
            scroll: Scroll::default(),
            header,
            lines: Vec::new(),
            placed: false,
            wrapped_for: None,
            origin: Vec::new(),
        };
        pane.build();
        pane
    }

    /// `details`, when they are for what the pane shows; whether they were taken.
    pub fn set(&mut self, details: Details) -> bool {
        if details.subject != self.subject {
            return false;
        }
        // The same commit delivered again (`In` answered, a refresh) keeps the bar where it is;
        // the first delivery, or another commit, puts it on the first file.
        let oid = |d: &Option<Details>| {
            d.as_ref()
                .and_then(|d| d.summary.as_ref().map(|s| s.oid.clone()))
        };
        let same = std::mem::replace(&mut self.placed, true)
            && oid(&self.details) == details.summary.as_ref().map(|s| s.oid.clone());
        let spot = same.then(|| self.spot());
        self.details = Some(details);
        self.build();
        match spot {
            Some(spot) => self.restore(&spot),
            None => {
                let first_file = self
                    .lines
                    .iter()
                    .position(|l| matches!(l, Line::File { .. }));
                self.scroll.to(first_file.unwrap_or(0));
            }
        }
        true
    }

    /// Where the bar is now.
    fn spot(&self) -> Spot {
        let bar = self.scroll.bar;
        if let Some(Line::File { group, file }) = self.line() {
            if let Some((g, f)) = self.file(*group, *file) {
                return Spot::File(g, f.path.clone());
            }
        }
        let origin = self.origin.get(bar).copied().unwrap_or(bar);
        let first = self.origin.iter().position(|&o| o == origin).unwrap_or(bar);
        Spot::Line(origin, bar.saturating_sub(first))
    }

    /// The bar back on `spot` in the lines as they are now; a file no longer there leaves it on
    /// the same line number.
    fn restore(&mut self, spot: &Spot) {
        let last = self.lines.len().saturating_sub(1);
        let at = match spot {
            Spot::File(group, path) => self.lines.iter().position(|l| match l {
                Line::File { group: g, file: f } => self
                    .file(*g, *f)
                    .is_some_and(|(kind, row)| kind == *group && row.path == *path),
                _ => false,
            }),
            Spot::Line(origin, row) => self.origin.iter().position(|o| o == origin).map(|first| {
                let rows = self.origin[first..]
                    .iter()
                    .take_while(|o| *o == origin)
                    .count();
                first + (*row).min(rows.saturating_sub(1))
            }),
        };
        let fallback = match spot {
            Spot::Line(origin, _) => *origin,
            Spot::File(..) => self.scroll.bar,
        };
        self.scroll.bar = at.unwrap_or(fallback).min(last);
        self.scroll.moved = true;
    }

    /// The lines from the details, the message's not wrapped yet.
    fn build(&mut self) {
        self.wrapped_for = None;
        self.build_lines();
        self.origin = (0..self.lines.len()).collect();
    }

    /// The lines from the details, unwrapped.
    fn build_lines(&mut self) {
        let mut lines: Vec<Line> = self
            .header
            .iter()
            .map(|(name, value)| Line::Field {
                name,
                value: value.clone(),
            })
            .collect();
        let Some(d) = &self.details else {
            lines.push(Line::Reading);
            self.lines = lines;
            return;
        };
        if let Some(s) = &d.summary {
            let field = |name, value: String| Line::Field { name, value };
            lines.push(field("Commit", s.oid.as_str().to_string()));
            if s.parents.is_empty() {
                lines.push(field("Parents", "none (a root commit)".to_string()));
            }
            lines.extend(s.parents.iter().enumerate().map(|(k, p)| Line::Parent {
                first: k == 0,
                oid: p.clone(),
            }));
            lines.push(field(
                "Author",
                format!("{} <{}>", s.author_name, s.author_email),
            ));
            lines.push(field("Author date", date_text(s.author_time)));
            if (&s.committer_name, &s.committer_email) != (&s.author_name, &s.author_email) {
                lines.push(field(
                    "Committer",
                    format!("{} <{}>", s.committer_name, s.committer_email),
                ));
            }
            if s.commit_time != s.author_time {
                lines.push(field("Commit date", date_text(s.commit_time)));
            }
            let within = match &d.contains {
                None => "…".to_string(),
                Some(names) if names.is_empty() => "none".to_string(),
                Some(names) => names.join(", "),
            };
            lines.push(field("In", within));
            lines.push(Line::Blank);
            lines.extend(
                s.message
                    .trim_end()
                    .lines()
                    .map(|l| Line::Text(l.to_string())),
            );
            lines.push(Line::Blank);
        }
        if let Some(e) = &d.error {
            lines.push(Line::Error(e.clone()));
        }
        for (g, group) in d.groups.iter().enumerate() {
            lines.push(Line::Head(group.group.head()));
            lines.extend((0..group.files.len()).map(|f| Line::File { group: g, file: f }));
        }
        self.lines = lines;
    }

    /// The message's lines wrapped to `chars` cells (03 §6.1: the whole message, scrollable),
    /// once a width: the bar stays on its file row.
    pub fn wrap(&mut self, chars: usize) {
        if self.wrapped_for == Some(chars) {
            return;
        }
        let spot = self.spot();
        self.build();
        let mut wrapped = Vec::with_capacity(self.lines.len());
        let mut origin = Vec::with_capacity(self.lines.len());
        for (k, line) in std::mem::take(&mut self.lines).into_iter().enumerate() {
            match line {
                Line::Text(s) => {
                    for row in wrap_line(&s, chars) {
                        wrapped.push(Line::Text(row));
                        origin.push(k);
                    }
                }
                other => {
                    wrapped.push(other);
                    origin.push(k);
                }
            }
        }
        self.lines = wrapped;
        self.origin = origin;
        self.wrapped_for = Some(chars);
        self.restore(&spot);
    }

    /// The line under the pane's bar.
    pub fn line(&self) -> Option<&Line> {
        self.lines.get(self.scroll.bar)
    }

    /// The file row of a [`Line::File`].
    pub fn file(&self, group: usize, file: usize) -> Option<(Group, &FileRow)> {
        let g = self.details.as_ref()?.groups.get(group)?;
        Some((g.group, g.files.get(file)?))
    }
}

/// Whether [`draw`] paints the pane's cells (10 §17, X4-graph's seam): it does (X4-pane), so the
/// panel leaves none of them to egui.
pub(super) const PAINTS_CELLS: bool = true;

/// One of the pane's own glyphs as this pass's screen shows it: in a terminal that draws a glyph
/// of ambiguous width in two cells (`wide`), 10 §10's stand-in (`term::narrow`: `√` `.`, `→`
/// `>`), put here where the glyph is known to be the pane's, never a path's (T38).
fn own(glyph: char, wide: bool) -> String {
    let mut utf8 = [0u8; 4];
    let s: &str = glyph.encode_utf8(&mut utf8);
    match term::narrow(s).filter(|_| wide) {
        Some(stand_in) => stand_in.to_string(),
        None => s.to_string(),
    }
}

/// Between a rename's old and new path (`old → new`, 03 §6.1).
const RENAMED: char = '→';

/// The pane at cells `area` of `grid` (the panel hands it the rows under the graph's rule),
/// written into `buf`, the panel's buffer (10 §4, X4-pane): a line a row — a summary field's name
/// dim at its first cell and its value at the fourteenth, a group's head dim, a file's `√` (or an
/// untracked folder's `►`), its change letter, its path (a rename `old → new`) cut from its start
/// so that its end shows, and `+adds -dels` at the right; the message's lines as [`Pane::wrap`]
/// wrapped them — every text cut by cells, never half a wide glyph. The pane's bar (while the
/// keyboard is the pane's) in the active bar's colours when `active`, else the idle bar's; the
/// scroll bar on the border right of `area`, as the graph's. `viewed` says which files were
/// opened (`√`). The wheel scrolls the lines by cells; each line is an `interact` rect at its
/// cells: returns the line clicked, and whether the click is the second of a double click on it
/// (the browser's `second_click`: egui alone misses a double click right after a click on another
/// line).
pub(super) fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    area: Cells,
    buf: &mut Buffer,
    pane: &mut Pane,
    viewed: &dyn Fn(&FileRow) -> bool,
    active: bool,
) -> Option<(usize, bool)> {
    let ctx = ui.ctx().clone();
    let p = chrome::PALETTE;
    let wide = cells::ambiguous_wide(&ctx);
    let chars = usize::from(area.width);
    let visible = usize::from(area.height).max(1);
    pane.wrap(chars);
    let len = pane.lines.len();
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
        pane.scroll.visible = visible;
        pane.scroll.wheel(lines, len);
    }
    pane.scroll.settle(visible, len);
    let range = pane.scroll.range(len);
    let shown = range.start..range.end.min(range.start + usize::from(area.height));
    // Cell `c` of the pane, on the grid (every `c` here is below `chars`).
    let x = |c: usize| area.x.saturating_add(u16::try_from(c).unwrap_or(u16::MAX));
    let pane = &*pane;
    let mut clicked = None;
    for (k, line) in pane
        .lines
        .iter()
        .enumerate()
        .skip(shown.start)
        .take(shown.len())
    {
        let y = area.y + (k - shown.start) as u16;
        let row = Cells::new(area.x, y, area.width, 1);
        let on_bar = pane.keyboard && k == pane.scroll.bar;
        let back = match (on_bar, active) {
            (false, _) => p.back,
            (true, true) => p.bar,
            (true, false) => p.bar_idle,
        };
        let pen = |c: Color32| Pen::new(if on_bar && active { LOOK.on_bar } else { c }, back);
        cells::fill(buf, row, pen(LOOK.text));
        // `s` from cell `at`, cut by cells to the pane's end.
        let text = |buf: &mut Buffer, at: usize, s: &str, c: Color32| {
            let room = chars.saturating_sub(at);
            if room > 0 {
                cells::put(buf, x(at), y, &fitted(s, room), room, pen(c));
            }
        };
        match line {
            Line::Field { name, value } => {
                text(buf, 0, name, LOOK.dim);
                text(buf, VALUE, value, LOOK.text);
            }
            Line::Parent { first, oid } => {
                if *first {
                    text(buf, 0, "Parents", LOOK.dim);
                }
                text(buf, VALUE, oid.as_str(), LOOK.text);
            }
            Line::Text(s) => text(buf, 0, s, LOOK.text),
            Line::Blank => {}
            Line::Head(h) => text(buf, 0, h, LOOK.dim),
            Line::Reading => text(buf, 0, "reading…", LOOK.dim),
            Line::Error(e) => text(buf, 0, e, LOOK.error),
            Line::File { group, file } => {
                let Some((_, f)) = pane.file(*group, *file) else {
                    continue;
                };
                // The first cell: an untracked folder's `►`, a text; or the viewed mark `√`,
                // which keeps its colour on the bar as the graph's lanes and labels keep theirs.
                let mark = if f.is_folder() {
                    Some((look::FOLDER.to_string(), pen(LOOK.text)))
                } else if viewed(f) {
                    Some((own(look::VIEWED, wide), Pen::new(LOOK.viewed, back)))
                } else {
                    None
                };
                if let (Some((mark, pen)), true) = (mark, chars > 0) {
                    cells::put(buf, x(0), y, &mark, 1, pen);
                }
                file_row(buf, x, y, chars, f, wide, &pen);
            }
        }
        // Its id salted with the pane's first column: two repositories' panels draw into one
        // `ui` side by side (review round 1), always in two columns (`cells::halves`); never with
        // its row, which moves when the banner's rows change (`Layout::of`), and egui gives a
        // release to the id it pressed, so a click across that change would be lost. It senses
        // clicks alone (`Sense::CLICK`): the pane's keys are the panel's, and egui's focusable
        // `Sense::click()` would make Space or Enter a click.
        let id = ui.scope_id().with(("sopen-git-pane", area.x, k));
        let resp = ui.interact(grid.rect(row), id, Sense::CLICK);
        if resp.clicked() {
            // The line by what it shows — a file by its group and path, as the graph's rows by
            // what they draw, so a refresh that moves the files between two clicks starts a pair
            // of its own (review round 1) — for this subject. No panel in it: egui counts no
            // double click across more than 6 points (`max_click_dist`), and two panels' lines
            // are a side apart.
            let shows = match line {
                Line::File { group, file } => pane.file(*group, *file).map(|(g, f)| (g, &f.path)),
                _ => None,
            };
            let pair = egui::Id::unique(("sopen-git-pane-line", &pane.subject, shows.ok_or(k)));
            clicked = Some((k, super::super::second_click(&resp, pair)));
        }
    }
    // The scroll bar on the border, beside the pane's rows.
    let max_top = len.saturating_sub(visible);
    let at = (max_top > 0).then(|| pane.scroll.top as f32 / max_top as f32);
    if area.height > 0 {
        cells::scroll_bar(buf, area.right(), area.y, area.height, at);
    }
    clicked
}

/// `s` as it fits in `room` cells, its control characters escaped (`printable`): whole, or its
/// longest start before `…` (`cells::fit_start`). Only its start up to the first glyph past the
/// room is escaped and measured, so a pass costs the room, never the whole of a long value (`In`
/// naming every branch and tag that holds the commit).
pub(super) fn fitted(s: &str, room: usize) -> String {
    let mut used = 0;
    let mut end = s.len();
    for (i, g) in s.grapheme_indices(true) {
        walked();
        used += cells::width(&printable(g));
        if used > room {
            end = i + g.len();
            break;
        }
    }
    cells::fit_start(&printable(&s[..end]), room)
}

/// The rest of a file's row at row `y` of a pane `chars` cells wide (`x` its cells on the grid),
/// after its first cell's mark: the change letter at the third cell, the path at the fifth cut
/// from its start (`…`, a path keeps its end), and `+adds -dels` ending at the pane's last cell,
/// a blank between it and the path, where they fit; each in `pen`'s colours. In an
/// ambiguous-wide terminal (`wide`) the rename's `→` is §10's stand-in (T38); the paths are
/// content.
fn file_row(
    buf: &mut Buffer,
    x: impl Fn(usize) -> u16,
    y: u16,
    chars: usize,
    f: &FileRow,
    wide: bool,
    pen: &dyn Fn(Color32) -> Pen,
) {
    if chars > 2 {
        cells::put(buf, x(2), y, &f.letter.to_string(), 1, pen(LOOK.text));
    }
    let counts = f.lines.map(|(a, d)| format!("+{a} -{d}"));
    let counts_w = counts.as_ref().map_or(0, |c| cells::width(c));
    // The counts only where the letter and a blank stand before them.
    let counts = counts.filter(|_| chars >= 4 + counts_w);
    let room = match counts {
        Some(_) => chars.saturating_sub(4 + counts_w + 1),
        None => chars.saturating_sub(4),
    };
    if room > 0 {
        let shown = match &f.old_path {
            Some(old) => format!("{old} {} {}", own(RENAMED, wide), f.path),
            None => f.path.clone(),
        };
        let shown = cells::fit_left(&printable(&shown), room);
        cells::put(buf, x(4), y, &shown, room, pen(LOOK.text));
    }
    if let Some(counts) = counts {
        cells::put(
            buf,
            x(chars - counts_w),
            y,
            &counts,
            counts_w,
            pen(LOOK.text),
        );
    }
}

/// One message line as the pane's rows of `chars` cells: tabs to the next multiple of eight
/// cells, counted as the row shows them, a grapheme at a time — a wide glyph two, an emoji's
/// sequence the cells the buffer lays it in (`👩‍💻`, `❤️` two), an escaped control character its
/// escape's length; other control characters escaped; then wrapped by cells ([`wrap_cells`]).
fn wrap_line(line: &str, chars: usize) -> Vec<String> {
    let mut expanded = String::with_capacity(line.len());
    // The column, counted as the line is read (not measured again per tab).
    let mut column = 0;
    for g in line.graphemes(true) {
        if g == "\t" {
            let n = 8 - column % 8;
            expanded.extend(std::iter::repeat_n(' ', n));
            column += n;
        } else if g.chars().any(char::is_control) {
            // A control character is a grapheme of its own (`\r\n` the one pair): escaped.
            let at = expanded.len();
            expanded.extend(g.chars().flat_map(char::escape_default));
            column += expanded.len() - at;
        } else {
            expanded.push_str(g);
            column += cells::width(g);
        }
    }
    wrap_cells(&expanded, chars)
        .into_iter()
        .map(|row| row.trim_end().to_string())
        .collect()
}

/// `text` in rows of at most `room` cells as `dialog::wrap` breaks it — whole when it fits; else
/// at its last blank that fits (the blank dropped), a word longer than the row after its last
/// whole glyph, a glyph wider than the row alone on a row of its own; after a break, blanks alone
/// make no row — in one walk over the text: `dialog::wrap` measures what is left at each row,
/// which a message of a mebibyte on one line (03 §6.1's bound) makes quadratic.
pub(super) fn wrap_cells(text: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    // `text[start..]` is blank (or empty) once `start` is past its last glyph that is not.
    let end = text.trim_end().len();
    let mut rows = vec![];
    let mut start = 0;
    while rows.is_empty() || start < end {
        let rest = &text[start..];
        let (mut used, mut cut, mut blank, mut over) = (0, 0, None, false);
        for (i, g) in rest.grapheme_indices(true) {
            walked();
            let w = cells::width(g);
            if g == " " && i > 0 {
                blank = Some(i);
            }
            if used + w > room {
                over = true;
                break;
            }
            used += w;
            cut = i + g.len();
        }
        if !over {
            break;
        }
        let (row, next) = match blank {
            Some(b) if b <= cut => (&rest[..b], b + 1),
            // A glyph wider than the row alone: it goes on a row of its own, cut there.
            _ if cut == 0 => {
                let first = rest.graphemes(true).next().map_or(rest.len(), str::len);
                (&rest[..first], first)
            }
            _ => (&rest[..cut], cut),
        };
        rows.push(row.to_string());
        start += next;
    }
    if start < end || rows.is_empty() {
        rows.push(text[start..].to_string());
    }
    rows
}

#[cfg(test)]
thread_local! {
    /// The graphemes [`wrap_cells`] and [`fitted`] have measured on this thread: their work,
    /// which the tests bound by the text's length and by the room.
    pub(super) static WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts a grapheme measured ([`WALKED`], in the tests only).
fn walked() {
    #[cfg(test)]
    WALKED.with(|c| c.set(c.get() + 1));
}
