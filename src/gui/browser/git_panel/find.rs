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
//! [`FIND_HINT`], since a cell has no hover. Its ids are the panel's own key ([`panel_key`]),
//! never its column nor the ui's scope: two panels stand side by side in one `ui`, the right
//! side's column moves with the window's width, and a panel too short for its parts runs the
//! field in a child of its own (`mod.rs`), whose scope differs. It types only in the active panel;
//! its Enter, Shift+Enter and Esc act in the pass's order with what is typed ([`take_leading`],
//! [`take_trailing`]). The history search's dialog is
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

/// A new panel's own key for Find's ids (`GitView::find_key`, given once, when the view is made):
/// not its column, which moves with the window's width (the right side's) and which a panel shown
/// there later would take over; not the `ui`'s scope, which a panel too short for its parts runs
/// the field in a child of. So the field's caret, selection, undo and an input method's
/// composition stay with the panel whatever its cells, and two panels side by side in one `ui`
/// never share them (review round 1).
pub(super) fn panel_key() -> egui::Id {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    egui::Id::unique(("sopen-git-panel", n))
}

/// The id of Find's field in the panel keyed `panel` ([`panel_key`]).
fn find_id(panel: egui::Id) -> egui::Id {
    panel.with("sopen-git-find")
}

/// The `Aa` toggle's id in the panel keyed `panel`.
fn case_id(panel: egui::Id) -> egui::Id {
    panel.with("sopen-git-find-case")
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

/// Find's field at cells `cells` of `grid` (the heads' row), into `buf` (the panel's), its ids the
/// panel's (`panel`, [`panel_key`]): `Find` dim, the field — `ops::dialog`'s, black cells, its
/// text in white, its selection in the bar's colours — and the `Aa` toggle, which a click turns
/// (pointer only: `Sense::CLICK`, so no Tab and Space presses it). It has the typing while its
/// panel is active and no dialog of the panel's is over it (`typing`): the focus is asked for
/// before its engine is laid out, so the typing of this pass goes there, in order (a typed
/// letter's rest, an input method's composition, several letters in one pass), and it keeps the
/// keyboard through Tab (`lock_focus`); otherwise it gives the keyboard up, so a panel without it
/// neither types nor shows a caret (review round 1: two open Finds took every keystroke). A Find
/// opened anew starts afresh, its caret at its end, nothing to undo from before. On no cells (a
/// panel too short for its parts) only the engine runs, so typing still lands. The caret's cell,
/// while it has the keyboard and the key help does not cover it, for the panel's paint (in a
/// terminal, where its cursor goes).
pub(super) fn field(
    ui: &mut egui::Ui,
    grid: &Grid,
    cells: Cells,
    buf: &mut Buffer,
    find: &mut Find,
    typing: bool,
    panel: egui::Id,
) -> Option<Position> {
    let ctx = ui.ctx().clone();
    let id = find_id(panel);
    if std::mem::take(&mut find.fresh) {
        let mut state = egui::text_edit::TextEditState::default();
        let end = egui::text::CCursor::new(find.text.chars().count());
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(&ctx, id);
    }
    let has = ctx.memory(|m| m.has_focus(id));
    if typing && !has {
        ctx.memory_mut(|m| m.request_focus(id));
    } else if !typing && has {
        ctx.memory_mut(|m| m.surrender_focus(id));
    }
    let at = Placed::of(cells);
    // The row's own rect under the field and the toggle, laid first so both stand over it: egui
    // gives a click a few points off a widget to it when nothing is under the pointer, so a
    // click on the blank before the toggle, or on `Find`, would turn the toggle.
    if !cells.is_empty() {
        ui.interact(
            grid.rect(cells),
            panel.with("sopen-git-find-row"),
            Sense::CLICK,
        );
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
        let turned = ui.interact(grid.rect(toggle), case_id(panel), Sense::CLICK);
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

/// One of Find's own keys (03 §4.7): Enter the next match, Shift+Enter the previous, Esc closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FindKey {
    Next,
    Previous,
    Close,
}

/// Which of Find's keys `e` presses, with exactly its modifiers.
fn find_key(e: &egui::Event) -> Option<FindKey> {
    match e {
        egui::Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers,
            ..
        } if Mods::None.matches(*modifiers) => Some(FindKey::Next),
        egui::Event::Key {
            key: Key::Enter,
            pressed: true,
            modifiers,
            ..
        } if Mods::Shift.matches(*modifiers) => Some(FindKey::Previous),
        egui::Event::Key {
            key: Key::Escape,
            pressed: true,
            modifiers,
            ..
        } if Mods::None.matches(*modifiers) => Some(FindKey::Close),
        _ => None,
    }
}

/// Whether `e` is what Find's field reads as typing: text, an input method's, a paste or a cut, a
/// key pressed that is not one of Find's own.
fn typing_event(e: &egui::Event) -> bool {
    match e {
        egui::Event::Text(_)
        | egui::Event::Ime(_)
        | egui::Event::Paste(_)
        | egui::Event::Cut
        | egui::Event::Copy => true,
        egui::Event::Key { pressed: true, .. } => find_key(e).is_none(),
        _ => false,
    }
}

/// Find's keys, in the pass's order with what is typed into it (review round 1): the first of
/// `events` that is one of Find's keys, taken out, when nothing typed comes before it — so it acts
/// before the field types the pass (`keys`). Typed text first: `None`, the rest left to the field
/// and [`take_trailing`].
pub(super) fn take_leading(events: &mut Vec<egui::Event>) -> Option<FindKey> {
    let at = events
        .iter()
        .position(|e| typing_event(e) || find_key(e).is_some())?;
    let key = find_key(&events[at])?;
    events.remove(at);
    Some(key)
}

/// Once an Esc after what was typed closed Find: what the field read before it (still in
/// `events`, its engine leaving them there) goes with Find, and `rest`, what came after the Esc,
/// is the graph's — so a letter typed before the Esc never opens Find again.
pub(super) fn closed_with(events: &mut Vec<egui::Event>, rest: Vec<egui::Event>) {
    events.retain(|e| !typing_event(e));
    events.extend(rest);
}

/// Find's keys that come after what is typed in the pass, taken out of `events` in order before
/// the field reads it (its engine would end its typing at an Enter): each acts once the field has
/// typed and the bar has jumped (`graph_ui`). With an Esc, every keyboard event after it is taken
/// too, for the graph once Find has closed.
pub(super) fn take_trailing(events: &mut Vec<egui::Event>) -> (Vec<FindKey>, Vec<egui::Event>) {
    let (mut keys, mut rest, mut closed) = (Vec::new(), Vec::new(), false);
    events.retain(|e| {
        if closed {
            let theirs = typing_event(e) || find_key(e).is_some();
            if theirs {
                rest.push(e.clone());
            }
            return !theirs;
        }
        match find_key(e) {
            Some(key) => {
                closed = key == FindKey::Close;
                keys.push(key);
                false
            }
            None => true,
        }
    });
    (keys, rest)
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
        self.texts().iter().all(|s| s.trim().is_empty())
    }
}

