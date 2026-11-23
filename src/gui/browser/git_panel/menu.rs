//! The action menu (03 §7, §9: F10, Shift+F10, a right click): the actions of the object under the
//! bar — a commit row, a label, a stash row, the uncommitted row and its files, the repository —
//! by their `git::actions::ActionId`s.
//!
//! [`items`] is the menu of a [`Target`], in 03 §7's table order, with what each item is called
//! and its key; an action is offered only where its table says it applies (`checkout-branch` not
//! on the checked-out branch, `fetch-into` only beside a local branch that is not checked out, no
//! remote action without a remote, nothing that names a ref or a path that is not UTF-8). The
//! [`Menu`] draws it in the dialog look (`chrome::dialog`) with a bar of its own — ↑ ↓ PgUp PgDn
//! Home End move it, Enter or a click chooses, Esc or F10 closes it — and every menu but the
//! repository's ends in `Repository…`, which turns it into the repository's menu. Choosing gives
//! the id and its target back; [`super::dialogs::ActionDialog::open`] makes the dialog of it, and
//! the views among the ids ([`is_view`]) are the panel's to carry out.

use super::filters::split_remote;
use crate::git::actions::{ActionId, Repo};
use crate::git::model::{
    CommitRow, Head, ObjectType, RefKind, RefLabel, StashRow, StatusEntry, StatusState,
};
use crate::git::rebase::EditId;
use crate::gui::browser::keys::{self, Mods};
use crate::gui::chrome;
use eframe::egui::{self, Key};

/// The object whose menu opens (03 §7.1–§7.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A commit row (§7.1): the bar's commit and the marked commits, as the rows show them
    /// (newest first; the bar's among them when it is marked).
    Commit {
        commit: CommitRow,
        marked: Vec<CommitRow>,
    },
    /// The label under the label cursor: a local branch (§7.2), a remote branch (§7.3) or a tag
    /// (§7.4), as the refs read gives it.
    Label(RefLabel),
    /// A stash row (§7.5).
    Stash(StashRow),
    /// The uncommitted row (§7.6): its files are the status read's.
    Uncommitted,
    /// Files of the uncommitted row (§7.6): the one under the details pane's bar, or the marked
    /// ones.
    Files(Vec<StatusEntry>),
    /// The repository (§7.7).
    Repository,
}

/// One line of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: ActionId,
    /// What the menu calls it (03 §7's Action column).
    pub label: String,
    /// Its key, at the line's right end (03 §9): `F8`, `Cmd+Shift+R`.
    pub key: Option<String>,
}

/// The ids the panel carries out itself, with no plan and no dialog of this module (03 §8.1's
/// "No confirmation": views and the clipboard). `undo` is the panel's too: it builds the
/// restore of the newest group written in this window (§8.3), whose dialog is
/// [`super::dialogs::ActionDialog::restore`].
pub fn is_view(id: ActionId) -> bool {
    use ActionId as A;
    matches!(
        id,
        A::Compare
            | A::CopyHash
            | A::CopySubject
            | A::CopyName
            | A::OnlyBranch
            | A::AllBranches
            | A::TagDetails
            | A::Editor
            | A::RemoteVisibility
            | A::Refresh
            | A::Find
            | A::Search
            | A::Filters
            | A::Log
            | A::Backups
            | A::ShowBackup
            | A::Undo
    )
}

