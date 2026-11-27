//! The filters (03 §4.6): branches, remotes, tags, stashes, first parent, order, reflog commits;
//! the window's only.
//!
//! [`Filters`] is what the integration reads the log with: [`Filters::revisions`] are the refs to
//! hand S2-B's `history::roots` (with HEAD when [`Filters::head_is_revision`], and the stashes of
//! [`Filters::stash_revisions`]), `order`, `first_parent` and `reflog` its options, `stashes` the
//! row stream's `show_stashes` and `first_parent` the lane layout's. The view draws a ref's label
//! only when [`Filters::shows`] it, so the labels are exactly the refs the log was read from
//! (03 §4.1). The [`Dialog`] edits a copy; Enter applies it, Esc leaves them as they were.
//!
//! **In cells** (X4-menu, 10 §17): the dialog is an `ops::dialog` box on the panels' grid —
//! `Branches [X] All branches`, the type-to-filter field, the branches the typed text leaves as
//! the kit's list (`[X] name`, eight rows on screen as egui's, the bar, Space or a click turning
//! the bar's branch on or off), the remotes, `[X] Tags  [X] Stashes  [ ] First parent only
//! [ ] Reflog commits`, `Order (•) date  ( ) author date  ( ) topological`, `[ Apply ]  [ Cancel ]`
//! and the hint; 60 cells wide at least, as egui's was at most, widened by 10 §6's rule. The
//! field has the keyboard when it opens and while nothing of the dialog has it: typed letters
//! narrow the list after the pass, several in one pass in order, Hangul too, also in the pass
//! the dialog opens in. Tab goes through the controls in that order. Enter applies (03 §9.2:
//! inside a dialog, Enter runs) — on a focused `[ Cancel ]` it cancels — and each Enter of a pass
//! goes to what has the keyboard at that point: before the pass's first Tab what had it then,
//! after it what the Tab gave it to. One after a Shift+Tab or an arrow that moves the keyboard
//! (egui moves it after the pass) is dropped, and a Space too. Only what the screen showed is
//! applied or turned — the paintings of the last pass that reached the screen
//! (`cells::OnScreen`: a terminal prints no pass while input waits): Enter and `[ Apply ]` apply
//! only the filters such a pass painted, unchanged in it and in this one — else the box says the
//! filters changed and the next Enter applies them — and Space or a click on the list turns a
//! branch only while the list holds the rows it painted, in the box where it painted them (egui
//! gives a click to the control the pass before laid out under the pointer; the list finds the
//! row by where it stands now). A pass that types lays the list out before the text, so it
//! paints the rows it held, and a click in the next pass, where the text changed them, turns
//! nothing (in its own pass a click takes the keyboard from the field before it types, egui's
//! rule). Esc, `[ Cancel ]` or a click outside cancels, and a pass that both cancels and applies
//! cancels. Its ids are one set for the window, as the menu's ([`super::menu`]).

use crate::git::model::{RefKind, RefLabel, StashRow};
use crate::gui::browser::keys::{self, Mods};
use crate::gui::browser::ops::dialog::{self as kit, Bar, Button, Ink, Input, Inputs, Part};
use crate::gui::browser::ops::dialog::{Row, Rows};
use crate::gui::cells::{self, Cells, Grid};
use eframe::egui::{self, Key};
use std::collections::BTreeSet;
use std::sync::Arc;

/// The filters of one git panel (03 §4.6), their defaults git's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filters {
    /// The branches whose history is shown.
    pub branches: BranchChoice,
    /// The remotes turned off: their branches are neither shown nor read.
    pub hidden_remotes: BTreeSet<String>,
    /// Tag labels, and tags as the log's revisions.
    pub tags: bool,
    /// Stash rows, and stashes' bases as the log's revisions.
    pub stashes: bool,
    /// `--first-parent`, and the layout's first parents only.
    pub first_parent: bool,
    pub order: Order,
    /// `--reflog`, with every branch shown.
    pub reflog: bool,
}

/// The Branches filter. Its copies and comparisons count the names they walk in a test build
/// ([`copied`]): the dialog's pass may walk none of them (X4-menu round 1, 20,000 branches).
#[derive(Debug, Default, Eq)]
pub enum BranchChoice {
    /// Every branch (the default).
    #[default]
    All,
    /// Only these branches, by refname (`refs/heads/main`, `refs/remotes/origin/main`); none
    /// chosen leaves no ref: `no refs chosen` (03 §4.1).
    Only(BTreeSet<String>),
}