/// How the history search's dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SearchEnd {
    Run(SearchQuery),
    Cancel,
}

/// The history search's dialog, in cells (X4-find): the kit's box on the panels' grid, titled
/// `History search` — its six fields under labels of one width (each after its own label where
/// the grid leaves no field 12 cells after the widest), `[ ] Match case`, a blank row,
/// `[ Search ]  [ Cancel ]` and a note row. The keyboard opens on Message and goes back there
/// while none of its controls has it; Tab goes through them in that order. Enter searches — a
/// Shift+Enter or an Alt+Enter too, as the egui dialog's did — but each Enter of a pass goes to
/// what has the keyboard at that point (on Cancel it cancels), and one egui cannot follow there is
/// dropped, a Space too ([`take_enters`]). Only what the screen showed runs: an Enter or a Search
/// runs the query the last pass that reached the screen painted (`cells::OnScreen`: the terminal
/// prints no pass while input waits), unchanged in this one — not in the pass that lays the box
/// out unseen, nor the first one seen — else the note says the search changed and the next Enter
/// runs it. An empty query is refused there (`type what to search for`), and a query with a
/// control that is not on the grid and holds something (the box taller than it, a field of no
/// cells; a field's text, Match case on) is never run: the box says so first ([`TOO_SMALL`]) and
/// its Search is off (review round 1, fix-check). Esc, Cancel or a click
/// outside cancels; a pass that both cancels and searches cancels. What is typed in the pass that
/// opens it lands in Message, in order (its field reads that pass too). A change of the note asks
/// for a pass, so it shows at once.
#[derive(Debug, Clone, Default)]
pub(super) struct SearchDialog {
    pub query: SearchQuery,
    /// What the note row says.
    note: Note,
    /// The query the last pass that reached the screen painted where it could be seen (`None`
    /// until one has: egui's sizing pass paints nothing).
    screen: cells::OnScreen<SearchQuery>,
    /// The last pass laid out no cells for a control that holds something (a field's text, Match
    /// case on): [`TOO_SMALL`], Search off.
    off_grid: bool,
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

/// What the box says first, in the error colour, while a field holding text has no cells on the
/// grid: Search is off then (review round 1, as an action's dialog says it, X4-dialogs).
pub(super) const TOO_SMALL: &str = "the window is too small for all of it: make it larger";

/// The note once an Enter or a Search came with a change: only a search on screen runs.
const SEARCH_CHANGED: &str = "the search changed: Enter searches for it · Esc cancels";

/// The history search's box (its modal) and the ids of its controls.
fn search_id() -> egui::Id {
    egui::Id::unique("sopen-git-search")
}

pub(super) fn search_field_id(k: usize) -> egui::Id {
    search_id().with(("field", k))
}

pub(super) fn match_case_id() -> egui::Id {
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

/// The cells a field keeps after the labels padded to one width; with fewer, each field follows
/// its own label (a narrow grid).
const SEARCH_FIELD_FEWEST: usize = 12;

impl SearchQuery {
    /// The six fields' texts, in the dialog's order.
    fn texts(&self) -> [&str; 6] {
        [
            &self.message,
            &self.author,
            &self.text,
            &self.path,
            &self.since,
            &self.until,
        ]
        .map(String::as_str)
    }
}

impl SearchDialog {
    /// The box on `grid` as the dialog stands: [`TOO_SMALL`] first while a filled field has no
    /// cells, Search off then; the fields under labels padded to the widest, so they line up,
    /// where a field still keeps [`SEARCH_FIELD_FEWEST`] cells after them; the note cut to one
    /// row, so the box is the same whichever note it holds.
    fn layout(&self, grid: Grid) -> kit::Layout {
        let title = "History search";
        let width = kit::inside_width(grid.cols, SEARCH_NOMINAL, SEARCH_NOMINAL);
        let wide = (SEARCH_FIELDS.iter().map(|n| cells::width(n)).max()).unwrap_or(0);
        let pad = if width >= wide + 1 + SEARCH_FIELD_FEWEST {
            wide
        } else {
            0
        };
        let mut parts = Vec::new();
        if self.off_grid {
            parts.push(Part::Text(cells::fit_start(TOO_SMALL, width), Ink::Error));
        }
        parts.extend(
            (SEARCH_FIELDS.iter().enumerate()).map(|(k, name)| Part::Field {
                label: format!("{name:<pad$}"),
                id: search_field_id(k),
            }),
        );
        let (note, ink) = self.note.text();
        let mut search = Button::new("Search", search_button_id());
        search.on = !self.off_grid;
        parts.extend([
            Part::Check {
                label: "Match case".to_string(),
                id: match_case_id(),
            },
            Part::Blank,
            Part::Buttons(vec![search, Button::new("Cancel", cancel_button_id())]),
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

    /// The note row says `note` from the next pass, which a change asks for.
    fn say(&mut self, ctx: &egui::Context, note: Note) {
        if std::mem::replace(&mut self.note, note) != note {
            ctx.request_repaint();
        }
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
        let enters = take_enters(ctx, began.is_some() && !in_field, in_field);
        // egui lays a box it has no size for out unseen first (`Area`'s sizing pass: one it has
        // not shown before, or restored without its size): what that pass paints, nobody saw.
        let unseen = egui::AreaState::load(ctx, search_id()).is_none_or(|a| a.size.is_none());
        if !began.is_some_and(|f| controls.contains(&f)) {
            ctx.memory_mut(|m| m.request_focus(search_field_id(0)));
        }
        let layout = self.layout(grid);
        let before = self.query.clone();
        // What the owner saw: the query of the last pass that reached the screen.
        let seen = self.screen.start(ctx).cloned();
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
        // What this pass painted, where it could be seen: the next pass's Enter runs it, if this
        // pass reaches the screen.
        if !unseen {
            self.screen.painted(self.query.clone());
        }
        // A control that holds something other than its default with no cells this pass
        // (`Shown::controls` lists the laid ones) — a field with text, Match case on: never run,
        // the box saying so from the next pass (fix-check: Match case below the grid's foot was
        // searched).
        let laid = |id: egui::Id| pass.controls.contains(&id);
        let texts = self.query.texts();
        let off_grid = (0..texts.len()).any(|k| !texts[k].is_empty() && !laid(search_field_id(k)))
            || (self.query.case && !laid(match_case_id()));
        let was_off = std::mem::replace(&mut self.off_grid, off_grid);
        if was_off != off_grid {
            ctx.request_repaint();
        }
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
                self.say(ctx, Note::Hint);
            }
            return None;
        }
        // Search was off on screen, or a filled field has no cells now.
        if was_off || off_grid {
            return None;
        }
        // Only what the screen showed runs: the query the pass before painted, unchanged here.
        if changed || seen.as_ref() != Some(&self.query) {
            if seen.is_some() {
                self.say(ctx, Note::Changed);
            }
            ctx.request_repaint();
            return None;
        }
        if self.query.is_empty() {
            self.say(ctx, Note::Empty);
            return None;
        }
        Some(SearchEnd::Run(self.query.clone()))
    }
}

/// The pass's presses of Enter — with no modifiers, or Shift or Alt alone, as the egui dialog's
/// `consume_key` took them; never with Cmd/Ctrl — taken out of its input in order, each saying
/// whether it came after the pass's first move of the keyboard (a Tab, or an arrow where
/// `arrows_move`: on a control whose arrows egui takes as a move, not a field's caret) — so no
/// button, check box or field the keyboard is on reads it as its own: the dialog gives each to
/// what has the keyboard then. Where egui cannot follow the moves the owner made before an Enter,
/// that Enter is taken and dropped, and the owner presses it again on what now shows the keyboard:
/// - after a move, in a pass with a Shift+Tab or an arrow that moves the keyboard — egui moves it
///   only on the next pass, or at the pass's end;
/// - after a second move — egui moves the keyboard once a pass, its last direction (review round
///   1).
///
/// A Space there is dropped too, with its text, which the button or check box the keyboard is
/// leaving would take — unless the pass began on a field (`on_text`), which types it, and whose Tab
/// the kit already keeps any other control from pressing (review round 1). As X4-dialogs' action
/// dialogs read them, but for which Enters count (their message fields take Shift+Enter).
fn take_enters(ctx: &egui::Context, arrows_move: bool, on_text: bool) -> Vec<bool> {
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
        let (mut moved, mut out, mut typed) = (0usize, Vec::new(), false);
        // A press here goes where egui cannot follow the owner's moves.
        let lost = |moved: usize| moved >= 2 || (moved == 1 && later);
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
            ) if !(modifiers.command || modifiers.ctrl || modifiers.mac_cmd) => {
                if !lost(moved) {
                    out.push(moved > 0);
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
            ) if !on_text && lost(moved) && Mods::None.matches(*modifiers) => {
                typed = true;
                false
            }
            (e, _) => {
                moved += usize::from(moves(e).is_some());
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
