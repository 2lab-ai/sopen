//! The action menu (03 §7, §9: F10, Shift+F10, a right click): the actions of the object under the
//! bar — a commit row, a label, a stash row, the uncommitted row and its files, the repository —
//! by their `git::actions::ActionId`s.
//!
//! [`items`] is the menu of a [`Target`], in 03 §7's table order, with what each item is called
//! and its key; an action is offered only where its table says it applies (`checkout-branch` not
//! on the checked-out branch, `fetch-into` only beside a local branch that is not checked out, no
//! remote action without a remote, nothing that names a ref or a path that is not UTF-8). Every
//! menu but the repository's ends in `Repository…`, which turns it into the repository's menu.
//! Choosing gives the id and its target back; [`super::dialogs::ActionDialog::open`] makes the
//! dialog of it, and the views among the ids ([`is_view`]) are the panel's to carry out.
//!
//! **In cells** (X4-menu, 10 §17): the menu is an `ops::dialog` box on the panels' grid, its
//! lines one list of the kit's — each item with its key at the right, then a commit's history
//! row, then `Repository…` — with the bar, ↑ ↓ PgUp PgDn Home End moving it and the rows it shows
//! following it, the wheel and ↑ ░ ↓ on the box's border scrolling them. It is as wide as its
//! widest line by 10 §6's rule (24 cells at least, as egui's was), and its list shows at most
//! seven tenths of the grid's rows, as egui's 70 % of the window, giving way where the grid is
//! shorter. No line stands between the groups: the kit's list has no line the bar passes over. A
//! commit's title is its subject cut at 40 cells, and the no-item line wraps at 40 as egui's did.
//! Enter or a click chooses, as egui's menu; Space chooses nothing (in the panel it marks, 03
//! §9.2), though the kit's list would take it. Esc, F10 or a click outside closes it; its keys
//! are taken before anything else sees them, and an Enter, Space, Esc or F10 it does not act on
//! is dropped, never left for what is under it. A key at an
//! item's right is that item's key in the panel, not while the menu is open (as egui's menu). Only
//! what the screen showed is chosen: a choice acts only on the lines the last pass that reached
//! the screen painted (`cells::OnScreen`: a terminal prints no pass while input waits), in the
//! box where it painted them, so a pass's choices after `Repository…` made the lines anew are
//! dropped (the menu's lines change no other way: a refresh does not touch an open menu), a box
//! laid elsewhere (the grid resized) chooses nothing in that pass, and presses after a Tab in
//! the pass are the kit's to drop. Its ids are one set for the window:
//! the menu has no panel's column (the panel hands its dialogs the panels' grid alone,
//! `GitView::modals`).

use super::filters::split_remote;
use super::in_progress::{self, StopStep};
use crate::git::actions::{ActionId, Proceed, Repo};
use crate::git::model::{
    CommitRow, Head, ObjectType, RefKind, RefLabel, StashRow, StatusEntry, StatusState,
};
use crate::git::rebase::EditId;
use crate::gui::browser::keys::{self, Command, Mods};
use crate::gui::browser::ops::dialog::{self as kit, Bar, Ink, Input, Inputs, Part, Row, Rows};
use crate::gui::cells::{self, Cells, Grid};
use eframe::egui::{self, Key};
use std::sync::Arc;

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
            // The subject cut by cells, a Hangul one at 19 characters and `…` (egui's took 40
            // characters of any width).
            let subject = super::graph_view::printable(&commit.subject);
            format!("{} {}", commit.oid.short(), cells::fit_start(&subject, 40))
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
    /// A line of the repository's menu that is a key of the panel's ([`Step`]): its command, for
    /// the panel to run as that key runs it.
    Key(Command),
    /// Esc, F10 or a click outside it.
    Cancel,
}

/// A line of the repository's menu that runs one of the panel's Cmd/Ctrl+Shift keys (10 §9,
/// T17): a terminal without the kitty protocol loses the Shift of those chords, so the menu holds
/// each, chosen as its key runs it — the banner's steps the state offers (Continue, Skip, Abort,
/// bisect's Reset), an edit stop's Amend and Split (`Edit::SplitStop`, not the history row's
/// quick split), and the panel over both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub command: Command,
    pub label: String,
    /// Its key, at the line's right end: `Ctrl/⌘+Shift+C`.
    pub key: String,
}