/// What 03 §7's tables call each action (its Action column), by id. The ids missing here (one
/// added later) show their id.
const LABELS: &[(ActionId, &str)] = &[
    (ActionId::Tag, "Tag…"),
    (ActionId::Branch, "New branch…"),
    (ActionId::MoveBranch, "Move a branch here…"),
    (ActionId::Checkout, "Check out (detached HEAD)…"),
    (ActionId::Pick, "Cherry-pick…"),
    (ActionId::Revert, "Revert…"),
    (ActionId::Merge, "Merge into the current branch…"),
    (ActionId::Rebase, "Rebase the current branch onto this…"),
    (ActionId::Reset, "Reset the current branch here…"),
    (ActionId::Archive, "Save archive…"),
    (ActionId::CopyHash, "Copy hash"),
    (ActionId::CopySubject, "Copy subject"),
    (ActionId::Compare, "Compare with the marked commit"),
    (ActionId::CheckoutBranch, "Check out"),
    (ActionId::Rename, "Rename…"),
    (ActionId::DeleteBranch, "Delete…"),
    (ActionId::Push, "Push…"),
    (ActionId::OnlyBranch, "Show only this branch"),
    (ActionId::AllBranches, "Show all branches"),
    (ActionId::CopyName, "Copy name"),
    (ActionId::CheckoutRemote, "Check out as a local branch…"),
    (ActionId::DeleteRemoteBranch, "Delete on the remote…"),
    (ActionId::FetchInto, "Fetch into the local branch…"),
    (ActionId::Pull, "Pull into the current branch…"),
    (ActionId::TagDetails, "Details"),
    (ActionId::DeleteTag, "Delete…"),
    (ActionId::PushTag, "Push…"),
    (ActionId::MoveTag, "Move tag here…"),
    (ActionId::StashApply, "Apply…"),
    (ActionId::StashPop, "Pop…"),
    (ActionId::StashBranch, "New branch from it…"),
    (ActionId::StashDrop, "Drop…"),
    (ActionId::Stage, "Stage"),
    (ActionId::Unstage, "Unstage"),
    (ActionId::Commit, "Commit…"),
    (
        ActionId::Amend,
        "Add the staged changes to the last commit…",
    ),
    (ActionId::Stash, "Stash the changes…"),
    (ActionId::Discard, "Discard changes…"),
    (ActionId::ResetChanges, "Reset the changes…"),
    (ActionId::Clean, "Remove untracked files…"),
    (ActionId::Ours, "Take ours…"),
    (ActionId::Theirs, "Take theirs…"),
    (ActionId::Resolved, "Mark resolved…"),
    (ActionId::RedoMerge, "Redo the merge of the file…"),
    (ActionId::Editor, "Edit"),
    (ActionId::Fetch, "Fetch all remotes…"),
    (ActionId::FetchRemote, "Fetch a remote…"),
    (ActionId::PruneRemote, "Prune a remote…"),
    (ActionId::AddRemote, "Add a remote…"),
    (ActionId::EditRemote, "Edit a remote…"),
    (ActionId::DeleteRemote, "Delete a remote…"),
    (
        ActionId::RemoteVisibility,
        "Show / hide a remote's branches",
    ),
    (ActionId::User, "Name and email…"),
    (ActionId::UserRemove, "Remove name and email…"),
    (ActionId::Refresh, "Refresh"),
    (ActionId::Find, "Find"),
    (ActionId::Search, "History search"),
    (ActionId::Filters, "Filters"),
    (ActionId::Log, "Command log"),
    (ActionId::Backups, "Backups"),
    (ActionId::Undo, "Undo"),
    (ActionId::Restore, "Restore…"),
    (ActionId::ShowBackup, "Show"),
    (ActionId::DeleteBackup, "Delete…"),
    (ActionId::Continue, "Continue"),
    (ActionId::Skip, "Skip"),
    (ActionId::Abort, "Abort"),
    (ActionId::BisectReset, "Reset (bisect)"),
];

/// What 03 §7 calls `id` (its Action column).
pub fn label(id: ActionId) -> &'static str {
    LABELS
        .iter()
        .find(|(i, _)| *i == id)
        .map_or(id.as_str(), |(_, l)| l)
}

/// The checked-out branch's refname, when one is (a name not UTF-8 is none).
pub(super) fn checked_out(repo: &Repo) -> Option<&str> {
    match &repo.head {
        Head::Branch {
            refname,
            lossy: false,
            ..
        } => Some(refname),
        _ => None,
    }
}

/// The local branches a commit's `move-branch` may move: not the checked-out one, not one whose
/// name is not UTF-8 (by short name, in the refs read's order).
pub(super) fn movable(repo: &Repo) -> Vec<String> {
    let out = checked_out(repo);
    repo.refs
        .iter()
        .filter(|r| r.kind == RefKind::LocalBranch && !r.lossy && Some(r.refname.as_str()) != out)
        .map(|r| r.name().to_string())
        .collect()
}

