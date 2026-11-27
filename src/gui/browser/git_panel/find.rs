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
//!
//! **In cells** (X4-find, 10 §17). Find's field stands in the heads' row: the word `Find` dim,
//! `ops::dialog`'s field — egui's `TextEdit` as the engine only, typing, IME, the clipboard, at
//! the field's cells, which show what it holds — and the `Aa` toggle at the right, `[ ] Aa` or
//! `[X] Aa` (a check box's mark, 10 §6), a click turning it; what it does is said in
//! [`FIND_HINT`], since a cell has no hover. Its ids carry the panel's column, never the ui's
//! scope: two panels stand side by side in one `ui`, and a panel too short for its parts runs the
//! field in a child of its own (`mod.rs`), whose scope differs. The history search's dialog is
//! the kit's box on the panels' grid ([`SearchDialog`]): its six fields, `[ ] Match case`,
//! `[ Search ]  [ Cancel ]` and a note; Enter searches for what the pass before painted.

use super::graph_view::row_labels;
use super::{Label, Paging, Scroll};
use crate::git::graph::Row;
use crate::git::model::Oid;
use crate::gui::browser::keys::{self, Mods};
use crate::gui::browser::ops::dialog::{self as kit, Button, Ink, Input, Inputs, Part};
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen, Position};
use crate::gui::chrome::{self, git::LOOK};
use eframe::egui::{self, Key, Sense};
use std::collections::HashMap;

/// What the status row says while Find is open: its keys, and what the `Aa` toggle does (X4-find's
/// decision, 10 §17: a cell has no hover, and the heads' row keeps its cells for the field — on a
/// 24-cell column too). Last, so a short status row cuts it before Esc.
pub(super) const FIND_HINT: &str =
    "Enter next · Shift+Enter previous · Esc closes Find · Aa matches case";

/// Whether [`field`] paints Find's cells (10 §17, X4-graph's seam): it does (X4-find) — the word,
/// the field and the toggle are the panel's cells.
pub(super) const PAINTS_CELLS: bool = true;

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

/// The id of Find's field in the panel whose heads' row starts at column `column`: two panels
/// side by side in one `ui` each have their own, and it stays the same in the short panel's
/// child `ui` (whose scope differs), so the field's caret and composition survive the panel
/// going short and back. A width that moves the panel's column (the right side's) gives it a
/// new id, and its caret starts again.
fn find_id(column: u16) -> egui::Id {
    egui::Id::unique(("sopen-git-find", column))
}

/// The `Aa` toggle's id in the panel at `column` (as [`find_id`]).
fn case_id(column: u16) -> egui::Id {
    egui::Id::unique(("sopen-git-find-case", column))
}

/// The word before the field, dim, and the blank after it.
const WORD: &str = "Find";
const WORD_CELLS: u16 = 5;

/// The toggle as it reads off and on: a check box's mark (10 §6), never a colour.
const TOGGLE_OFF: &str = "[ ] Aa";
const TOGGLE_ON: &str = "[X] Aa";

/// The toggle's cells and the blank before it.
const TOGGLE_CELLS: u16 = 7;

/// The fewest cells the field keeps while the word or the toggle still stands beside it.
const FIELD_FEWEST: u16 = 8;

/// Where Find's parts stand in the heads' row `row`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Placed {
    /// `Find`, where there is room for it.
    pub word: Option<Cells>,
    pub field: Cells,
    /// `[ ] Aa`, where there is room for it.
    pub toggle: Option<Cells>,
}

impl Placed {
    /// The word and a blank at the left, the field, a blank and the toggle at the right. A
    /// narrow row gives the word up first and then the toggle, the field keeping
    /// [`FIELD_FEWEST`] cells while either stands; at the last the field is the whole row.
    pub fn of(row: Cells) -> Placed {
        let w = row.width;
        let word = w >= WORD_CELLS + FIELD_FEWEST + TOGGLE_CELLS;
        let toggle = w >= FIELD_FEWEST + TOGGLE_CELLS;
        let left = if word { WORD_CELLS } else { 0 };
        let right = if toggle { TOGGLE_CELLS } else { 0 };
        let one = |x: u16, width: u16| Cells::new(x, row.y, width, row.height.min(1));
        Placed {
            word: word.then(|| one(row.x, WORD_CELLS - 1)),
            field: one(row.x + left, w.saturating_sub(left + right)),
            toggle: toggle.then(|| one(row.right() - (TOGGLE_CELLS - 1), TOGGLE_CELLS - 1)),
        }
    }
}