impl Clone for BranchChoice {
    fn clone(&self) -> Self {
        match self {
            BranchChoice::All => BranchChoice::All,
            BranchChoice::Only(chosen) => {
                walked(chosen.len());
                BranchChoice::Only(chosen.clone())
            }
        }
    }
}

impl PartialEq for BranchChoice {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (BranchChoice::All, BranchChoice::All) => true,
            (BranchChoice::Only(a), BranchChoice::Only(b)) => {
                walked(a.len().min(b.len()));
                a == b
            }
            _ => false,
        }
    }
}

#[cfg(not(test))]
#[inline(always)]
fn walked(_: usize) {}

#[cfg(test)]
fn walked(n: usize) {
    COPIED.with(|c| c.set(c.get() + n));
}

#[cfg(test)]
thread_local! {
    static COPIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The branch names copied or compared since the last call (a test's bound on a pass).
#[cfg(test)]
pub(super) fn copied() -> usize {
    COPIED.with(|c| c.replace(0))
}

/// The log's order (03 §4.1): S2-B's `history::Order`, variant for variant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    /// `--date-order`, the default.
    #[default]
    Date,
    /// `--author-date-order`.
    AuthorDate,
    /// `--topo-order`.
    Topo,
}

impl Order {
    /// The dialog's word for it.
    fn word(self) -> &'static str {
        match self {
            Order::Date => "date",
            Order::AuthorDate => "author date",
            Order::Topo => "topological",
        }
    }
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            branches: BranchChoice::All,
            hidden_remotes: BTreeSet::new(),
            tags: true,
            stashes: true,
            first_parent: false,
            order: Order::Date,
            reflog: false,
        }
    }
}

impl Filters {
    /// Every filter at its default: the frame title has no ` [filtered]` (03 §4.6).
    pub fn is_default(&self) -> bool {
        *self == Filters::default()
    }

    /// The branches chosen, `None` for every branch: "Reflog commits" shows every branch
    /// (03 §4.6), whatever the Branches filter holds.
    fn chosen(&self) -> Option<&BTreeSet<String>> {
        match &self.branches {
            BranchChoice::Only(chosen) if !self.reflog => Some(chosen),
            _ => None,
        }
    }

    /// Whether `r`'s label is drawn — and so whether it is one of the log's revisions; `remotes`
    /// are the repository's ([`split_remote`]). A remote's own `HEAD` never is (03 §4.4); a branch
    /// whose name ends in `/HEAD` is a branch. With a branch filter only the chosen branches are
    /// (03:298–299, "the chosen refs' ids only"), so no tag either.
    pub fn shows(&self, r: &RefLabel, remotes: &[String]) -> bool {
        let chosen = |r: &RefLabel| self.chosen().is_none_or(|c| c.contains(&r.refname));
        match r.kind {
            RefKind::LocalBranch => chosen(r),
            RefKind::RemoteBranch => match split_remote(&r.refname, remotes) {
                Some((_, "HEAD")) => false,
                Some((remote, _)) => !self.hidden_remotes.contains(remote) && chosen(r),
                None => chosen(r),
            },
            RefKind::Tag => self.tags && self.chosen().is_none(),
        }
    }

    /// The refs to read the log from, for S2-B's `history::roots`: those [`Filters::shows`].
    pub fn revisions(&self, refs: &[RefLabel], remotes: &[String]) -> Vec<RefLabel> {
        refs.iter()
            .filter(|r| self.shows(r, remotes))
            .cloned()
            .collect()
    }

    /// Whether HEAD's commit is one of the log's revisions: not with a branch filter (03:298).
    pub fn head_is_revision(&self) -> bool {
        self.chosen().is_none()
    }

    /// The stashes whose bases are revisions: none with Stashes off, nor with a branch filter.
    pub fn stash_revisions<'a>(&self, stashes: &'a [StashRow]) -> &'a [StashRow] {
        if self.stashes && self.chosen().is_none() {
            stashes
        } else {
            &[]
        }
    }

    /// "Show only this branch" (a label's menu, 03 §7.2).
    pub fn only(&mut self, refname: &str) {
        self.branches = BranchChoice::Only(BTreeSet::from([refname.to_string()]));
    }

    /// "Show all branches".
    pub fn all_branches(&mut self) {
        self.branches = BranchChoice::All;
    }

    /// Whether the branch `refname` is chosen.
    fn has_branch(&self, refname: &str) -> bool {
        self.chosen().is_none_or(|c| c.contains(refname))
    }
}