/// The repository's key lines on `repo`: those before its items — the banner's steps the state
/// offers, then an edit stop's — and those after them (the panel over both sides). The banner and
/// the edit stop are the panel's own (`GitView::set_repo` reads them so from the snapshot).
fn steps_of(repo: &Repo) -> (Vec<Step>, Vec<Step>) {
    let banner = in_progress::BannerView::probe(&repo.in_progress, &repo.status);
    let stop = in_progress::EditStop::probe(&repo.in_progress, &repo.status, repo.head.oid());
    let mut before = Vec::new();
    for p in banner.as_ref().map_or(&[][..], |b| b.offers()) {
        let (command, id) = match p {
            Proceed::Continue => (Command::Continue, ActionId::Continue),
            Proceed::Skip => (Command::Skip, ActionId::Skip),
            Proceed::Abort => (Command::Abort, ActionId::Abort),
            Proceed::BisectReset => (Command::BisectReset, ActionId::BisectReset),
        };
        before.push(Step {
            command,
            label: label(id).to_string(),
            key: in_progress::key_text(*p),
        });
    }
    for s in stop.map_or_else(Vec::new, |s| s.offers()) {
        let (command, words) = match s {
            StopStep::Amend => (Command::Amend, "Amend the stopped commit…"),
            StopStep::Split => (Command::Split, "Split the stopped commit…"),
        };
        before.push(Step {
            command,
            label: words.to_string(),
            key: in_progress::stop_key_text(s),
        });
    }
    let after = vec![Step {
        command: Command::Wide,
        label: "The git panel over both sides, and back".to_string(),
        key: format!("{}G", Mods::CommandShift.prefix()),
    }];
    (before, after)
}

/// The menu of one object, open.
#[derive(Debug, Clone)]
pub struct Menu {
    target: Target,
    title: String,
    items: Vec<Item>,
    /// The repository's items, for `Repository…` (none on the repository's own menu).
    repository: Vec<Item>,
    /// The repository's key lines, before its items and after them ([`steps_of`]): the
    /// repository's menu shows them, another keeps them for `Repository…`.
    steps: (Vec<Step>, Vec<Step>),
    /// Its lines in cells: the items, then `Repository…` unless this is the repository's menu.
    list: List,
}

/// A menu's lines in cells (X4-menu): the kit's rows, the bar, and which rows the screen shows —
/// a choice acts only on the rows the owner saw.
#[derive(Debug, Clone)]
struct List {
    rows: Arc<Rows>,
    bar: Bar,
    /// How many times the rows were made anew (`Repository…`): which rows these are.
    made: u64,
    /// The `made` of the rows and where the box stood, as the last pass that reached the screen
    /// painted them (`cells::OnScreen`: a terminal prints no pass while input waits).
    screen: cells::OnScreen<(u64, Vec<Cells>)>,
}

impl List {
    fn new(rows: Arc<Rows>) -> List {
        List {
            rows,
            bar: Bar::default(),
            made: 0,
            screen: cells::OnScreen::default(),
        }
    }

    /// The rows made anew, the bar on the first: none of them is painted yet.
    fn renew(&mut self, rows: Arc<Rows>) {
        self.rows = rows;
        self.bar = Bar::default();
        self.made += 1;
    }
}

/// A menu's rows: its key lines before its items, the items, the key lines after them, each
/// with its key at the right ([`Menu::lines`]), a commit's `history` row, then `Repository…`
/// where `repository`.
fn rows_of(menu: &Menu, history: &[HistoryItem], repository: bool) -> Arc<Rows> {
    let row = |label: &str, key: &Option<String>| Row::new(label, key.as_deref().unwrap_or(""));
    let (before, items, after) = menu.lines();
    let step = |s: &Step| Row::new(&s.label, &s.key);
    let mut all: Vec<Row> = before.iter().map(step).collect();
    all.extend(items.iter().map(|i| row(&i.label, &i.key)));
    all.extend(after.iter().map(step));
    all.extend(history.iter().map(|i| row(&i.label, &i.key)));
    if repository {
        all.push(Row::new(REPOSITORY, ""));
    }
    Rows::new(all)
}

/// Why a menu has no item.
const NOTHING: &str = "nothing to do here: a name that is not valid UTF-8 names nothing";

/// The cells [`NOTHING`] is wrapped at: egui's menu was 40 wide for it.
const NOTHING_CELLS: usize = 40;

/// The line that turns a menu into the repository's.
const REPOSITORY: &str = "Repository…";

/// A menu's nominal width inside, in cells: egui's narrowest (10 §6's rule widens it to its
/// widest line, at most 76).
const NOMINAL: usize = 24;

fn menu_id() -> egui::Id {
    egui::Id::unique("sopen-git-menu")
}

fn list_id() -> egui::Id {
    menu_id().with("list")
}