/// Find's field at cells `cells` of `grid` (the heads' row), into `buf` (the panel's): `Find`
/// dim, the field — `ops::dialog`'s, black cells, its text in white, its selection in the bar's
/// colours — and the `Aa` toggle, which a click turns (pointer only: `Sense::CLICK`, so no Tab and
/// Space presses it). It has the typing while it is open (`typing`: no dialog over it): the focus
/// is asked for before its engine is laid out, so the typing of this pass goes there, in order (a
/// typed letter's rest, an input method's composition, several letters in one pass), and it
/// keeps the keyboard through Tab (`lock_focus`). On no cells (a panel too short for its parts)
/// only the engine runs, so typing still lands. The caret's cell, while it has the keyboard and
/// the key help does not cover it, for the panel's paint (in a terminal, where its cursor goes).
pub(super) fn field(
    ui: &mut egui::Ui,
    grid: &Grid,
    cells: Cells,
    buf: &mut Buffer,
    find: &mut Find,
    typing: bool,
) -> Option<Position> {
    let ctx = ui.ctx().clone();
    let id = find_id(cells.x);
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
    let at = Placed::of(cells);
    // The row's own rect under the field and the toggle, laid first so both stand over it: egui
    // gives a click a few points off a widget to it when nothing is under the pointer, so a
    // click on the blank before the toggle, or on `Find`, would turn the toggle.
    if !cells.is_empty() {
        let row = egui::Id::unique(("sopen-git-find-row", cells.x));
        ui.interact(grid.rect(cells), row, Sense::CLICK);
    }
    kit::field(ui, grid, at.field, id, &mut find.text, |e| {
        e.lock_focus(true)
    });
    if cells.is_empty() {
        return None;
    }
    let p = chrome::PALETTE;
    if let Some(word) = at.word {
        let dim = Pen::new(LOOK.dim, p.back);
        cells::put(buf, word.x, word.y, WORD, word.width.into(), dim);
    }
    if let Some(toggle) = at.toggle {
        let turned = ui.interact(grid.rect(toggle), case_id(cells.x), Sense::CLICK);
        if turned.clicked() {
            find.case = !find.case;
        }
        let shown = if find.case { TOGGLE_ON } else { TOGGLE_OFF };
        let pen = Pen::new(p.text, p.back);
        cells::put(buf, toggle.x, toggle.y, shown, toggle.width.into(), pen);
    }
    let focused = ctx.memory(|m| m.has_focus(id));
    let caret = kit::field_cells(&ctx, buf, at.field, id, &find.text, focused);
    let help = crate::gui::help::covered(&ctx, grid);
    caret.filter(|c| !help.iter().any(|r| r.contains(*c)))
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

/// The history search's dialog, in cells (X4-find): the kit's box on the panels' grid, titled
/// `History search` — its six fields under labels of one width, `[ ] Match case`, a blank row,
/// `[ Search ]  [ Cancel ]` and a note row. The keyboard opens on Message and goes back there
/// while none of its controls has it; Tab goes through them in that order. Enter searches, but
/// each Enter of a pass goes to what has the keyboard at that point — on Cancel it cancels — and
/// one after a Shift+Tab or an arrow that moves the keyboard in that pass (egui moves it later)
/// is dropped, a Space too ([`take_enters`]). Only what the screen showed runs: an Enter or a
/// Search runs the query the pass before painted, unchanged in this one — not in the pass that
/// lays the box out unseen, nor the first one seen — else the note says the search changed and
/// the next Enter runs it. An empty query is refused there (`type what to search for`). Esc,
/// Cancel or a click outside cancels; a pass that both cancels and searches cancels. What is
/// typed in the pass that opens it lands in Message, in order (its field reads that pass too).
#[derive(Debug, Clone, Default)]
pub(super) struct SearchDialog {
    pub query: SearchQuery,
    /// What the note row says.
    note: Note,
    /// The query the last pass painted where it could be seen (`None` until one has).
    shown: Option<SearchQuery>,
}

/// What the history search's note row says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Note {
    /// Enter and Esc (weak).
    #[default]
    Hint,
    /// Enter with every field empty (the error colour).
    Empty,
    /// An Enter or a Search came in a pass that changed what is shown: none ran.
    Changed,
}