/// A remote branch's refname as its remote and its branch: the longest of `remotes` (the
/// repository's, `git remote`) that `refs/remotes/<remote>/` starts it — `team/up/main` is
/// `team/up`'s branch `main` when both `team` and `team/up` are remotes — else, without such a
/// remote, its first component. `None` for any other refname.
pub fn split_remote<'a>(refname: &'a str, remotes: &[String]) -> Option<(&'a str, &'a str)> {
    let rest = refname.strip_prefix("refs/remotes/")?;
    let named = remotes
        .iter()
        .filter(|r| {
            rest.strip_prefix(r.as_str())
                .is_some_and(|after| after.starts_with('/'))
        })
        .map(String::len)
        .max();
    match named {
        Some(n) => Some((&rest[..n], &rest[n + 1..])),
        None => rest.split_once('/'),
    }
}

/// A ref's name without its kind's prefix: `main`, `origin/main`, `v1`.
pub fn short_name(refname: &str) -> &str {
    ["refs/heads/", "refs/remotes/", "refs/tags/"]
        .iter()
        .find_map(|p| refname.strip_prefix(p))
        .unwrap_or(refname)
}

/// How the filters' dialog ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DialogEnd {
    /// Enter or Apply: these filters.
    Apply(Filters),
    /// Esc, Cancel or a click outside it.
    Cancel,
}

/// The filters' dialog: a copy of the filters, edited.
#[derive(Debug, Clone)]
pub(super) struct Dialog {
    filters: Filters,
    /// The Branches list's type-to-filter text.
    typed: String,
    /// Every local and remote branch, by refname (a remote's own `HEAD` left out), as read.
    branches: Vec<String>,
    /// Their short names in lower case, for the type-to-filter (made once).
    lower: Vec<String>,
    /// The branches the typed text leaves, by index, and the text they are for: worked out when
    /// the text changes, never per pass.
    shown: Vec<usize>,
    shown_for: Option<String>,
    /// The repository's remotes, else every remote a branch names.
    remotes: Vec<String>,
    /// In cells: the shown branches as the kit's rows, `[X] name` or `[ ] name` (made when the
    /// shown branches or their marks change, never per pass), and the list's bar.
    rows: Arc<Rows>,
    bar: Bar,
    /// How many times the shown branches changed: which rows the list holds.
    made: u64,
    /// The `made` of the rows and where the box stood, as the last pass that reached the screen
    /// painted them (`cells::OnScreen`: a terminal prints no pass while input waits).
    rows_screen: cells::OnScreen<(u64, Vec<Cells>)>,
    /// Bumped whenever a filter changes: which filters these are, compared in place of the
    /// filters themselves (a copy of 20,000 chosen branches a pass would not be bounded).
    revision: u64,
    /// The `revision` the last pass that reached the screen painted, where it changed none of
    /// them — `None` for a pass that changed one, which paints the choices as it leaves them and
    /// the list's marks as they were: it shows no filters to apply, but an Enter after it says the
    /// filters changed.
    filters_screen: cells::OnScreen<Option<u64>>,
    /// An Enter or `[ Apply ]` came while the filters on screen were not the ones it would apply.
    changed: bool,
    /// The dialog's controls, as the last pass laid them (the ones that may hold the keyboard).
    controls: Vec<egui::Id>,
}

fn dialog_id() -> egui::Id {
    egui::Id::unique("sopen-git-filters")
}

fn typed_id() -> egui::Id {
    egui::Id::unique("sopen-git-filters-typed")
}

fn list_id() -> egui::Id {
    dialog_id().with("branches")
}

/// The id of the dialog's row of choices `name`.
fn row_id(name: &str) -> egui::Id {
    dialog_id().with(("row", name))
}

fn apply_id() -> egui::Id {
    dialog_id().with("apply")
}

fn cancel_id() -> egui::Id {
    dialog_id().with("cancel")
}

/// The Branches row's one choice.
const ALL: &str = "All branches";

/// The type-to-filter field's label: what egui's field said inside it, which a field in cells
/// cannot.
const TYPED: &str = "Type to filter";

/// The row of filters that are on or off, in its order.
const SHOWS: [&str; 4] = ["Tags", "Stashes", "First parent only", "Reflog commits"];

/// The orders, in the row's order.
const ORDERS: [Order; 3] = [Order::Date, Order::AuthorDate, Order::Topo];