/// The box of a menu titled `title` with `list`, on `grid` (the panels'): [`NOTHING`] first when
/// it has no item, then its lines, at most seven tenths of the grid's rows of them on screen.
fn layout(grid: Grid, title: &str, list: &List, nothing: bool) -> kit::Layout {
    let most = u16::try_from(u32::from(grid.rows) * 7 / 10).unwrap_or(u16::MAX);
    let mut parts = Vec::new();
    if nothing {
        // Wrapped at 40 cells, as egui's menu wrapped it, so the box keeps egui's width.
        let rows = kit::wrap(NOTHING, NOTHING_CELLS).into_iter();
        parts.extend(rows.map(|row| Part::Text(row, Ink::Text)));
    }
    if !list.rows.rows().is_empty() {
        parts.push(Part::List {
            rows: list.rows.clone(),
            id: list_id(),
            most: most.max(1),
        });
    }
    kit::layout(grid, title, NOMINAL, &parts)
}

/// What a pass of a menu's lines did ([`pass`]).
enum Pass {
    /// Esc, F10 or a click outside the box.
    Cancel,
    /// The lines chosen, in order — none where the pass before painted other rows, or the box
    /// elsewhere — and the pass's events after its first Enter, set aside ([`set_aside`]).
    Chose(Vec<usize>, Vec<egui::Event>),
}

/// The pass's events after its first plain Enter, taken out of its input: the Enter may choose a
/// line that opens a dialog, and what was typed after it is that dialog's ([`hand_on`]).
fn set_aside(ctx: &egui::Context) -> Vec<egui::Event> {
    ctx.input_mut(|i| {
        let enter = i.events.iter().position(|e| {
            matches!(
                e,
                egui::Event::Key {
                    key: Key::Enter,
                    pressed: true,
                    modifiers,
                    ..
                } if Mods::None.matches(*modifiers)
            )
        });
        match enter {
            Some(at) => i.events.split_off(at + 1),
            None => Vec::new(),
        }
    })
}

/// The events [`set_aside`] took, back in the pass after the menu's own: in their order for the
/// dialog a choice opened — an action's (`GitView::dialog`) or a quick edit's (`quick`), which
/// `GitView::modals` shows in the same pass, as if they were typed once it was open (10 §17,
/// X4-menu); a dialog that opens after the integration's reads (a prefilled one, a history edit's
/// range), the result, the log's entry or the filters gets none of them — but no Enter, which would run a dialog never shown (only what showed
/// runs); with no choice, without the pass's Enters and Spaces, which would have been the menu's
/// and come after a choice it dropped — the first Enter's too, when the list did not take it
/// (after a Tab). The menu's keyboard goes with a choice, so the dialog's field
/// takes the typing in that pass as it does whenever nothing has it.
fn hand_on(ctx: &egui::Context, rest: Vec<egui::Event>, chose: bool) {
    ctx.input_mut(|i| i.events.extend(rest));
    if chose {
        drop_presses(ctx, &[Key::Enter]);
        ctx.memory_mut(|m| m.surrender_focus(list_id()));
    } else {
        drop_presses(ctx, &[Key::Enter, Key::Space]);
    }
}

/// Every Esc and F10 left in the pass, taken out of its input: whether there was one.
fn closes(ctx: &egui::Context) -> bool {
    let take = |key: Key| ctx.input_mut(|i| keys::take(i, key, Mods::None));
    let mut close = false;
    while take(Key::Escape) || take(Key::F10) {
        close = true;
    }
    close
}

/// The end of a pass that chose `end` with `rest` set aside: the rest handed on; with no choice,
/// an Esc or F10 among it closes the menu (it came after an Enter that chose nothing).
fn finish<E>(ctx: &egui::Context, rest: Vec<egui::Event>, end: Option<E>, cancel: E) -> Option<E> {
    hand_on(ctx, rest, end.is_some());
    if end.is_none() && closes(ctx) {
        ctx.memory_mut(|m| m.surrender_focus(list_id()));
        return Some(cancel);
    }
    end
}

/// The pass's plain presses of `dropped`, and a Space's text, taken out of its input: the menu
/// has the keyboard, so none reaches what is under it.
fn drop_presses(ctx: &egui::Context, dropped: &[Key]) {
    ctx.input_mut(|i| {
        let mut typed = false;
        i.events.retain(|e| match (e, std::mem::take(&mut typed)) {
            (egui::Event::Text(text), true) if text == " " => false,
            (
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                },
                _,
            ) if dropped.contains(key) && Mods::None.matches(*modifiers) => {
                typed = *key == Key::Space;
                false
            }
            _ => true,
        });
    });
}

