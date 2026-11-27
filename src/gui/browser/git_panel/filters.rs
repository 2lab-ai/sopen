//! The filters (03 §4.6): branches, remotes, tags, stashes, first parent, order, reflog commits;
//! the window's only.
//!
//! [`Filters`] is what the integration reads the log with: [`Filters::revisions`] are the refs to
//! hand S2-B's `history::roots` (with HEAD when [`Filters::head_is_revision`], and the stashes of
//! [`Filters::stash_revisions`]), `order`, `first_parent` and `reflog` its options, `stashes` the
//! row stream's `show_stashes` and `first_parent` the lane layout's. The view draws a ref's label
//! only when [`Filters::shows`] it, so the labels are exactly the refs the log was read from
//! (03 §4.1). The [`Dialog`] edits a copy; Enter applies it, Esc leaves them as they were.

use crate::git::model::{RefKind, RefLabel, StashRow};
use crate::gui::cells::{Cells, Grid};
use crate::gui::chrome;
use eframe::egui::{self, Key, Modifiers};
use std::collections::BTreeSet;

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

/// The Branches filter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BranchChoice {
    /// Every branch (the default).
    #[default]
    All,
    /// Only these branches, by refname (`refs/heads/main`, `refs/remotes/origin/main`); none
    /// chosen leaves no ref: `no refs chosen` (03 §4.1).
    Only(BTreeSet<String>),
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
}

fn dialog_id() -> egui::Id {
    egui::Id::unique("sopen-git-filters")
}

fn typed_id() -> egui::Id {
    egui::Id::unique("sopen-git-filters-typed")
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
        Dialog {
            filters: filters.clone(),
            typed: String::new(),
            branches,
            lower,
            shown: Vec::new(),
            shown_for: None,
            remotes: named,
        }
    }

    /// The branches the typed text leaves, worked out again only when it changed.
    fn narrow(&mut self) {
        let typed = self.typed.to_lowercase();
        if self.shown_for.as_ref() == Some(&typed) {
            return;
        }
        self.shown = (0..self.lower.len())
            .filter(|&k| self.lower[k].contains(&typed))
            .collect();
        self.shown_for = Some(typed);
    }

    /// Toggles one branch: from every branch, all but it; else in or out of the chosen.
    fn toggle_branch(&mut self, refname: &str) {
        let mut chosen = match &self.filters.branches {
            BranchChoice::All => self.branches.iter().cloned().collect(),
            BranchChoice::Only(chosen) => chosen.clone(),
        };
        if !chosen.remove(refname) {
            chosen.insert(refname.to_string());
        }
        self.filters.branches = BranchChoice::Only(chosen);
    }

    /// The cells of `grid` (the panels') the dialog covers, its box and its shadow, for the panel
    /// and the other side to blank (10 §17, X4-graph's seam): none while egui draws it, until
    /// X4-menu.
    pub fn covers(&self, _grid: &Grid) -> Vec<Cells> {
        Vec::new()
    }

    /// One pass on `grid` (the panels', where it stands once it is in cells): Esc and Enter are
    /// taken before its widgets see them.
    pub fn ui(&mut self, ctx: &egui::Context, _grid: Grid) -> Option<DialogEnd> {
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            return Some(DialogEnd::Cancel);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            return Some(DialogEnd::Apply(self.filters.clone()));
        }
        // The type-to-filter field has the typing while nothing else of the dialog has the
        // focus (egui lays a new dialog out unseen once first, so one early request is not
        // enough).
        if ctx.memory(|m| m.focused()).is_none() {
            ctx.memory_mut(|m| m.request_focus(typed_id()));
        }
        let mut end = None;
        let response = chrome::dialog(ctx, dialog_id(), "Filters", |ui| {
            ui.set_max_width(60.0 * chrome::CELL.x);
            ui.horizontal(|ui| {
                ui.label("Branches");
                let all = self.filters.branches == BranchChoice::All && !self.filters.reflog;
                if chrome::choice(ui, all, "All branches").clicked() {
                    self.filters.branches = if all {
                        BranchChoice::Only(BTreeSet::new())
                    } else {
                        BranchChoice::All
                    };
                }
                ui.add(
                    chrome::field(&mut self.typed)
                        .id(typed_id())
                        .hint_text("type to filter")
                        .desired_width(20.0 * chrome::CELL.x),
                );
            });
            // One branch a row; only the rows in view are laid out.
            self.narrow();
            let row = ui.spacing().interact_size.y;
            let mut toggled = None;
            egui::ScrollArea::vertical()
                .max_height(8.0 * (row + ui.spacing().item_spacing.y))
                .show_rows(ui, row, self.shown.len(), |ui, range| {
                    for &k in &self.shown[range] {
                        let refname = &self.branches[k];
                        let on = self.filters.has_branch(refname);
                        if chrome::choice(ui, on, short_name(refname)).clicked() {
                            toggled = Some(k);
                        }
                    }
                });
            if let Some(k) = toggled {
                let refname = self.branches[k].clone();
                self.toggle_branch(&refname);
            }
            if !self.remotes.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Remotes");
                    for remote in self.remotes.clone() {
                        let on = !self.filters.hidden_remotes.contains(&remote);
                        if chrome::choice(ui, on, &remote).clicked()
                            && !self.filters.hidden_remotes.remove(&remote)
                        {
                            self.filters.hidden_remotes.insert(remote);
                        }
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                let f = &mut self.filters;
                for (on, word) in [
                    (&mut f.tags, "Tags"),
                    (&mut f.stashes, "Stashes"),
                    (&mut f.first_parent, "First parent only"),
                    (&mut f.reflog, "Reflog commits"),
                ] {
                    if chrome::choice(ui, *on, word).clicked() {
                        *on = !*on;
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Order");
                for order in [Order::Date, Order::AuthorDate, Order::Topo] {
                    if chrome::choice(ui, self.filters.order == order, order.word()).clicked() {
                        self.filters.order = order;
                    }
                }
            });
            ui.add_space(chrome::CELL.y / 2.0);
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    end = Some(DialogEnd::Apply(self.filters.clone()));
                }
                if ui.button("Cancel").clicked() {
                    end = Some(DialogEnd::Cancel);
                }
            });
        });
        if end.is_none() && response.should_close() {
            end = Some(DialogEnd::Cancel);
        }
        end
    }
}