impl Note {
    fn text(self) -> (&'static str, Ink) {
        match self {
            Note::Hint => ("Enter searches · Esc cancels", Ink::Weak),
            Note::Empty => ("type what to search for", Ink::Error),
            Note::Changed => (SEARCH_CHANGED, Ink::Text),
        }
    }
}

/// The note once an Enter or a Search came with a change: only a search on screen runs.
const SEARCH_CHANGED: &str = "the search changed: Enter searches for it · Esc cancels";

/// The history search's box (its modal) and the ids of its controls.
fn search_id() -> egui::Id {
    egui::Id::unique("sopen-git-search")
}

fn search_field_id(k: usize) -> egui::Id {
    search_id().with(("field", k))
}

fn match_case_id() -> egui::Id {
    search_id().with("case")
}

fn search_button_id() -> egui::Id {
    search_id().with("search")
}

pub(super) fn cancel_button_id() -> egui::Id {
    search_id().with("cancel")
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

/// The search box's nominal width inside, in cells: its widest note, so the box keeps its width
/// whichever note shows (on a narrower grid each is cut to the row).
const SEARCH_NOMINAL: usize = 56;

impl SearchDialog {
    /// The box on `grid` as the dialog stands: the fields under labels padded to the widest, so
    /// they line up; the note cut to one row, so the box is the same whichever note it holds.
    fn layout(&self, grid: Grid) -> kit::Layout {
        let title = "History search";
        let wide = (SEARCH_FIELDS.iter().map(|n| cells::width(n)).max()).unwrap_or(0);
        let mut parts: Vec<Part> = (SEARCH_FIELDS.iter().enumerate())
            .map(|(k, name)| Part::Field {
                label: format!("{name:<wide$}"),
                id: search_field_id(k),
            })
            .collect();
        let width = kit::inside_width(grid.cols, SEARCH_NOMINAL, SEARCH_NOMINAL);
        let (note, ink) = self.note.text();
        parts.extend([
            Part::Check {
                label: "Match case".to_string(),
                id: match_case_id(),
            },
            Part::Blank,
            Part::Buttons(vec![
                Button::new("Search", search_button_id()),
                Button::new("Cancel", cancel_button_id()),
            ]),
            Part::Text(cells::fit_start(note, width), ink),
        ]);
        kit::layout(grid, title, SEARCH_NOMINAL, &parts)
    }

    /// The cells of `grid` (the panels') the dialog covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam), as the dialog stands before its
    /// pass — the pass lays the same box.
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        self.layout(*grid).covers()
    }

    /// One pass on `grid` (the panels'), see [`SearchDialog`]: Esc and each Enter are taken
    /// before the box's controls see them.
    pub fn ui(&mut self, ctx: &egui::Context, grid: Grid) -> Option<SearchEnd> {
        let escape = ctx.input_mut(|i| keys::take(i, Key::Escape, Mods::None));
        let controls: Vec<egui::Id> = (0..SEARCH_FIELDS.len())
            .map(search_field_id)
            .chain([match_case_id(), search_button_id(), cancel_button_id()])
            .collect();
        let began = ctx.memory(|m| m.focused());
        let in_field = (0..SEARCH_FIELDS.len()).any(|k| began == Some(search_field_id(k)));
        let enters = take_enters(ctx, began.is_some() && !in_field);
        // egui lays a box it has no size for out unseen first (`Area`'s sizing pass: one it has
        // not shown before, or restored without its size): what that pass paints, nobody saw.
        let unseen = egui::AreaState::load(ctx, search_id()).is_none_or(|a| a.size.is_none());
        if !began.is_some_and(|f| controls.contains(&f)) {
            ctx.memory_mut(|m| m.request_focus(search_field_id(0)));
        }
        let layout = self.layout(grid);
        let before = self.query.clone();
        let seen = self.shown.take();
        let q = &mut self.query;
        let texts = [
            &mut q.message,
            &mut q.author,
            &mut q.text,
            &mut q.path,
            &mut q.since,
            &mut q.until,
        ];
        let mut by_id: Vec<(egui::Id, Input)> = (texts.into_iter().enumerate())
            .map(|(k, text)| (search_field_id(k), Input::Text(text)))
            .collect();
        by_id.push((match_case_id(), Input::Check(&mut q.case)));
        // The buttons take the kit's presses, in the pass's order (F-kit-tab).
        by_id.extend([search_button_id(), cancel_button_id()].map(|b| (b, Input::Button)));
        let inputs = Inputs {
            by_id,
            ..Inputs::default()
        };
        let pass = kit::show(ctx, search_id(), &layout, inputs);
        let now = ctx.memory(|m| m.focused());
        // What this pass painted, where it could be seen: the next pass's Enter runs it.
        self.shown = (!unseen).then(|| self.query.clone());
        let mut cancel = escape || pass.clicked(cancel_button_id()) || pass.modal.should_close();
        let mut run = pass.clicked(search_button_id());
        for after_tab in enters {
            match if after_tab { now } else { began } {
                Some(id) if id == cancel_button_id() => cancel = true,
                _ => run = true,
            }
        }
        if cancel {
            return Some(SearchEnd::Cancel);
        }
        let changed = self.query != before;
        if !run {
            if changed {
                self.note = Note::Hint;
            }
            return None;
        }
        // Only what the screen showed runs: the query the pass before painted, unchanged here.
        if changed || seen.as_ref() != Some(&self.query) {
            if seen.is_some() {
                self.note = Note::Changed;
            }
            ctx.request_repaint();
            return None;
        }
        if self.query.is_empty() {
            self.note = Note::Empty;
            return None;
        }
        Some(SearchEnd::Run(self.query.clone()))
    }
}