/// The repository's tags whose names are UTF-8, by short name.
pub(super) fn tags(repo: &Repo) -> Vec<String> {
    repo.refs
        .iter()
        .filter(|r| r.kind == RefKind::Tag && !r.lossy)
        .map(|r| r.name().to_string())
        .collect()
}

/// Whether a local branch `name` exists (its name UTF-8).
pub(super) fn has_local(repo: &Repo, name: &str) -> bool {
    let refname = format!("refs/heads/{name}");
    repo.refs
        .iter()
        .any(|r| r.kind == RefKind::LocalBranch && !r.lossy && r.refname == refname)
}

/// A status entry's parts (03 §6.1): changes not staged or untracked, staged changes, a conflict.
pub(super) fn unstaged(e: &StatusEntry) -> bool {
    matches!(
        e.state,
        StatusState::Changed {
            unstaged: Some(_),
            ..
        } | StatusState::Untracked
    )
}

pub(super) fn staged(e: &StatusEntry) -> bool {
    matches!(
        e.state,
        StatusState::Changed {
            staged: Some(_),
            ..
        }
    )
}

pub(super) fn conflicted(e: &StatusEntry) -> bool {
    matches!(e.state, StatusState::Conflict(_))
}

/// A tracked change (what `--autostash` stashes): not untracked, not a conflict.
pub(super) fn tracked_change(e: &StatusEntry) -> bool {
    matches!(e.state, StatusState::Changed { .. })
}

/// The ids `target`'s menu offers, in 03 §7's table order.
fn ids(target: &Target, repo: &Repo) -> Vec<ActionId> {
    use ActionId as A;
    let any_remote = !repo.remotes.is_empty();
    match target {
        Target::Commit { commit, marked } => {
            let mut v = vec![A::Tag, A::Branch];
            if !movable(repo).is_empty() {
                v.push(A::MoveBranch);
            }
            v.extend([
                A::Checkout,
                A::Pick,
                A::Revert,
                A::Merge,
                A::Rebase,
                A::Reset,
                A::Archive,
                A::CopyHash,
                A::CopySubject,
            ]);
            if let [other] = &marked[..] {
                if other.oid != commit.oid {
                    v.push(A::Compare);
                }
            }
            if !tags(repo).is_empty() {
                v.push(A::MoveTag);
            }
            v
        }
        Target::Label(r) if r.lossy => Vec::new(),
        Target::Label(r) => match r.kind {
            RefKind::LocalBranch => {
                let mut v = Vec::new();
                if checked_out(repo) != Some(r.refname.as_str()) {
                    v.push(A::CheckoutBranch);
                }
                v.extend([A::Rename, A::DeleteBranch, A::Merge, A::Rebase]);
                if any_remote {
                    v.push(A::Push);
                }
                v.extend([A::Archive, A::OnlyBranch, A::AllBranches, A::CopyName]);
                v
            }
            RefKind::RemoteBranch => {
                let Some((_, branch)) = split_remote(&r.refname, &repo.remotes) else {
                    return Vec::new();
                };
                let mut v = vec![A::CheckoutRemote, A::DeleteRemoteBranch];
                let local = format!("refs/heads/{branch}");
                if has_local(repo, branch) && checked_out(repo) != Some(local.as_str()) {
                    v.push(A::FetchInto);
                }
                v.extend([
                    A::Merge,
                    A::Pull,
                    A::Archive,
                    A::OnlyBranch,
                    A::AllBranches,
                    A::CopyName,
                ]);
                v
            }
            RefKind::Tag => {
                let mut v = Vec::new();
                if r.object_type == ObjectType::Tag {
                    v.push(A::TagDetails);
                }
                v.push(A::DeleteTag);
                if any_remote {
                    v.push(A::PushTag);
                }
                v.extend([A::Archive, A::CopyName]);
                v
            }
        },
        Target::Stash(_) => vec![
            A::StashApply,
            A::StashPop,
            A::StashBranch,
            A::StashDrop,
            A::CopyName,
            A::CopyHash,
        ],
        Target::Uncommitted => {
            let es: Vec<&StatusEntry> = repo.status.entries.iter().filter(|e| !e.lossy).collect();
            let mut v = Vec::new();
            if es.iter().any(|e| unstaged(e)) {
                v.push(A::Stage);
            }
            if es.iter().any(|e| staged(e)) {
                v.push(A::Unstage);
            }
            v.push(A::Commit);
            if repo.head.oid().is_some() {
                v.push(A::Amend);
            }
            v.extend([A::Stash, A::ResetChanges]);
            if es.iter().any(|e| e.state == StatusState::Untracked) {
                v.push(A::Clean);
            }
            v.push(A::Editor);
            v
        }
        Target::Files(entries) => {
            let es: Vec<&StatusEntry> = entries.iter().filter(|e| !e.lossy).collect();
            if es.is_empty() {
                return Vec::new();
            }
            let mut v = Vec::new();
            if es.iter().any(|e| unstaged(e)) {
                v.push(A::Stage);
            }
            if es.iter().any(|e| staged(e)) {
                v.push(A::Unstage);
            }
            if es.iter().any(|e| !conflicted(e)) {
                v.push(A::Discard);
            }
            if es.iter().any(|e| conflicted(e)) {
                v.extend([A::Ours, A::Theirs, A::Resolved, A::RedoMerge]);
            }
            if es.len() == 1 {
                v.push(A::Editor);
            }
            v
        }
        Target::Repository => {
            let mut v = Vec::new();
            if any_remote {
                v.extend([A::Fetch, A::FetchRemote, A::PruneRemote]);
            }
            v.push(A::AddRemote);
            if any_remote {
                v.extend([A::EditRemote, A::DeleteRemote]);
            }
            if repo.refs.iter().any(|r| r.kind == RefKind::RemoteBranch) {
                v.push(A::RemoteVisibility);
            }
            v.extend([
                A::User,
                A::UserRemove,
                A::Refresh,
                A::Find,
                A::Search,
                A::Filters,
                A::Log,
                A::Backups,
                A::Undo,
            ]);
            v
        }
    }
}

