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

use super::graph_view::{date_text, printable};
use super::{Scroll, Subject};
use crate::git::model::Oid;
use crate::gui::chrome;
use crate::gui::chrome::git::{self as look, LOOK};
use eframe::egui::{self, Pos2, Rect, Sense, Vec2};

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
        };
        pane.build();
        pane
    }

    /// `details`, when they are for what the pane shows; whether they were taken.
    pub fn set(&mut self, details: Details) -> bool {
        if details.subject != self.subject {
            return false;
        }
        self.details = Some(details);
        self.build();
        if !std::mem::replace(&mut self.placed, true) {
            let first_file = self
                .lines
                .iter()
                .position(|l| matches!(l, Line::File { .. }));
            self.scroll.to(first_file.unwrap_or(0));
        } else {
            self.scroll.bar = self.scroll.bar.min(self.lines.len().saturating_sub(1));
        }
        true
    }

    /// The lines from the details, the message's not wrapped yet.
    fn build(&mut self) {
        self.wrapped_for = None;
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

    /// The message's lines wrapped to `chars` cells of `font` (03 §6.1: the whole message,
    /// scrollable), once a width: the bar stays on its file row.
    pub fn wrap(&mut self, ctx: &egui::Context, font: &egui::FontId, cw: f32, chars: usize) {
        if self.wrapped_for == Some(chars) {
            return;
        }
        let bar_file = match self.line() {
            Some(Line::File { group, file }) => Some((*group, *file)),
            _ => None,
        };
        self.build();
        let width = cw * chars.max(1) as f32;
        let mut wrapped = Vec::with_capacity(self.lines.len());
        for line in std::mem::take(&mut self.lines) {
            match line {
                Line::Text(s) => {
                    wrapped.extend(wrap_line(ctx, font, &s, width).into_iter().map(Line::Text))
                }
                other => wrapped.push(other),
            }
        }
        self.lines = wrapped;
        self.wrapped_for = Some(chars);
        let at = bar_file.and_then(|(g, f)| {
            self.lines
                .iter()
                .position(|l| *l == Line::File { group: g, file: f })
        });
        self.scroll.bar = at.unwrap_or(self.scroll.bar.min(self.lines.len().saturating_sub(1)));
        self.scroll.moved = true;
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

/// The pane in `area`, the scroll bar on the right border beside it; `viewed` says which files
/// were opened (`√`), `active` whether the pane's bar is the active one. A click puts the pane's
/// bar on a line and gives it the keyboard; returns the line clicked and whether twice.
pub(super) fn draw(
    ui: &mut egui::Ui,
    area: Rect,
    pane: &mut Pane,
    viewed: &dyn Fn(&FileRow) -> bool,
    active: bool,
) -> Option<(usize, bool)> {
    let ctx = ui.ctx().clone();
    let font = chrome::mono(ui);
    let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = text_h.ceil();
    let visible = ((area.height() / row_h).floor() as usize).max(1);
    let chars = (area.width() / cw).floor().max(0.0) as usize;
    pane.wrap(&ctx, &font, cw, chars);
    let len = pane.lines.len();
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
        pane.scroll.visible = visible;
        pane.scroll.wheel(lines, len);
    }
    pane.scroll.settle(visible, len);
    let range = pane.scroll.range(len);
    let painter = ui.painter_at(area);
    let x = |cell: usize| area.left() + cw * cell as f32;
    let mut clicked = None;
    for (k, line) in pane
        .lines
        .iter()
        .enumerate()
        .skip(range.start)
        .take(range.len())
    {
        let y = area.top() + row_h * (k - range.start) as f32;
        let row = Rect::from_min_size(Pos2::new(area.left(), y), Vec2::new(area.width(), row_h));
        let on_bar = pane.keyboard && k == pane.scroll.bar;
        // Everything of the line is painted inside it.
        let painter = painter.with_clip_rect(row.intersect(area));
        if on_bar {
            chrome::bar(&painter, row, active);
        }
        let paint = |c| if on_bar && active { LOOK.on_bar } else { c };
        let text = |cell: usize, s: &str, c| {
            let room = cw * chars.saturating_sub(cell) as f32;
            let shown = chrome::fit_start(&ctx, &printable(s), &font, room);
            painter.text(
                Pos2::new(x(cell), y),
                egui::Align2::LEFT_TOP,
                shown,
                font.clone(),
                paint(c),
            );
        };
        match line {
            Line::Field { name, value } => {
                text(0, name, LOOK.dim);
                text(VALUE, value, LOOK.text);
            }
            Line::Parent { first, oid } => {
                if *first {
                    text(0, "Parents", LOOK.dim);
                }
                text(VALUE, oid.as_str(), LOOK.text);
            }
            Line::Text(s) => text(0, s, LOOK.text),
            Line::Blank => {}
            Line::Head(h) => text(0, h, LOOK.dim),
            Line::Reading => text(0, "reading…", LOOK.dim),
            Line::Error(e) => text(0, e, LOOK.error),
            Line::File { group, file } => {
                let Some((_, f)) = pane.file(*group, *file) else {
                    continue;
                };
                if f.is_folder() {
                    text(0, &look::FOLDER.to_string(), LOOK.text);
                } else if viewed(f) {
                    text(0, &look::VIEWED.to_string(), LOOK.viewed);
                }
                text(2, &f.letter.to_string(), LOOK.text);
                let counts = f.lines.map(|(a, d)| format!("+{a} -{d}"));
                let counts_w = counts.as_ref().map_or(0, |c| c.chars().count() + 1);
                let room = cw * chars.saturating_sub(4 + counts_w) as f32;
                let path = chrome::fit_left(&ctx, &printable(&f.shown()), &font, room);
                painter.text(
                    Pos2::new(x(4), y),
                    egui::Align2::LEFT_TOP,
                    path,
                    font.clone(),
                    paint(LOOK.text),
                );
                if let Some(counts) = counts {
                    painter.text(
                        Pos2::new(area.right(), y),
                        egui::Align2::RIGHT_TOP,
                        counts,
                        font.clone(),
                        paint(LOOK.text),
                    );
                }
            }
        }
        let resp = ui.interact(
            row,
            ui.scope_id().with(("sopen-git-pane", k)),
            Sense::click(),
        );
        if resp.double_clicked() {
            clicked = Some((k, true));
        } else if resp.clicked() {
            clicked = Some((k, false));
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
        (max_top > 0).then(|| pane.scroll.top as f32 / max_top as f32),
    );
    clicked
}

/// One message line as the pane's lines of `width` points: tabs to the next multiple of eight
/// cells, other control characters escaped, then wrapped as egui wraps it (at word boundaries; a
/// word longer than the line is broken).
fn wrap_line(ctx: &egui::Context, font: &egui::FontId, line: &str, width: f32) -> Vec<String> {
    let mut expanded = String::with_capacity(line.len());
    for c in line.chars() {
        if c == '\t' {
            let n = expanded.chars().count();
            expanded.extend(std::iter::repeat_n(' ', 8 - n % 8));
        } else {
            expanded.push(c);
        }
    }
    let text = printable(&expanded).into_owned();
    if text.is_empty() {
        return vec![String::new()];
    }
    let galley = ctx.fonts_mut(|f| f.layout(text, font.clone(), LOOK.text, width));
    galley
        .rows
        .iter()
        .map(|row| {
            let s: String = row.row.glyphs.iter().map(|g| g.chr).collect();
            s.trim_end().to_string()
        })
        .collect()
}