/// The branches the list shows at most: egui's eight rows.
const BRANCH_ROWS: u16 = 8;

/// The dialog's nominal width inside, in cells: egui's widest (10 §6's rule widens it to its
/// widest text, at most 76).
const NOMINAL: usize = 60;

/// What the dialog says Enter and Esc do — on the status row too while it is open.
pub(super) const HINT: &str = "Space turns on or off · Enter applies · Esc cancels";

/// The hint once an Enter or `[ Apply ]` came while the filters on screen were not the ones it
/// would apply: only what showed is applied.
pub(super) const CHANGED: &str = "the filters changed — Enter applies them · Esc cancels";

/// The pass's presses of Enter with no modifiers, taken out of its input in order, each saying
/// whether it came after the pass's first move of the keyboard (a Tab) — and the Enters and
/// Spaces where egui cannot follow the owner's moves, dropped with a Space's text: after a move
/// egui makes only after the pass (a Shift+Tab; an arrow on a choice or a button, and ← → on the
/// list) they would reach the control the keyboard is leaving, and after a second move in the
/// pass the control egui stops on (it moves the keyboard once a pass, the last direction). A
/// Space is dropped so only when the pass did not begin in the field, which typed it. `began`:
/// what had the keyboard as the pass began (in the field arrows are its caret's; the list keeps
/// ↑ ↓). X4-dialogs' rules, in the same shape (`dialogs::take_enters`).
fn take_enters(ctx: &egui::Context, began: Option<egui::Id>) -> Vec<bool> {
    let (field, list) = (began == Some(typed_id()), began == Some(list_id()));
    // Whether a key moves the keyboard as egui reads it (`None`: not), and whether egui makes that
    // move only after the pass (`true`) rather than in it (a Tab).
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
        } if began.is_some() && !field && !modifiers.any() => match key {
            Key::ArrowLeft | Key::ArrowRight => Some(true),
            Key::ArrowUp | Key::ArrowDown if !list => Some(true),
            _ => None,
        },
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
            ) if Mods::None.matches(*modifiers) => {
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
            ) if !field && lost(moved) && Mods::None.matches(*modifiers) => {
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

/// The pass's plain Enter and Space presses, and a Space's text, taken out of its input: the pass
/// that closes the dialog leaves none for what is under it.
fn drop_presses(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        let mut typed = false;
        i.events.retain(|e| match (e, std::mem::take(&mut typed)) {
            (egui::Event::Text(text), true) if text == " " => false,
            (
                egui::Event::Key {
                    key: key @ (Key::Enter | Key::Space),
                    pressed: true,
                    modifiers,
                    ..
                },
                _,
            ) if Mods::None.matches(*modifiers) => {
                typed = *key == Key::Space;
                false
            }
            _ => true,
        });
    });
}

/// The pass's plain Space presses no control took, and their text, taken out of its input: the
/// dialog has the keyboard, so none reaches what is under it.
fn drop_spaces(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        let mut typed = false;
        i.events.retain(|e| match (e, std::mem::take(&mut typed)) {
            (egui::Event::Text(text), true) if text == " " => false,
            (
                egui::Event::Key {
                    key: Key::Space,
                    pressed: true,
                    modifiers,
                    ..
                },
                _,
            ) if Mods::None.matches(*modifiers) => {
                typed = true;
                false
            }
            _ => true,
        });
    });
}

impl Dialog {
    /// Over `filters`, listing the branches and remotes of `refs`.
    pub fn new(filters: &Filters, refs: &[RefLabel], remotes: &[String]) -> Dialog {
        let own_head =
            |r: &RefLabel| matches!(split_remote(&r.refname, remotes), Some((_, "HEAD")));
        let branches: Vec<String> = refs
            .iter()
            .filter(|r| r.kind != RefKind::Tag && !own_head(r))
            .map(|r| r.refname.clone())
            .collect();
        let lower = branches
            .iter()
            .map(|b| short_name(b).to_lowercase())
            .collect();
        let mut named: Vec<String> = remotes.to_vec();
        if named.is_empty() {
            for (name, _) in refs.iter().filter_map(|r| split_remote(&r.refname, &[])) {
                if !named.iter().any(|r| r == name) {
                    named.push(name.to_string());
                }
            }
        }
        let mut dialog = Dialog {
            filters: filters.clone(),
            typed: String::new(),
            branches,
            lower,
            shown: Vec::new(),
            shown_for: None,
            remotes: named,
            rows: Rows::new(Vec::new()),
            bar: Bar::default(),
            made: 0,
            rows_screen: cells::OnScreen::default(),
            revision: 0,
            filters_screen: cells::OnScreen::default(),
            changed: false,
            controls: Vec::new(),
        };
        dialog.narrow();
        dialog.rows = dialog.branch_rows();
        dialog
    }