/// An item's words on its target: the stash's `Copy stash@{n}`, the uncommitted row's
/// `Stage all`.
fn item_label(id: ActionId, target: &Target) -> String {
    use ActionId as A;
    match (id, target) {
        (A::CopyName, Target::Stash(s)) => format!("Copy {}", s.selector),
        (A::CopyHash, Target::Stash(_)) => "Copy the hash".to_string(),
        (A::Stage, Target::Uncommitted) => "Stage all".to_string(),
        (A::Unstage, Target::Uncommitted) => "Unstage all".to_string(),
        _ => label(id).to_string(),
    }
}

/// The key of an item on its target (03 §9.1's slots, §9.2's Cmd/Ctrl letters).
fn key_of(id: ActionId, target: &Target) -> Option<String> {
    use ActionId as A;
    let command = |shift: bool, letter: char| {
        let mods = if shift {
            Mods::CommandShift
        } else {
            Mods::Command
        };
        format!("{}{letter}", mods.prefix())
    };
    let f = |n: u8| Some(format!("F{n}"));
    match (id, target) {
        (A::Rename, Target::Label(_)) => f(2),
        (A::TagDetails, _) => f(3),
        (A::Editor, _) => f(4),
        (A::Pick, Target::Commit { .. }) | (A::StashApply, _) => f(5),
        (A::Rebase, Target::Commit { .. } | Target::Label(_)) => f(6),
        (A::Branch, Target::Commit { .. }) | (A::Commit, _) | (A::StashBranch, _) => f(7),
        (
            A::DeleteBranch
            | A::DeleteRemoteBranch
            | A::DeleteTag
            | A::ResetChanges
            | A::Discard
            | A::StashDrop,
            _,
        ) => f(8),
        (A::Fetch, _) => Some(command(true, 'R')),
        (A::Refresh, _) => Some(command(false, 'R')),
        (A::Find, _) => Some(command(false, 'F')),
        (A::Search, _) => Some(command(true, 'F')),
        (A::Log, _) => Some(command(false, 'L')),
        (A::Backups, _) => Some(command(false, 'B')),
        (A::Undo, _) => Some(command(false, 'Z')),
        _ => None,
    }
}

/// `target`'s menu (03 §7): its items in the tables' order.
pub fn items(target: &Target, repo: &Repo) -> Vec<Item> {
    ids(target, repo)
        .into_iter()
        .map(|id| Item {
            id,
            label: item_label(id, target),
            key: key_of(id, target),
        })
        .collect()
}

