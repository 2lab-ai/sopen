//! Find over the loaded rows and history search (03 §4.7).
//!
//! - **Find** is a one-line field over the graph (in the column heads' row), as the hex window's
//!   Go to field: a typed letter in the graph starts it with that letter, Cmd/Ctrl+F empty. It
//!   matches the loaded rows' subject, labels, author name and hash prefix — case-insensitive
//!   unless its `Aa` toggle is on — and the bar jumps to the first match from where Find opened as
//!   the text changes; Enter goes to the next match, Shift+Enter to the previous (around the
//!   ends), and the foot says `k of n`. Esc closes it with the bar where it is. Its matches are
//!   worked out when the text or the rows change — a page appended is matched on its own — never
//!   per pass.
//! - **History search** is a dialog (message, author, text added or removed, changed path,
//!   since, until, case) whose run is the integration's ([`super::GitAction::Search`]); its
//!   results are a flat list in place of the graph, paged like it ([`Results`]).

use super::graph_view::row_labels;
use super::{Label, Paging, Scroll};
use crate::git::graph::Row;
use crate::git::model::Oid;
use crate::gui::cells::{Buffer, Cells, Grid, Position};
use crate::gui::chrome;
use eframe::egui::{self, Key, Modifiers};
use std::collections::HashMap;

/// What the status row says while Find is open.
pub(super) const FIND_HINT: &str = "Enter next · Shift+Enter previous · Esc closes Find";

/// Whether [`field`] paints Find's cells (10 §17, X4-graph's seam): not yet — egui draws the field
/// at `grid.rect(cells)` and the panel leaves those cells to `term::compose` until X4-find makes
/// it `true`.
pub(super) const PAINTS_CELLS: bool = false;

/// Find's state while its field is open.
#[derive(Debug, Clone)]
pub(super) struct Find {
    pub text: String,
    /// Case-sensitive (the toggle); case-insensitive by default.
    pub case: bool,
    /// The bar's row when Find opened: as the text changes, the bar goes to the first match
    /// from there.
    origin: usize,
    /// The text and case the matches are for (`None`: not worked out yet).
    matched: Option<(String, bool)>,
    /// The matching rows, in order.
    matches: Vec<usize>,
    /// The rows matched so far: rows appended later are matched on their own.
    scanned: usize,
    /// The field has not been laid out yet: its cursor goes after the text.
    pub fresh: bool,
}

impl Find {
    /// Find, opened with `text` (a typed letter, or nothing) and the bar on row `origin`.
    pub fn new(text: String, origin: usize) -> Find {
        Find {
            text,
            case: false,
            origin,
            matched: None,
            matches: Vec::new(),
            scanned: 0,
            fresh: true,
        }
    }

    /// The rows or their labels were replaced (a load, a refresh, a filter): match them again —
    /// not a change of the text, so the bar stays where it is (03 §5).
    pub fn reset(&mut self) {
        self.matches.clear();
        self.scanned = 0;
    }

    /// Brings the matches up to date with the text and `rows` (`labels`: the label index);
    /// `true` when the text or the case changed, so the bar is to jump.
    pub fn refresh(&mut self, rows: &[Row], labels: &HashMap<Oid, Vec<Label>>) -> bool {
        let wanted = (self.text.clone(), self.case);
        let changed = self.matched.as_ref() != Some(&wanted);
        if changed {
            self.matched = Some(wanted);
            self.matches.clear();
            self.scanned = 0;
        }
        if self.text.is_empty() {
            self.scanned = rows.len();
            return changed;
        }
        let query = if self.case {
            self.text.clone()
        } else {
            self.text.to_lowercase()
        };
        let start = self.scanned.min(rows.len());
        for (i, row) in rows.iter().enumerate().skip(start) {
            if matches(row, &row_labels(row, labels), &query, self.case) {
                self.matches.push(i);
            }
        }
        self.scanned = rows.len();
        changed
    }

    /// The first match at or after the row Find opened on, around the end.
    pub fn first(&self) -> Option<usize> {
        let at = self.matches.partition_point(|&m| m < self.origin);
        self.matches.get(at).or(self.matches.first()).copied()
    }