    /// The branches the typed text leaves, worked out again only when it changed: whether they
    /// changed.
    fn narrow(&mut self) -> bool {
        let typed = self.typed.to_lowercase();
        if self.shown_for.as_ref() == Some(&typed) {
            return false;
        }
        let shown: Vec<usize> = (0..self.lower.len())
            .filter(|&k| self.lower[k].contains(&typed))
            .collect();
        self.shown_for = Some(typed);
        let changed = shown != self.shown;
        self.shown = shown;
        changed
    }

    /// The shown branches as the list's rows, each with its mark.
    fn branch_rows(&self) -> Arc<Rows> {
        let row = |&k: &usize| {
            let refname = &self.branches[k];
            let mark = if self.filters.has_branch(refname) {
                "[X]"
            } else {
                "[ ]"
            };
            Row::new(&format!("{mark} {}", short_name(refname)), "")
        };
        Rows::new(self.shown.iter().map(row).collect())
    }

    /// Toggles one branch: from every branch, all but it; else in or out of the chosen.
    fn toggle_branch(&mut self, refname: &str) {
        if self.filters.branches == BranchChoice::All {
            let every = self.branches.iter().cloned().collect();
            self.filters.branches = BranchChoice::Only(every);
        }
        if let BranchChoice::Only(chosen) = &mut self.filters.branches {
            if !chosen.remove(refname) {
                chosen.insert(refname.to_string());
            }
        }
    }

    /// Whether "All branches" shows chosen: every branch, and no reflog (which shows every branch
    /// whatever the Branches filter holds).
    fn all(&self) -> bool {
        self.filters.branches == BranchChoice::All && !self.filters.reflog
    }

    /// The box on `grid` (the panels'), as the dialog stands (see the module's notes).
    fn layout(&self, grid: Grid) -> kit::Layout {
        let choices = |label: &str, name: &str, words: Vec<String>, many: bool| Part::Choices {
            label: label.to_string(),
            id: row_id(name),
            choices: words,
            many,
        };
        let words = |all: &[&str]| all.iter().map(|w| w.to_string()).collect();
        let mut parts = vec![
            choices("Branches", "all", words(&[ALL]), true),
            Part::Field {
                label: TYPED.to_string(),
                id: typed_id(),
            },
            Part::List {
                rows: self.rows.clone(),
                id: list_id(),
                most: BRANCH_ROWS,
            },
        ];
        if !self.remotes.is_empty() {
            parts.push(choices("Remotes", "remotes", self.remotes.clone(), true));
        }
        parts.push(choices("", "shows", words(&SHOWS), true));
        let orders = ORDERS.iter().map(|o| o.word().to_string()).collect();
        parts.push(choices("Order", "order", orders, false));
        parts.push(Part::Blank);
        let buttons = vec![
            Button::new("Apply", apply_id()),
            Button::new("Cancel", cancel_id()),
        ];
        parts.push(Part::Buttons(buttons));
        parts.push(match self.changed {
            true => Part::Text(CHANGED.to_string(), Ink::Text),
            false => Part::Text(HINT.to_string(), Ink::Weak),
        });
        kit::layout(grid, "Filters", NOMINAL, &parts)
    }