/// What the menu's frame calls the object.
fn title(target: &Target) -> String {
    match target {
        Target::Commit { commit, .. } => {
            let subject: String = commit.subject.chars().take(40).collect();
            super::graph_view::printable(&format!("{} {subject}", commit.oid.short())).into_owned()
        }
        Target::Label(r) => super::graph_view::printable(r.name()).into_owned(),
        Target::Stash(s) => s.selector.clone(),
        Target::Uncommitted => "Uncommitted changes".to_string(),
        Target::Files(es) => match &es[..] {
            [one] => super::graph_view::printable(&one.path).into_owned(),
            many => format!("{} files", many.len()),
        },
        Target::Repository => "Repository".to_string(),
    }
}

/// How the menu ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEnd {
    /// Enter or a click on an item: its id, on this target (the repository's after
    /// `Repository…`).
    Chose { id: ActionId, target: Target },
    /// Esc, F10 or a click outside it.
    Cancel,
}

/// The menu of one object, open.
#[derive(Debug, Clone)]
pub struct Menu {
    target: Target,
    title: String,
    items: Vec<Item>,
    /// The repository's items, for `Repository…` (none on the repository's own menu).
    repository: Vec<Item>,
    /// The line under the menu's bar (`items.len()`: `Repository…`).
    bar: usize,
    /// The bar moved by a key: the next pass shows it.
    moved: bool,
}

/// Why a menu has no item.
const NOTHING: &str = "nothing to do here: a name that is not valid UTF-8 names nothing";

/// The line that turns a menu into the repository's.
const REPOSITORY: &str = "Repository…";

fn menu_id() -> egui::Id {
    egui::Id::unique("sopen-git-menu")
}

impl Menu {
    /// `target`'s menu on `repo`, the bar on its first item.
    pub fn new(target: Target, repo: &Repo) -> Menu {
        let repository = match target {
            Target::Repository => Vec::new(),
            _ => items(&Target::Repository, repo),
        };
        Menu {
            title: title(&target),
            items: items(&target, repo),
            target,
            repository,
            bar: 0,
            moved: false,
        }
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Its lines: the items, then `Repository…` unless this is the repository's menu.
    fn lines(&self) -> usize {
        self.items.len() + usize::from(!self.repository.is_empty())
    }

    /// The line under the bar chosen: its id, or the repository's menu in its place.
    fn choose(&mut self, at: usize) -> Option<MenuEnd> {
        match self.items.get(at) {
            Some(item) => Some(MenuEnd::Chose {
                id: item.id,
                target: self.target.clone(),
            }),
            None if at == self.items.len() && !self.repository.is_empty() => {
                self.target = Target::Repository;
                self.title = title(&self.target);
                self.items = std::mem::take(&mut self.repository);
                self.bar = 0;
                None
            }
            None => None,
        }
    }

    /// One pass: its keys are taken before anything else sees them.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<MenuEnd> {
        let take = |key: Key| ctx.input_mut(|i| keys::take(i, key, Mods::None));
        if take(Key::Escape) || take(Key::F10) {
            return Some(MenuEnd::Cancel);
        }
        let last = self.lines().saturating_sub(1);
        let page = 8;
        for (key, to) in [
            (Key::ArrowUp, self.bar.saturating_sub(1)),
            (Key::ArrowDown, (self.bar + 1).min(last)),
            (Key::PageUp, self.bar.saturating_sub(page)),
            (Key::PageDown, (self.bar + page).min(last)),
            (Key::Home, 0),
            (Key::End, last),
        ] {
            if take(key) {
                self.bar = to;
                self.moved = true;
            }
        }
        let mut chosen = take(Key::Enter).then_some(self.bar);
        let cells = self
            .items
            .iter()
            .map(|i| i.label.chars().count() + i.key.as_ref().map_or(0, |k| k.chars().count() + 4))
            .max()
            .unwrap_or(0)
            .max(NOTHING.chars().count().min(40))
            .max(24);
        let width = (cells as f32 * chrome::CELL.x).min(ctx.content_rect().width() * 0.8);
        let (bar, moved) = (self.bar, std::mem::take(&mut self.moved));
        let response = chrome::dialog(ctx, menu_id(), &self.title, |ui| {
            ui.set_width(width);
            egui::ScrollArea::vertical()
                .max_height(ctx.content_rect().height() * 0.7)
                .show(ui, |ui| {
                    for (k, item) in self.items.iter().enumerate() {
                        let mut button = egui::Button::selectable(k == bar, item.label.as_str())
                            .min_size(egui::vec2(width, chrome::CELL.y));
                        if let Some(key) = &item.key {
                            button = button.right_text(key.as_str());
                        }
                        let r = ui.add(button);
                        if moved && k == bar {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            chosen = Some(k);
                        }
                    }
                    if self.items.is_empty() {
                        ui.add(egui::Label::new(NOTHING).wrap());
                    }
                });
            if !self.repository.is_empty() {
                ui.add_space(chrome::CELL.y / 2.0);
                let at = self.items.len();
                let button = egui::Button::selectable(at == bar, REPOSITORY)
                    .min_size(egui::vec2(width, chrome::CELL.y));
                if ui.add(button).clicked() {
                    chosen = Some(at);
                }
            }
        });
        if let Some(at) = chosen {
            return self.choose(at);
        }
        response.should_close().then_some(MenuEnd::Cancel)
    }
}