    /// The match after `bar` (`forward`) or before it, around the ends.
    pub fn step(&self, bar: usize, forward: bool) -> Option<usize> {
        if forward {
            let at = self.matches.partition_point(|&m| m <= bar);
            self.matches.get(at).or(self.matches.first()).copied()
        } else {
            let at = self.matches.partition_point(|&m| m < bar);
            at.checked_sub(1)
                .and_then(|k| self.matches.get(k))
                .or(self.matches.last())
                .copied()
        }
    }

    /// What the foot says: `k of n` with the bar on the `k`th match, `n matches` elsewhere,
    /// `no match`; nothing while the field is empty.
    pub fn foot(&self, bar: usize) -> String {
        if self.text.is_empty() {
            return String::new();
        }
        let n = self.matches.len();
        if n == 0 {
            return "no match".to_string();
        }
        match self.matches.binary_search(&bar) {
            Ok(k) => format!("{} of {n}", k + 1),
            Err(_) => format!("{n} matches"),
        }
    }
}

/// Whether `row` matches `query` (lowercase unless `case`): its subject, labels, author name or
/// hash prefix. The uncommitted row has none of them.
fn matches(row: &Row, labels: &[Label], query: &str, case: bool) -> bool {
    let has = |text: &str| {
        if case {
            text.contains(query)
        } else {
            text.to_lowercase().contains(query)
        }
    };
    let (oid, subject, author) = match row {
        Row::Uncommitted { .. } => return false,
        Row::Stash(s) => (&s.oid, &s.subject, None),
        Row::Commit { commit, .. } => (&commit.oid, &commit.subject, Some(&commit.author_name)),
    };
    oid.as_str().starts_with(query)
        || has(subject)
        || author.is_some_and(|a| has(a))
        || labels.iter().any(|l| has(&l.text))
}

fn find_id() -> egui::Id {
    egui::Id::unique("sopen-git-find")
}

/// Find's field at cells `cells` of `grid` (the heads' row): `Find` and the field, then the `Aa`
/// case toggle. It has the typing while it is open (`typing`: no dialog over it): the focus is
/// asked for before it is laid out, so the typing of this pass goes there (a typed letter's rest
/// included). `buf` is the panel's buffer, for the field's cells once it paints them
/// ([`PAINTS_CELLS`]); the caret's cell is returned then, for the panel's paint (none while egui
/// draws it).
pub(super) fn field(
    ui: &mut egui::Ui,
    grid: &Grid,
    cells: Cells,
    _buf: &mut Buffer,
    find: &mut Find,
    typing: bool,
) -> Option<Position> {
    let rect = grid.rect(cells);
    let cw = chrome::CELL.x;
    let ctx = ui.ctx().clone();
    let id = find_id();
    if std::mem::take(&mut find.fresh) {
        let mut state = egui::text_edit::TextEditState::load(&ctx, id).unwrap_or_default();
        let end = egui::text::CCursor::new(find.text.chars().count());
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(&ctx, id);
    }
    if typing && ctx.memory(|m| m.focused()) != Some(id) {
        ctx.memory_mut(|m| m.request_focus(id));
    }
    let painter = ui.painter_at(rect);
    painter.text(
        rect.left_top(),
        egui::Align2::LEFT_TOP,
        "Find",
        chrome::mono(ui),
        chrome::git::LOOK.dim,
    );
    let toggle_w = 4.0 * cw;
    let field_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 5.0 * cw, rect.top()),
        egui::pos2(
            (rect.right() - toggle_w - cw).max(rect.left() + 6.0 * cw),
            rect.bottom(),
        ),
    );
    ui.put(
        field_rect,
        chrome::field(&mut find.text)
            .id(id)
            .lock_focus(true)
            .margin(egui::Margin::ZERO)
            .desired_width(field_rect.width()),
    );
    let toggle = egui::Rect::from_min_max(
        egui::pos2(rect.right() - toggle_w, rect.top()),
        rect.right_bottom(),
    );
    let aa = ui
        .put(
            toggle,
            egui::Button::selectable(find.case, "Aa").frame_when_inactive(true),
        )
        .on_hover_text("case-sensitive");
    if aa.clicked() {
        find.case = !find.case;
    }
    None
}