/// The pass's presses of Enter with no modifiers, taken out of its input in order, each saying
/// whether it came after the pass's first move of the keyboard (a Tab, or an arrow where
/// `arrows_move`: on a control whose arrows egui takes as a move, not a field's caret) — so no
/// button, check box or field the keyboard is on reads it as its own: the dialog gives each to
/// what has the keyboard then. egui moves the keyboard for a Shift+Tab only on the next pass and
/// for an arrow at the pass's end, where an Enter after it in the pass cannot follow: in a pass
/// with one, an Enter after a move is taken and dropped, and the owner presses it again on what
/// now shows the keyboard — a Space too, with its text, which the button or check box the
/// keyboard is leaving would take. (As X4-dialogs' action dialogs read them.)
fn take_enters(ctx: &egui::Context, arrows_move: bool) -> Vec<bool> {
    let arrow = |key: &Key| {
        matches!(
            key,
            Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight
        )
    };
    // Whether a key moves the keyboard as egui reads it (`None`: not), and whether egui makes
    // that move only later (`true`: a Shift+Tab, an arrow) rather than in the pass (a Tab).
    let moves = |e: &egui::Event| match e {
        egui::Event::Key {
            key: Key::Tab,
            pressed: true,
            modifiers,
            ..
        } if !modifiers.any() || modifiers.shift_only() => Some(modifiers.shift),
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } if arrows_move && arrow(key) && !modifiers.any() => Some(true),
        _ => None,
    };
    ctx.input_mut(|i| {
        let later = i.events.iter().any(|e| moves(e) == Some(true));
        let (mut moved, mut out, mut typed) = (false, Vec::new(), false);
        i.events.retain(|e| match (e, std::mem::take(&mut typed)) {
            (egui::Event::Text(text), true) if text == " " => false,
            (
                egui::Event::Key {
                    key: Key::Enter,
                    pressed: true,
                    modifiers,
                    ..
                },
                _,
            ) if Mods::None.matches(*modifiers) => {
                if !(moved && later) {
                    out.push(moved);
                }
                false
            }
            (
                egui::Event::Key {
                    key: Key::Space,
                    pressed: true,
                    modifiers,
                    ..
                },
                _,
            ) if moved && later && Mods::None.matches(*modifiers) => {
                typed = true;
                false
            }
            (e, _) => {
                moved |= moves(e).is_some();
                true
            }
        });
        out
    })
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