// ---------------------------------------------------------------------------------------------
// The commit menu's history row (03 §7.1's last row, §7.8.1; S4-B). S3-B's items above are
// unchanged: [`HistoryMenu`] holds S3-B's [`Menu`] and gives its [`MenuEnd`] back as it is, so
// what matches on `MenuEnd` keeps working. Each item below carries its own `dead_code`
// allowance, which S4-C removes when it wires the row.

/// One line of the commit menu's history row.
#[allow(dead_code)] // S4-C removes this when it wires the history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub id: EditId,
    /// What the menu calls it (03 §7.8.1's Action column).
    pub label: String,
    /// Its key (03 §9.1: F2, F8; §9.2: Cmd/Ctrl+E).
    pub key: Option<String>,
}

/// What 03 §7.8.1 calls a history edit (its Action column).
#[allow(dead_code)] // S4-C removes this when it wires the history row.
pub fn history_label(id: EditId) -> &'static str {
    match id {
        EditId::Reword => "Reword…",
        EditId::Author => "Author and date…",
        EditId::Squash => "Squash into the commit below…",
        EditId::Fixup => "Fix up into the commit below",
        EditId::MoveUp => "Move up",
        EditId::MoveDown => "Move down",
        EditId::Drop => "Drop…",
        EditId::Edit => "Stop here to edit…",
        EditId::Split => "Split…",
        EditId::Fold => "Fold the staged changes into this commit…",
        EditId::Plan => "Edit history from here…",
    }
}

/// A history edit's key on a commit row: F2 `reword`, F8 `drop` (03 §9.1), Cmd/Ctrl+E the
/// planner (§9.2).
#[allow(dead_code)] // S4-C removes this when it wires the history row.
fn history_key(id: EditId) -> Option<String> {
    match id {
        EditId::Reword => Some("F2".to_string()),
        EditId::Drop => Some("F8".to_string()),
        EditId::Plan => Some(format!("{}E", Mods::Command.prefix())),
        _ => None,
    }
}

/// What the history row needs to know of a commit that `Repo` does not hold: the graph's
/// answers, which the integration (S4-C) fills from the rows it has read. The default is a
/// commit with no history edit.
#[allow(dead_code)] // S4-C removes this when it wires the history row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Eligibility {
    /// The commit is HEAD or an ancestor of HEAD (03 §7.8: history edits rewrite those).
    pub ancestor: bool,
    /// No commit from it up to HEAD, itself included, is a merge (more than one parent): the
    /// range a quick edit rewrites (03 §7.8.2: a range with merges is refused).
    pub linear: bool,
    /// The commit below it (its first parent) is a merge: `squash`, `fixup` and `move-down`
    /// rewrite that one too (their range starts below it, `rebase::Quick::apply`).
    pub below_merge: bool,
}