/// One pass of a menu titled `title` with `list` on `grid` (see the module's notes): every Esc
/// and F10 of the pass closes it; else the box, its list's keys and clicks, and the lines chosen.
fn pass(ctx: &egui::Context, grid: Grid, title: &str, list: &mut List, nothing: bool) -> Pass {
    // What the screen shows as the owner pressed this pass's keys: the last painting that
    // reached it.
    let seen = list.screen.start(ctx).cloned();
    // What follows the pass's first Enter waits: it may be the dialog's that Enter opens.
    let rest = set_aside(ctx);
    if closes(ctx) {
        // Its Enters and Spaces too, the rest's among them: the closing pass leaves none for what
        // is under it.
        hand_on(ctx, rest, false);
        ctx.memory_mut(|m| m.surrender_focus(list_id()));
        return Pass::Cancel;
    }
    // The list holds the keyboard; egui drops a focus its layer cannot take yet (a new box's
    // first pass), so it is asked for again while it is elsewhere.
    if !ctx.memory(|m| m.has_focus(list_id())) {
        ctx.memory_mut(|m| m.request_focus(list_id()));
    }
    // Space chooses nothing: egui's menu chose on Enter or a click only, and in the panel Space
    // marks (03 §9.2), so a Space meant for the rows never runs an item.
    drop_presses(ctx, &[Key::Space]);
    let laid = layout(grid, title, list, nothing);
    let inputs = Inputs {
        by_id: vec![(list_id(), Input::List(&mut list.bar))],
        ..Inputs::default()
    };
    let shown = kit::show(ctx, menu_id(), &laid, inputs);
    // What the owner saw when they chose: the rows the screen shows, in the box where it shows
    // them. egui gives a click to the widget the pass before laid out under the pointer (its hit
    // test reads the last pass's widgets, `Context::begin_pass`), but the list finds its row by
    // where it stands now: a box laid elsewhere (the grid resized) has another line there.
    let now = (list.made, laid.covers());
    let seen = seen.as_ref() == Some(&now);
    list.screen.painted(now);
    if shown.modal.should_close() {
        hand_on(ctx, rest, false);
        return Pass::Cancel;
    }
    match seen {
        true => Pass::Chose(shown.chosen(list_id()), rest),
        false => Pass::Chose(Vec::new(), rest),
    }
}

impl Menu {
    /// `target`'s menu on `repo`, the bar on its first item.
    pub fn new(target: Target, repo: &Repo) -> Menu {
        let repository = match target {
            Target::Repository => Vec::new(),
            _ => items(&Target::Repository, repo),
        };
        let mut menu = Menu {
            title: title(&target),
            list: List::new(Rows::new(Vec::new())),
            items: items(&target, repo),
            target,
            repository,
            steps: steps_of(repo),
        };
        let repository = !menu.repository.is_empty();
        menu.list = List::new(rows_of(&menu, &[], repository));
        menu
    }

    /// Its lines before the history row: the key lines before its items, the items, the key
    /// lines after them — the key lines only on the repository's menu.
    fn lines(&self) -> (&[Step], &[Item], &[Step]) {
        match self.target {
            Target::Repository => (&self.steps.0, &self.items, &self.steps.1),
            _ => (&[], &self.items, &[]),
        }
    }

    /// How many lines [`Menu::lines`] holds.
    fn len(&self) -> usize {
        let (before, items, after) = self.lines();
        before.len() + items.len() + after.len()
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The line under the bar chosen: its id, a key line's command, or the repository's menu in
    /// its place (its lines made anew).
    fn choose(&mut self, at: usize) -> Option<MenuEnd> {
        let (before, items, after) = self.lines();
        let (b, n) = (before.len(), items.len());
        if let Some(step) = before.get(at) {
            return Some(MenuEnd::Key(step.command));
        }
        if let Some(item) = items.get(at - b) {
            return Some(MenuEnd::Chose {
                id: item.id,
                target: self.target.clone(),
            });
        }
        if let Some(step) = after.get(at - b - n) {
            return Some(MenuEnd::Key(step.command));
        }
        if at == self.len() && !self.repository.is_empty() {
            self.target = Target::Repository;
            self.title = title(&self.target);
            self.items = std::mem::take(&mut self.repository);
            let rows = rows_of(self, &[], false);
            self.list.renew(rows);
        }
        None
    }

    /// The cells of `grid` its box covers, as [`HistoryMenu::covers`] (S3-B's tests' menu).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        layout(*grid, &self.title, &self.list, self.len() == 0).covers()
    }