    /// The cells of `grid` (the panels') the dialog covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17): the box as this pass lays it out, from the dialog as
    /// it stands when the pass begins.
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        self.layout(*grid).covers()
    }

    /// One pass on `grid` (the panels'), in cells (see the module's notes): Esc and Enter are
    /// taken before its controls see them.
    pub fn ui(&mut self, ctx: &egui::Context, grid: Grid) -> Option<DialogEnd> {
        // What the screen shows as the owner pressed this pass's keys: the last paintings that
        // reached it.
        let seen_rows = self.rows_screen.start(ctx).cloned();
        let seen = self.filters_screen.start(ctx).copied();
        let mut escape = false;
        while ctx.input_mut(|i| keys::take(i, Key::Escape, Mods::None)) {
            escape = true;
        }
        if escape {
            drop_presses(ctx);
            return Some(DialogEnd::Cancel);
        }
        let began = ctx.memory(|m| m.focused());
        let enters = take_enters(ctx, began);
        // The field has the keyboard while nothing of the dialog has it.
        if !began.is_some_and(|id| self.controls.contains(&id)) {
            ctx.memory_mut(|m| m.request_focus(typed_id()));
        }
        let laid_revision = self.revision;
        let laid_all = self.all();
        let laid = self.layout(grid);
        let f = &self.filters;
        let mut all = [laid_all];
        let mut remotes: Vec<bool> = (self.remotes.iter())
            .map(|r| !f.hidden_remotes.contains(r))
            .collect();
        let mut shows = [f.tags, f.stashes, f.first_parent, f.reflog];
        let mut order = ORDERS.iter().position(|o| *o == f.order);
        let inputs = Inputs {
            by_id: vec![
                (row_id("all"), Input::Any(&mut all)),
                (typed_id(), Input::Text(&mut self.typed)),
                (list_id(), Input::List(&mut self.bar)),
                (row_id("remotes"), Input::Any(&mut remotes)),
                (row_id("shows"), Input::Any(&mut shows)),
                (row_id("order"), Input::One(&mut order)),
                (apply_id(), Input::Button),
                (cancel_id(), Input::Button),
            ],
            ..Inputs::default()
        };
        let shown = kit::show(ctx, dialog_id(), &laid, inputs);
        drop_spaces(ctx);
        let now = ctx.memory(|m| m.focused());
        self.controls = shown.controls.clone();
        // The list's rows chosen, each the branch the pass before painted there, in the box where
        // it painted them: egui gives a click to the widget the pass before laid out under the
        // pointer (its hit test reads the last pass's widgets, `Context::begin_pass`), but the list
        // finds its row by where it stands now, and a box laid elsewhere (fewer or more rows, the
        // grid resized) has another row there.
        let at = (self.made, laid.covers());
        let seen_rows = seen_rows.as_ref() == Some(&at);
        self.rows_screen.painted(at);
        // Whether the branches' marks change: the Branches filter or Reflog commits.
        let mut marks = false;
        if seen_rows {
            for k in shown.chosen(list_id()) {
                if let Some(refname) = self.shown.get(k).map(|&b| self.branches[b].clone()) {
                    self.toggle_branch(&refname);
                    marks = true;
                }
            }
        }
        // The choices, as their presses left them; each change a revision.
        if all[0] != laid_all {
            self.filters.branches = match laid_all {
                true => BranchChoice::Only(BTreeSet::new()),
                false => BranchChoice::All,
            };
            marks = true;
        }
        let mut changed = marks;
        for (remote, on) in self.remotes.iter().zip(remotes) {
            changed |= match on {
                true => self.filters.hidden_remotes.remove(remote),
                false => self.filters.hidden_remotes.insert(remote.clone()),
            };
        }
        let f = &mut self.filters;
        let was = [f.tags, f.stashes, f.first_parent, f.reflog];
        marks |= was[3] != shows[3];
        changed |= was != shows;
        [f.tags, f.stashes, f.first_parent, f.reflog] = shows;
        if let Some(order) = order.and_then(|k| ORDERS.get(k)) {
            changed |= f.order != *order;
            f.order = *order;
        }
        if changed {
            self.revision += 1;
        }
        // The rows anew where the typed text changed what they show (the bar on the first) or a
        // branch's mark changed.
        let narrowed = self.narrow();
        if narrowed || marks {
            self.rows = self.branch_rows();
        }
        if narrowed {
            (self.made, self.bar) = (self.made + 1, Bar::default());
        }
        // Each Enter to what had the keyboard at its point of the pass, the buttons' presses.
        let mut cancel = shown.clicked(cancel_id()) || shown.modal.should_close();
        let mut apply = shown.clicked(apply_id());
        for after_tab in enters {
            match if after_tab { now } else { began } {
                Some(id) if id == cancel_id() => cancel = true,
                _ => apply = true,
            }
        }
        let unchanged = self.revision == laid_revision;
        self.filters_screen
            .painted(unchanged.then_some(laid_revision));
        if cancel {
            return Some(DialogEnd::Cancel);
        }
        if !unchanged || narrowed {
            ctx.request_repaint();
        }
        if !apply {
            return None;
        }
        // Only what the screen showed is applied: the filters the last pass that reached it
        // painted, which this pass left as they were.
        if seen == Some(Some(self.revision)) {
            return Some(DialogEnd::Apply(self.filters.clone()));
        }
        self.changed |= seen.is_some();
        ctx.request_repaint();
        None
    }
}