/// The history row of `commit`'s menu (03 §7.8.1), in its table's order, as `eligible` allows.
/// Offered or absent, never greyed (S3-B's rule):
/// - nothing off the ancestors of HEAD, nor with HEAD unborn (03 §7.8);
/// - the quick edits only on a linear range (03 §7.8.2 refuses a range with merges, G-H4), and
///   `squash`, `fixup` and `move-down` not above a merge, whose range holds it;
/// - `plan` on any ancestor but HEAD, merges or not: the planner opens on the range and names
///   its merges as a check that fails (03 §7.8.2), so a base above the newest merge can be
///   chosen;
/// - nothing above HEAD to move up past or to plan; nothing below a root commit to squash or fix
///   up into or to move down past; `fold` only with staged changes. `split` stays on a root
///   commit, where its plan refuses it with the reason (G-H3).
#[allow(dead_code)] // S4-C removes this when it wires the history row.
pub fn history_items(commit: &CommitRow, eligible: Eligibility, repo: &Repo) -> Vec<HistoryItem> {
    let Some(head) = repo.head.oid().filter(|_| eligible.ancestor) else {
        return Vec::new();
    };
    let is_head = *head == commit.oid;
    let root = commit.parents.is_empty();
    let any_staged = repo.status.entries.iter().any(|e| !e.lossy && staged(e));
    EditId::ALL
        .into_iter()
        .filter(|id| match id {
            EditId::Plan => !is_head,
            _ if !eligible.linear => false,
            EditId::Squash | EditId::Fixup | EditId::MoveDown => !root && !eligible.below_merge,
            EditId::MoveUp => !is_head,
            EditId::Fold => any_staged,
            _ => true,
        })
        .map(|id| HistoryItem {
            id,
            label: history_label(id).to_string(),
            key: history_key(id),
        })
        .collect()
}

/// How the commit menu with its history row ended.
#[allow(dead_code)] // S4-C removes this when it wires the history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryMenuEnd {
    /// S3-B's menu ended (one of its items, `Repository…`'s menu, Esc): its [`MenuEnd`] as it is.
    Menu(MenuEnd),
    /// A history item: its id on this commit, for `planner::QuickDialog::open` (`plan`: the
    /// planner on the commit's row).
    Edit { id: EditId, commit: CommitRow },
}

/// A menu with the history row: S3-B's [`Menu`] of the target, then, on a commit row, §7.8's
/// items, then `Repository…` (03 §7.1's last row). Another target's is S3-B's menu as it is.
#[allow(dead_code)] // S4-C removes this when it wires the history row.
#[derive(Debug, Clone)]
pub struct HistoryMenu {
    menu: Menu,
    /// The commit whose history row this is (none once `Repository…` was chosen).
    commit: Option<CommitRow>,
    history: Vec<HistoryItem>,
    /// The line under the bar: S3-B's items, then the history row, then `Repository…`.
    bar: usize,
    /// The bar moved by a key: the next pass shows it.
    moved: bool,
}

#[allow(dead_code)] // S4-C removes this when it wires the history row.
impl HistoryMenu {
    /// `target`'s menu on `repo` with the history row ([`history_items`]; `eligible`: what the
    /// graph says of a commit target), the bar on its first item.
    pub fn new(target: Target, eligible: Eligibility, repo: &Repo) -> HistoryMenu {
        let (commit, history) = match &target {
            Target::Commit { commit, .. } => {
                let items = history_items(commit, eligible, repo);
                (Some(commit.clone()), items)
            }
            _ => (None, Vec::new()),
        };
        HistoryMenu {
            menu: Menu::new(target, repo),
            commit,
            history,
            bar: 0,
            moved: false,
        }
    }

    /// S3-B's menu inside.
    pub fn menu(&self) -> &Menu {
        &self.menu
    }

    pub fn history(&self) -> &[HistoryItem] {
        &self.history
    }

    /// Its lines: S3-B's items, the history row, then `Repository…` unless this is the
    /// repository's menu.
    fn lines(&self) -> usize {
        self.menu.items.len() + self.history.len() + usize::from(!self.menu.repository.is_empty())
    }