    /// One pass on `grid` (the panels'), in cells (see the module's notes): its keys are taken
    /// before anything else sees them. The panel draws [`HistoryMenu`], which holds this menu and
    /// draws it with a commit's history row through the same pass; S3-B's tests draw it alone.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ui(&mut self, ctx: &egui::Context, grid: Grid) -> Option<MenuEnd> {
        let nothing = self.len() == 0;
        let made = self.list.made;
        let (chosen, rest) = match pass(ctx, grid, &self.title, &mut self.list, nothing) {
            Pass::Cancel => return Some(MenuEnd::Cancel),
            Pass::Chose(chosen, rest) => (chosen, rest),
        };
        let mut end = None;
        for at in chosen {
            // `Repository…` made the lines anew: the pass's later choices were of the old ones.
            if self.list.made != made {
                break;
            }
            end = self.choose(at);
            if end.is_some() {
                break;
            }
        }
        finish(ctx, rest, end, MenuEnd::Cancel)
    }
}

// ---------------------------------------------------------------------------------------------
// The commit menu's history row (03 §7.1's last row, §7.8.1; S4-B). S3-B's items above are
// unchanged: [`HistoryMenu`] holds S3-B's [`Menu`] and gives its [`MenuEnd`] back as it is, so
// what matches on `MenuEnd` keeps working. S4-C wires it (`GitView`'s menu).

/// One line of the commit menu's history row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub id: EditId,
    /// What the menu calls it (03 §7.8.1's Action column).
    pub label: String,
    /// Its key (03 §9.1: F2, F8; §9.2: Cmd/Ctrl+E).
    pub key: Option<String>,
}

/// What 03 §7.8.1 calls a history edit (its Action column).
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
#[derive(Debug, Clone)]
pub struct HistoryMenu {
    menu: Menu,
    /// The commit whose history row this is (none once `Repository…` was chosen).
    commit: Option<CommitRow>,
    history: Vec<HistoryItem>,
}

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
        let mut menu = Menu::new(target, repo);
        // Its lines: S3-B's items, the history row, then `Repository…`.
        let repository = !menu.repository.is_empty();
        menu.list = List::new(rows_of(&menu, &history, repository));
        HistoryMenu {
            menu,
            commit,
            history,
        }
    }

    /// S3-B's menu inside.
    pub fn menu(&self) -> &Menu {
        &self.menu
    }

    /// The line `at` chosen.
    fn choose(&mut self, at: usize) -> Option<HistoryMenuEnd> {
        let n = self.menu.len();
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
            // `Repository…`: S3-B's menu turns into the repository's, its lines made anew; the
            // history row goes.
            self.history.clear();
            self.commit = None;
            return self.menu.choose(n).map(HistoryMenuEnd::Menu);
        }
        None
    }

    /// How many lines it offers to choose: the key lines, the items and the history row
    /// (`Repository…` aside), as the status row counts them.
    pub fn lines(&self) -> usize {
        self.menu.len() + self.history.len()
    }

    /// Whether it has no item to choose (only `Repository…`, if anything).
    fn nothing(&self) -> bool {
        self.menu.len() == 0 && self.history.is_empty()
    }

    /// The cells of `grid` (the panels') the menu covers, its box and its shadow, for the panel and
    /// the other side to blank (10 §17): the box as this pass lays it out, from the menu as it
    /// stands when the pass begins.
    pub fn covers(&self, grid: &Grid) -> Vec<Cells> {
        let m = &self.menu;
        layout(*grid, &m.title, &m.list, self.nothing()).covers()
    }

    /// One pass on `grid` (the panels'), in cells, as S3-B's [`Menu::ui`] (see the module's
    /// notes): ↑ ↓ PgUp PgDn Home End move the bar over all its lines, Enter or a click chooses,
    /// Esc or F10 closes; its keys are taken before anything else sees them.
    pub fn ui(&mut self, ctx: &egui::Context, grid: Grid) -> Option<HistoryMenuEnd> {
        let nothing = self.nothing();
        let made = self.menu.list.made;
        let m = &mut self.menu;
        let (chosen, rest) = match pass(ctx, grid, &m.title, &mut m.list, nothing) {
            Pass::Cancel => return Some(HistoryMenuEnd::Menu(MenuEnd::Cancel)),
            Pass::Chose(chosen, rest) => (chosen, rest),
        };
        let mut end = None;
        for at in chosen {
            // `Repository…` made the lines anew: the pass's later choices were of the old ones.
            if self.menu.list.made != made {
                break;
            }
            end = self.choose(at);
            if end.is_some() {
                break;
            }
        }
        finish(ctx, rest, end, HistoryMenuEnd::Menu(MenuEnd::Cancel))
    }
}