/// A history search (03 §4.7): what its dialog asked for. The integration runs `git log` with
/// these and the panel's revisions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// `--grep`: git's regular expression over the message.
    pub message: String,
    /// `--author`.
    pub author: String,
    /// `-S`: text added or removed.
    pub text: String,
    /// `-- <p>` (`--follow` when it is one file).
    pub path: String,
    /// `--since`, `--until`.
    pub since: String,
    pub until: String,
    /// Case as typed; without it `-i`.
    pub case: bool,
}

impl SearchQuery {
    /// Nothing to search for.
    pub fn is_empty(&self) -> bool {
        [
            &self.message,
            &self.author,
            &self.text,
            &self.path,
            &self.since,
            &self.until,
        ]
        .iter()
        .all(|s| s.trim().is_empty())
    }
}

/// How the history search's dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SearchEnd {
    Run(SearchQuery),
    Cancel,
}

/// The history search's dialog.
#[derive(Debug, Clone, Default)]
pub(super) struct SearchDialog {
    pub query: SearchQuery,
    /// Enter with every field empty: said in the dialog.
    error: Option<String>,
}

fn search_id() -> egui::Id {
    egui::Id::unique("sopen-git-search")
}

/// The history search's fields, in the dialog's order.
const SEARCH_FIELDS: [&str; 6] = [
    "Message",
    "Author",
    "Text added or removed",
    "Changed path",
    "Since",
    "Until",
];

impl SearchDialog {
    /// The cells of `grid` (the panels') the dialog covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam): none while egui draws it, until
    /// X4-find.
    pub fn covers(&self, _grid: &Grid) -> Vec<Cells> {
        Vec::new()
    }

    /// One pass on `grid` (the panels', where it stands once it is in cells): Esc and Enter are
    /// taken before its widgets see them.
    pub fn ui(&mut self, ctx: &egui::Context, _grid: Grid) -> Option<SearchEnd> {
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            return Some(SearchEnd::Cancel);
        }
        let mut run = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        // The typing goes to one of its fields: the message's while none of them has it (egui
        // lays a new dialog out unseen once first, so one early request is not enough).
        let names = SEARCH_FIELDS.map(|name| search_id().with(name));
        let focused = ctx.memory(|m| m.focused());
        if !focused.is_some_and(|f| names.contains(&f)) {
            ctx.memory_mut(|m| m.request_focus(names[0]));
        }
        let mut cancel = false;
        let response = chrome::dialog(ctx, search_id(), "History search", |ui| {
            let q = &mut self.query;
            egui::Grid::new(search_id().with("grid"))
                .num_columns(2)
                .show(ui, |ui| {
                    let texts = [
                        &mut q.message,
                        &mut q.author,
                        &mut q.text,
                        &mut q.path,
                        &mut q.since,
                        &mut q.until,
                    ];
                    for ((name, text), id) in SEARCH_FIELDS.iter().zip(texts).zip(names) {
                        ui.label(*name);
                        ui.add(
                            chrome::field(text)
                                .id(id)
                                .desired_width(30.0 * chrome::CELL.x),
                        );
                        ui.end_row();
                    }
                });
            if chrome::choice(ui, q.case, "Match case").clicked() {
                q.case = !q.case;
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.add_space(chrome::CELL.y / 2.0);
            ui.horizontal(|ui| {
                run |= ui.button("Search").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if cancel || response.should_close() {
            return Some(SearchEnd::Cancel);
        }
        if run {
            if self.query.is_empty() {
                self.error = Some("type what to search for".to_string());
                return None;
            }
            return Some(SearchEnd::Run(self.query.clone()));
        }
        None
    }
}

/// A history search's results, shown as a flat list in place of the graph.
#[derive(Debug, Clone, Default)]
pub(super) struct Results {
    pub query: SearchQuery,
    pub rows: Vec<Row>,
    pub paging: Paging,
    pub scroll: Scroll,
    /// [`super::GitAction::MoreResults`] was asked for since the rows or the paging changed.
    pub more_asked: bool,
}

impl Results {
    /// A search under way.
    pub fn new(query: SearchQuery) -> Results {
        Results {
            query,
            ..Default::default()
        }
    }
}