    /// The line `at` chosen.
    fn choose(&mut self, at: usize) -> Option<HistoryMenuEnd> {
        let n = self.menu.items.len();
        if at < n {
            return self.menu.choose(at).map(HistoryMenuEnd::Menu);
        }
        if let Some(item) = self.history.get(at - n) {
            let commit = self.commit.clone()?;
            return Some(HistoryMenuEnd::Edit {
                id: item.id,
                commit,
            });
        }
        if at == n + self.history.len() && !self.menu.repository.is_empty() {
            // `Repository…`: S3-B's menu turns into the repository's; the history row goes.
            self.history.clear();
            self.commit = None;
            self.bar = 0;
            return self.menu.choose(n).map(HistoryMenuEnd::Menu);
        }
        None
    }

    /// One pass, as S3-B's [`Menu::ui`]: ↑ ↓ PgUp PgDn Home End move the bar over all its lines,
    /// Enter or a click chooses, Esc or F10 closes; its keys are taken before anything else sees
    /// them.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<HistoryMenuEnd> {
        let take = |key: Key| ctx.input_mut(|i| keys::take(i, key, Mods::None));
        if take(Key::Escape) || take(Key::F10) {
            return Some(HistoryMenuEnd::Menu(MenuEnd::Cancel));
        }
        let last = self.lines().saturating_sub(1);
        let page = 8;
        for (key, to) in [
            (Key::ArrowUp, self.bar.saturating_sub(1)),
            (Key::ArrowDown, (self.bar + 1).min(last)),
            (Key::PageUp, self.bar.saturating_sub(page)),
            (Key::PageDown, (self.bar + page).min(last)),
            (Key::Home, 0),
            (Key::End, last),
        ] {
            if take(key) {
                self.bar = to;
                self.moved = true;
            }
        }
        let mut chosen = take(Key::Enter).then_some(self.bar);
        let cells = self
            .menu
            .items
            .iter()
            .map(|i| (i.label.as_str(), i.key.as_deref()))
            .chain(
                self.history
                    .iter()
                    .map(|i| (i.label.as_str(), i.key.as_deref())),
            )
            .map(|(label, key)| label.chars().count() + key.map_or(0, |k| k.chars().count() + 4))
            .max()
            .unwrap_or(0)
            .max(NOTHING.chars().count().min(40))
            .max(24);
        let width = (cells as f32 * chrome::CELL.x).min(ctx.content_rect().width() * 0.8);
        let (bar, moved) = (self.bar, std::mem::take(&mut self.moved));
        let n = self.menu.items.len();
        let line = |ui: &mut egui::Ui, at: usize, label: &str, key: Option<&str>| {
            let mut button = egui::Button::selectable(at == bar, label)
                .min_size(egui::vec2(width, chrome::CELL.y));
            if let Some(key) = key {
                button = button.right_text(key);
            }
            let r = ui.add(button);
            if moved && at == bar {
                r.scroll_to_me(None);
            }
            r.clicked()
        };
        let response = chrome::dialog(ctx, menu_id(), &self.menu.title, |ui| {
            ui.set_width(width);
            // As tall as it needs up to the most it may take, at once: without the floor egui
            // grows a new modal's scroll area over several passes (about 20 lines at first).
            let most = ctx.content_rect().height() * 0.7;
            egui::ScrollArea::vertical()
                .max_height(most)
                .min_scrolled_height(most)
                .show(ui, |ui| {
                    for (k, item) in self.menu.items.iter().enumerate() {
                        if line(ui, k, &item.label, item.key.as_deref()) {
                            chosen = Some(k);
                        }
                    }
                    if !self.history.is_empty() {
                        ui.add_space(chrome::CELL.y / 2.0);
                    }
                    for (k, item) in self.history.iter().enumerate() {
                        if line(ui, n + k, &item.label, item.key.as_deref()) {
                            chosen = Some(n + k);
                        }
                    }
                    if self.menu.items.is_empty() && self.history.is_empty() {
                        ui.add(egui::Label::new(NOTHING).wrap());
                    }
                });
            if !self.menu.repository.is_empty() {
                ui.add_space(chrome::CELL.y / 2.0);
                let at = n + self.history.len();
                if line(ui, at, REPOSITORY, None) {
                    chosen = Some(at);
                }
            }
        });
        if let Some(at) = chosen {
            return self.choose(at);
        }
        response
            .should_close()
            .then_some(HistoryMenuEnd::Menu(MenuEnd::Cancel))
    }
}
