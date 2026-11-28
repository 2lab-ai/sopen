//! The git panel (03; 02 §3): the graph of every commit and branch of the active folder's
//! repository on a side of the browser, read-only in S2 — the graph (03 §4.2–§4.4), the details
//! pane and its diffs (§6), find and filters (§4.6–§4.7). Its look is
//! [`crate::gui::chrome::git`]'s. S3 adds the menu, dialogs, backups, log and banner, S4 the
//! planner (02 §5).
//!
//! It draws from inputs of its own, not from git: a [`GitView`] is given the refs and HEAD
//! ([`GitView::set_refs`]), the rows of a load page by page ([`GitView::set_rows`],
//! [`GitView::append_rows`]) — `git::graph`'s [`Row`]s, laid out here with `git::graph`'s lane
//! layout — the paging state, the details asked for ([`GitView::set_details`]) and a history
//! search's results. Its keys ([`keys`], through the key table's git context) and [`draw`] give
//! [`GitAction`]s: what is left for the integration (S2-F) to read or to carry out. The bar, the
//! marks, the label cursor, Find, the filters' choices and the details pane's keyboard are the
//! view's own.
//!
//! S2-F wires it into the browser (`gui::browser::git_host`). S3-C adds the actions: given the
//! repository's snapshot ([`GitView::set_repo`]: a `git::actions::Repo` from the refresh, the
//! probe's state among it) the F-keys (03 §9.1), the menu, the banner's keys, Cmd/Ctrl+Z, B, L and
//! Shift+R act on the object under the bar — the label under the label cursor, the details pane's
//! file, else the bar's row — through S3-B's screens (`menu`, `dialogs`, `backups`, `log_view`,
//! `in_progress`): what runs git is a [`GitAction`] again (a dialog's prefill to read, an action
//! to run, a restore's entries, the Backups list, the log). A view with no snapshot offers no
//! action: its keys do what S2's do.
//!
//! S4-C adds the history edits (03 §7.8) through S4-B's screens: the commit menu's history row
//! (`menu::HistoryMenu`, offered as the loaded graph says — [`GitView::eligibility`]), F2 and F8
//! on a commit (`reword`, `drop`), Cmd/Ctrl+E and the planner over the body (`planner`, which
//! takes its own keys first: while it is open only the window's keys are looked up here), the
//! quick edits' dialogs, and an edit stop's Amend and Split on the banner (`in_progress`). What
//! runs git is a [`GitAction`] again: the reads a quick edit's dialog or the planner opens on, a
//! history edit to run.

mod backups;
mod details;
mod dialogs;
mod filters;
mod find;
mod graph_view;
mod in_progress;
mod lanes;
mod log_view;
mod menu;
mod planner;
mod state;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_f_git_ids;
#[cfg(test)]
mod tests_s3;
#[cfg(test)]
mod tests_s4;
// X4's units' own tests (10 §17, X4's rule for shared files): a file each.
#[cfg(test)]
mod tests_x4_dialogs;
#[cfg(test)]
mod tests_x4_dialogs2;
#[cfg(test)]
mod tests_x4_find;
#[cfg(test)]
mod tests_x4_graph;
#[cfg(test)]
mod tests_x4_lists;
#[cfg(test)]
mod tests_x4_menu;
#[cfg(test)]
mod tests_x4_pane;
#[cfg(test)]
mod tests_x4_planner;
#[cfg(test)]
mod tests_x4_planner2;
#[cfg(test)]
mod tests_x4_quick;

pub use details::{Details, FileGroup, FileRow, Group, Summary};
pub use dialogs::Prefill;
#[allow(unused_imports)]
pub use filters::{BranchChoice, Filters, Order};
pub use find::SearchQuery;
pub use menu::{history_label, label as action_label, Target};
pub use planner::Draft;
#[cfg(test)]
pub use {dialogs::PLAN_FIRST, planner::UPDATE_REFS};

use super::keys::{self, Command, Context, Mods, Step, Trigger};
use super::view::Mode;
use crate::git::actions::{self, Action, ActionId, Outcome, Plan, Proceed, Repo};
use crate::git::backup::{Record, RestoreEntry};
use crate::git::graph::{Graph, Row};
use crate::git::model::{BackupRef, CommitRow, ObjectType, Oid, RefKind, RefLabel, StatusEntry};
use crate::git::rebase::{self, Base, Edit, EditId, Model};
use crate::git::runner::LogEntry;
use crate::gui::cells::{self, Buffer, Cells, Grid, Pen};
use crate::gui::chrome::{self, FKey};
use crate::gui::DialogId;
use crate::gui::{help, Status};
use eframe::egui;
// The panel's test modules take `Key` from here (`use super::*`); the panel itself reads keys
// through `keys` since its dialogs moved to `dialogs.rs` (X4-dialogs2).
#[cfg(test)]
use eframe::egui::Key;
use std::collections::{HashMap, HashSet, VecDeque};

/// A row of the graph by what it draws: the bar across a reload, the marks and the details pane
/// name it, never a row index (the uncommitted row coming or going shifts every index, 03 §5).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// The uncommitted row.
    Uncommitted,
    /// A commit of the log.
    Commit(Oid),
    /// A stash row: the stash's own commit.
    Stash(Oid),
}

impl RowKey {
    /// The key of `row`.
    pub fn of(row: &Row) -> RowKey {
        match row {
            Row::Uncommitted { .. } => RowKey::Uncommitted,
            Row::Stash(stash) => RowKey::Stash(stash.oid.clone()),
            Row::Commit { commit, .. } => RowKey::Commit(commit.oid.clone()),
        }
    }
}

/// What the details pane shows (03 §6.1, §6.4), and what a details read is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    /// A row's own details: a commit's or a stash's summary and files, the uncommitted row's
    /// groups.
    Row(RowKey),
    /// Two rows compared: `from` the lower (older) row, `to` the upper (03 §6.4).
    Compare { from: RowKey, to: RowKey },
}

/// What HEAD is (03 §4.1's HEAD read), for the labels: `◙`, `►` and the detached `HEAD`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HeadRef {
    /// A branch is checked out: its refname (`refs/heads/main`).
    Branch(String),
    /// HEAD is detached at a commit: the `HEAD` label, first (03 §4.4).
    Detached(Oid),
    /// No commit yet.
    #[default]
    Unborn,
}

/// The branch's ahead / behind for the foot's right end: `main ↑2 ↓1 origin/main` (03 §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracking {
    /// The checked-out branch's short name.
    pub branch: String,
    /// Its upstream (`origin/main`).
    pub upstream: String,
    pub ahead: u64,
    pub behind: u64,
}

/// Where the log (or a history search's) stream stands: what the paging line (the foot) says
/// (03 §4.2, §4.5, §2.3, §4.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Paging {
    /// The first page is not in yet: `loading…`.
    #[default]
    Loading,
    /// More can be read: `N commits · M more`, `M` the next page's size (500). The bar coming
    /// within one screen of the last loaded row asks for it ([`GitAction::MoreRows`]).
    More(usize),
    /// The stream ended: `N commits · all loaded`.
    AllLoaded,
    /// The stream ended at a shallow clone's boundary: `history ends here (shallow clone)`.
    Shallow,
    /// An empty repository: `no commits yet` (03 §4.1).
    NoCommits,
    /// A filter left no ref, so no log ran: `no refs chosen` (03 §4.1).
    NoRefsChosen,
    /// A record that did not parse, or git failed: the error, on the paging line (03 §4.1).
    Error(String),
}

/// What a key or a click on the git panel asks of the integration (S2-F): reads to run and
/// things to carry out. Keys that only change the view (the bar, marks, the label cursor, Find,
/// the details pane's keyboard) give none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitAction {
    /// F1: the git panel's keys.
    Help,
    /// Enter or F3 on a row, a double click, or the bar moving while the pane follows it: read
    /// the details of the subject and hand them to [`GitView::set_details`]. A comparison when
    /// exactly one row is marked (03 §6.4).
    Details(Subject),
    /// The bar came within one screen of the last loaded row (or ↓ on the last row, or a click
    /// on the paging line): read the next page and hand it to [`GitView::append_rows`] (03 §4.5).
    MoreRows,
    /// History search (Cmd/Ctrl+Shift+F): run it and hand its rows to
    /// [`GitView::set_search_results`] (03 §4.7).
    Search(SearchQuery),
    /// The next page of the history search's results.
    MoreResults,
    /// The filters changed (03 §4.6): load again with them ([`GitView::filters`]).
    FiltersChanged(Filters),
    /// Enter or F3 on a file row of the details pane: its diff in the text viewer (03 §6.2).
    Diff {
        subject: Subject,
        group: Group,
        file: FileRow,
    },
    /// Shift+F3 on a file row: the file at its new endpoint (03 §6.3).
    RevisionFile {
        subject: Subject,
        group: Group,
        file: FileRow,
    },
    /// → or Enter on an untracked folder's row (`dir/`): list its files (`ls-files -z --others
    /// --exclude-standard -- dir/`) and hand the details again with them after the folder's row.
    ListFolder { subject: Subject, dir: String },
    /// F4: the top level in the editor (`None`), or the file under the pane's bar, its path now
    /// (03 §6.3).
    Edit(Option<String>),
    /// Tab, Shift+Tab: the keyboard back to the origin list (02 §3.2).
    OtherSide,
    /// F9: the browser's mode (02 §2).
    SwitchMode,
    /// Cmd/Ctrl+R: the hard refresh (03 §5).
    Refresh,
    /// Cmd/Ctrl+G: pin this git panel; again, unpin (02 §3.2).
    Pin,
    /// Esc with nothing open in the panel: unpin it when it is pinned (02 §3.2).
    Unpin,
    /// Cmd/Ctrl+Shift+G: the git panel over both sides, and back.
    Wide,
    // S3 (03 §7–§8; S3-C).
    /// An action's dialog, once what it starts from is read: the integration reads the
    /// [`Prefill`] `id` needs ([`prefilled`]) and gives it to [`GitView::open_action`].
    Open { id: ActionId, target: Target },
    /// Run the action on a worker under the repository's write lock (02 §6), then refresh (03
    /// §5): the plan its confirmation showed is [`GitView::take_plan`]'s, else plan it there.
    Run(Action),
    /// Cmd/Ctrl+Z (the window's newest group of this repository), or the Backups list's Restore:
    /// group `stamp`'s entries read now, then [`GitView::open_restore`] (03 §8.3); `only` names
    /// one backup ref.
    Restore { stamp: String, only: Option<String> },
    /// Cmd/Ctrl+B: the Backups list read, then [`GitView::open_backups`] (03 §8.2).
    Backups,
    /// Cmd/Ctrl+L: the command log, then [`GitView::open_log`] (03 §3.7).
    Log,
    /// The Backups list's Show: these refs into the graph's revisions until the next refresh.
    ShowBackup(Vec<BackupRef>),
    /// `tag-details` (03 §7.4): `cat-file -p <t>` read, then [`GitView::show_text`].
    TagDetails(String),
    /// Yes to "Stop git?" (03 §3.4): the write it asked about — the run of this id — is stopped.
    StopWrite(u64),
    // S4 (03 §7.8; S4-C).
    /// A quick edit's dialog (03 §7.8.1), once its commit is read: the range above `base` up to
    /// HEAD (`rebase::open_needs`, `actions::inspect`, `rebase::open`), whose rows hold `commit`
    /// and, for `squash`, the commit below it; then [`GitView::open_edit`].
    OpenEdit { id: EditId, commit: Oid, base: Base },
    /// The planner over the range above `base` up to HEAD (03 §7.8.2), once read; then
    /// [`GitView::open_planner`].
    OpenPlanner(Base),
    /// The Rebase dialog's "plan the commits first" (03 §7.1): the planner over the commits
    /// `rebase <onto>` moves — above `merge-base HEAD <onto>`, or from the root when they share
    /// none.
    PlanBeforeRebase(actions::Rev),
    /// Run a history edit on a worker under the repository's write lock (02 §6), then refresh
    /// (03 §5): the plan its confirmation showed is [`GitView::take_edit_plan`]'s, else plan it
    /// there (a plan that asks for a confirmation then opens it again instead).
    RunEdit(Edit),
}

/// A label of a row, as drawn (03 §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub text: String,
    pub kind: LabelKind,
    /// The ref's name is not valid UTF-8 (shown with U+FFFD; the read model's `RefLabel::lossy`).
    pub lossy: bool,
    /// Where in `text` the `↔` stands that joins a local branch to the remotes having it (a byte
    /// offset): the panel's own glyph, which an ambiguous-wide terminal gets as §10's stand-in,
    /// never a `↔` of a name (T38).
    pub joined: Option<usize>,
}

/// The kinds of label, in the order a row draws them (03 §4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LabelKind {
    /// A detached HEAD: `HEAD`.
    Head,
    /// The checked-out branch: `►main`.
    CheckedOut,
    /// Another local branch (with the remotes that have it on the same commit: `main↔origin`).
    Local,
    /// A remote branch: `origin/feature`.
    Remote,
    Tag,
    /// `stash@{n}`.
    Stash,
}

/// One row as text, for `SOPEN_LIST=git` (02 §9): its labels in order, the subject and the
/// columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowText {
    pub labels: Vec<String>,
    /// The subject, or `Uncommitted changes (N)`.
    pub subject: String,
    /// `2026-11-20 10:24`, local time; blank for the uncommitted row.
    pub date: String,
    /// The author's name; `*` for the uncommitted row.
    pub author: String,
    /// The first 8 hex digits; `*` for the uncommitted row.
    pub commit: String,
}

/// A list's bar and scroll: the graph's, a history search's, the details pane's.
#[derive(Debug, Clone, Copy, Default)]
struct Scroll {
    bar: usize,
    top: usize,
    /// Rows the last pass had room for.
    visible: usize,
    /// The bar moved since the last pass: that pass scrolls it into view.
    moved: bool,
}

impl Scroll {
    /// Moves the bar by `delta` rows within `len`, stopping at the first and the last.
    fn step(&mut self, delta: isize, len: usize) {
        let last = len.saturating_sub(1);
        self.bar = self.bar.saturating_add_signed(delta).min(last);
        self.moved = true;
    }

    /// The bar to `at`.
    fn to(&mut self, at: usize) {
        self.bar = at;
        self.moved = true;
    }

    /// A step key's rows: a page is one row less than the room.
    fn delta(&self, step: Step) -> isize {
        let page = self.visible.saturating_sub(1).max(1) as isize;
        match step {
            Step::Up => -1,
            Step::Down => 1,
            Step::PageUp => -page,
            Step::PageDown => page,
            Step::Home => isize::MIN,
            Step::End => isize::MAX,
        }
    }

    /// Before a pass with room for `visible` rows of `len`: a bar that moved is scrolled into
    /// view; the rows never end above the bottom while there are more.
    fn settle(&mut self, visible: usize, len: usize) {
        self.visible = visible;
        self.bar = self.bar.min(len.saturating_sub(1));
        if std::mem::take(&mut self.moved) {
            if self.bar < self.top {
                self.top = self.bar;
            } else if self.bar >= self.top + visible {
                self.top = self.bar + 1 - visible;
            }
        }
        self.top = self.top.min(len.saturating_sub(visible));
    }

    /// The rows on screen.
    fn range(&self, len: usize) -> std::ops::Range<usize> {
        let end = (self.top + self.visible).min(len);
        self.top.min(end)..end
    }

    /// The wheel's lines over the list scroll its rows (the bar stays where it is).
    fn wheel(&mut self, lines: f32, len: usize) {
        let max_top = len.saturating_sub(self.visible);
        self.top = (self.top as f32 + lines).round().clamp(0.0, max_top as f32) as usize;
    }
}

/// The git panel of one repository: its inputs and the view's own state.
#[derive(Debug)]
pub struct GitView {
    /// The frame's title: the top level (`host:/abs`) with its notes (03 §2.3); the view adds
    /// ` [filtered]` while a filter differs from its default (03 §4.6).
    pub title: String,
    /// What the integration says on the status row (the last command, a read's error), over the
    /// view's own words.
    pub status: Option<Status>,
    /// The foot's right end.
    pub tracking: Option<Tracking>,
    paging: Paging,
    rows: Vec<Row>,
    graph: Graph,
    /// `N` of `Uncommitted changes (N)`.
    uncommitted: usize,
    refs: Vec<RefLabel>,
    /// The repository's remotes (`git remote`): a remote branch is its longest prefix among them.
    remotes: Vec<String>,
    head: HeadRef,
    /// The labels each commit draws, by the commit (built when the refs or the filters change,
    /// never per pass).
    labels: HashMap<Oid, Vec<Label>>,
    /// The loaded rows that are commits (the foot's `N commits`).
    commits: usize,
    /// The checked-out commit's row, once loaded (Cmd/Ctrl+J).
    head_row: Option<usize>,
    graph_scroll: Scroll,
    /// The marked rows (Space / Ins).
    marks: HashSet<RowKey>,
    /// The label cursor on the bar's row: 0 the commit itself, `n` its `n`th label (03 §9.2).
    label_cursor: usize,
    /// [`GitAction::MoreRows`] was asked for since the rows or the paging last changed.
    more_asked: bool,
    filters: Filters,
    filters_dialog: Option<filters::Dialog>,
    find: Option<find::Find>,
    /// The panel's own key, whatever its column or `ui` (`find::panel_key`, X4-find): Find's ids,
    /// the graph's rows' and its paging line's, the banner's (handed to its `draw`), and those of
    /// the details pane, the log and the Backups list, each given it when made (`in_panel`,
    /// F-git-ids).
    find_key: egui::Id,
    search_dialog: Option<find::SearchDialog>,
    search: Option<find::Results>,
    pane: Option<details::Pane>,
    /// The files whose diff was opened in this window: `√` (03 §6.1).
    viewed: HashSet<(Subject, String)>,
    /// The view's own word for the status row (compare takes one marked row; a name not UTF-8).
    said: Option<Status>,
    /// Where the last pass drew the graph's rows.
    geometry: Option<graph_view::Geometry>,
    /// The row the bar goes to once it is loaded, and its place on screen: the bar's row across a
    /// load that reads its pages again (03 §4.5), or a row asked for ([`GitView::select`]).
    pending: Option<(RowKey, usize)>,
    /// `pending` was asked for ([`GitView::select`]), not kept across a reload: it waits for the
    /// history's end and says so there; a kept bar's row that the history no longer holds (a
    /// dropped stash, a deleted branch's commit) is given up once the stream ends (S3-C).
    pending_asked: bool,
    // S3 (S3-C): the actions.
    /// The repository as the last refresh read it ([`GitView::set_repo`]), with the reads a
    /// dialog asked for since ([`GitView::seen`]); `None` until then: no action is offered.
    repo: Option<Repo>,
    /// Counts [`GitView::set_repo`]: reads made for an older snapshot are dropped.
    repo_generation: u64,
    /// The in-progress banner of the last refresh (03 §8.5).
    banner: Option<in_progress::BannerView>,
    /// The newest backup group this window wrote for this repository: Cmd/Ctrl+Z's (03 §8.3).
    undo: Option<String>,
    /// Window state the dialogs start from: "Don't ask again in this window" (03 §7.1).
    unasked: bool,
    /// The menu of the object under the bar, with a commit's history row (S4).
    menu: Option<menu::HistoryMenu>,
    /// Another panel's dialog is up (two repositories' panels side by side): this one opens none
    /// and takes no click until it closes, one dialog at a time (X4-menu round 1).
    dialog_elsewhere: bool,
    dialog: Option<Box<dialogs::ActionDialog>>,
    /// The plan the dialog showed for the action it returned: what runs (S3-A: its backup
    /// verifies the values the owner confirmed).
    confirmed: Option<(Action, Plan)>,
    /// The result up: a run's error dialog, a read's failure, a tag's details (03 §3.8, §7.4).
    /// It stays until the owner closes it; one that comes meanwhile waits (F-git-outcome).
    result: Option<dialogs::ResultDialog>,
    /// The results that came while one was up, oldest first: each comes up once the one before
    /// it closes. At most [`RESULTS_WAITING`].
    waiting: VecDeque<dialogs::ResultDialog>,
    /// Which result is up and what the screen showed of it.
    result_screen: ResultScreen,
    /// The waiting results dropped since a result last came up ([`RESULTS_WAITING`]): the next
    /// one to come up says how many.
    results_dropped: usize,
    /// "Stop git?" over a write running for 10 s (03 §3.4): the action's words, and the id of the
    /// run it asks about.
    stop_question: Option<(String, u64)>,
    /// Each dialog slot's serial, by [`Front`]: bumped whenever the slot is set or cleared other
    /// than by the owner's own key in that dialog (those come in the pass's draw), so
    /// [`GitView::fronts`] tells a dialog replaced by another of its kind — a result by the next —
    /// from the one that was up (F-git-listq).
    front_serials: [u64; Front::KINDS],
    /// The Backups list in the body (Cmd/Ctrl+B, 03 §8.2).
    backups: Option<backups::BackupsView>,
    /// The command log in the body (Cmd/Ctrl+L, 03 §3.7).
    log: Option<log_view::LogView>,
    /// Text for the clipboard (the menu's Copy items), handed to egui on the next pass.
    to_copy: Option<String>,
    // S4 (S4-C): the history edits.
    /// A quick edit's dialog, or the confirmation of an edit built elsewhere — an edit stop's
    /// Split, a kept split's Continue, a run whose shown plan was lost (`planner::QuickDialog`).
    quick: Option<Box<planner::QuickDialog>>,
    /// The planner over the body (03 §7.8.2): it takes its own keys first.
    planner: Option<planner::PlannerView>,
    /// The plan the quick dialog or the planner's run showed for the edit it returned: what runs
    /// (S3-C's rule: never a plan no confirmation showed).
    confirmed_edit: Option<(Edit, Plan)>,
    /// The edit stop the banner shows (03 §8.5): its Amend and Split.
    stop: Option<in_progress::EditStop>,
    /// The split this window keeps for this repository (03 §7.8.1 `split`): while kept, the
    /// banner's Continue is `Edit::ContinueSplit`'s.
    split: Option<rebase::Split>,
    /// Cells a test says an open dialog covers ([`GitView::covered`]): every dialog answers none
    /// while egui draws it, so the blanking is proved through these until their units land.
    #[cfg(test)]
    pub test_covers: Vec<Cells>,
}

/// A dialog slot of a panel's (F-git-listq): the planner's own dialogs and the log's entry shown
/// whole in the body, the filters, the history search, the menu, a quick edit's, an action's, the
/// result, "Stop git?" ([`GitView::fronts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Front {
    Planner,
    Entry,
    Filters,
    Search,
    Menu,
    Quick,
    Dialog,
    Result,
    Stop,
}

impl Front {
    /// How many slots there are.
    pub const KINDS: usize = 9;
}

/// The open dialog's draft as the integration checks its reads by (F-git-drafted): which dialog
/// it is — its own serial, which no other dialog of any kind shares ([`dialog_serial`]), so two
/// dialogs whose drafts stand at the same revision are told apart — and its draft's revision,
/// which moves whenever a pass changes the draft ([`GitView::draft_key`]). Never a copy of the
/// draft: a planner run's is its whole model, messages included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftKey {
    pub serial: u64,
    pub revision: u64,
}

/// A serial for a dialog that opens ([`DraftKey::serial`]): no two in the process share one.
fn dialog_serial() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The bytes of messages the quick edits' dialogs' drafts were copied with on this thread since
/// the last call ([`planner::Draft`]'s copies, the integration's included): the browser's tests'
/// bound on a pass (F-git-drafted).
#[cfg(test)]
pub(super) fn quick_copied() -> usize {
    planner::copied()
}

/// The results that wait behind the one up (F-git-outcome). While a result is up it has the
/// keyboard, so what can still come is the end of a write queued before it (one runs at a time)
/// or of a read asked for before it — a handful; each can hold the last 64 KiB of a command's
/// stderr (`runner::STDERR_KEPT`), so eight hold half a mebibyte of it at most. Past them the
/// oldest waiting goes, so the newest, the one "delete anyway?" may answer (03 §7.2), is always
/// kept; the next result to come up — the one that came right after those dropped — says how
/// many were not shown and that the command log (Cmd/Ctrl+L, 03 §3.7) has the commands that ran
/// (a run that never ran one, `RunEnd::NotRun`, left none there), and the status row keeps the
/// latest run's words.
const RESULTS_WAITING: usize = 8;

/// What the screen showed of the result up (F-git-outcome), by the rule an action's dialog and
/// "Stop git?" follow (`dialogs.rs`): each result that comes up takes a serial of its own,
/// is laid out unseen first — and again after a pass it was not drawn in (behind the menu or an
/// action's dialog) — and Enter, Esc, OK or a click outside close it only once the screen showed
/// it ([`cells::OnScreen`]). So a press meant for the result before it, in the same pass or in a
/// burst the terminal did not print, closes nothing: it would go unread.
#[derive(Debug, Default)]
struct ResultScreen {
    /// The result up's serial: the painting the screen is compared by.
    serial: u64,
    /// The pass it was last laid out in.
    laid: Option<u64>,
    screen: cells::OnScreen<u64>,
}

impl ResultScreen {
    /// Another result is up: nothing of it shown yet.
    fn next(&mut self) {
        self.serial += 1;
        self.laid = None;
    }

    /// At the start of the result's pass: whether the screen shows it. The pass paints it where
    /// it can be seen once it was laid out in the pass before — any other pass before, later or
    /// earlier (a switch between the window and the terminal, which count their passes apart),
    /// lays it out unseen again. A pass that lays it out unseen asks for the next, which paints
    /// it, so the screen can show it before the owner's next press.
    fn shown(&mut self, ctx: &egui::Context) -> bool {
        let pass = ctx.cumulative_pass_nr();
        if self.laid.is_some_and(|nr| nr + 1 != pass) {
            self.laid = None;
        }
        let seen = self.screen.start(ctx) == Some(&self.serial);
        if self.laid.replace(pass).is_some() {
            self.screen.painted(self.serial);
        } else {
            ctx.request_repaint();
        }
        seen
    }
}

impl RowKey {
    /// Whether `row` is this row (without making its key).
    fn is(&self, row: &Row) -> bool {
        match (self, row) {
            (RowKey::Uncommitted, Row::Uncommitted { .. }) => true,
            (RowKey::Stash(oid), Row::Stash(s)) => *oid == s.oid,
            (RowKey::Commit(oid), Row::Commit { commit, .. }) => *oid == commit.oid,
            _ => false,
        }
    }
}

/// `row` as a comparison's end says it: its short hash and subject.
fn end_text(row: &Row, uncommitted: usize) -> String {
    let (subject, _, _, commit) = graph_view::cells(row, uncommitted);
    match row {
        Row::Uncommitted { .. } => subject,
        _ => format!("{commit} {subject}"),
    }
}

/// The status row's words for a name that is not UTF-8 (as the file list says it).
fn not_utf8(shown: &str) -> String {
    format!("{shown}: the name is not valid UTF-8; sopen cannot open it")
}

impl GitView {
    /// A panel titled `title`, nothing loaded yet.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            status: None,
            tracking: None,
            paging: Paging::Loading,
            rows: Vec::new(),
            graph: Graph::new(false),
            uncommitted: 0,
            refs: Vec::new(),
            remotes: Vec::new(),
            head: HeadRef::Unborn,
            labels: HashMap::new(),
            commits: 0,
            head_row: None,
            graph_scroll: Scroll::default(),
            marks: HashSet::new(),
            label_cursor: 0,
            more_asked: false,
            filters: Filters::default(),
            filters_dialog: None,
            find: None,
            find_key: find::panel_key(),
            search_dialog: None,
            search: None,
            pane: None,
            viewed: HashSet::new(),
            said: None,
            geometry: None,
            pending: None,
            pending_asked: false,
            repo: None,
            repo_generation: 0,
            banner: None,
            undo: None,
            unasked: false,
            menu: None,
            dialog_elsewhere: false,
            dialog: None,
            confirmed: None,
            result: None,
            waiting: VecDeque::new(),
            result_screen: ResultScreen::default(),
            results_dropped: 0,
            stop_question: None,
            front_serials: [0; Front::KINDS],
            backups: None,
            log: None,
            to_copy: None,
            quick: None,
            planner: None,
            confirmed_edit: None,
            stop: None,
            split: None,
            #[cfg(test)]
            test_covers: Vec::new(),
        }
    }

    /// The refs read and HEAD (03 §4.1): the labels are built from them now, by the filters.
    pub fn set_refs(&mut self, refs: Vec<RefLabel>, head: HeadRef) {
        self.refs = refs;
        self.head = head;
        self.relabel();
    }

    /// The repository's remotes (`git remote`, 03 §4.6): a remote branch belongs to the longest of
    /// them its name starts with (`team/up/main` is `team/up`'s when both `team` and `team/up` are
    /// remotes). Without them, a remote branch's remote is its first component.
    pub fn set_remotes(&mut self, remotes: Vec<String>) {
        self.remotes = remotes;
        self.relabel();
    }

    /// The labels again, after the refs, HEAD or the filters changed.
    fn relabel(&mut self) {
        self.labels = graph_view::index(&self.refs, &self.remotes, &self.head, &self.filters);
        if let Some(find) = &mut self.find {
            find.reset();
        }
    }

    /// A load's first rows, replacing what was loaded (a new load, or the uncommitted row coming
    /// or going, 03 §5), laid out with `first_parent` (the filter); `uncommitted` is the number
    /// of changed paths (`Uncommitted changes (N)`). The bar stays on its commit, where it was on
    /// screen, when it is still there (03 §4.5).
    pub fn set_rows(&mut self, rows: Vec<Row>, first_parent: bool, uncommitted: usize) {
        let asked = std::mem::take(&mut self.pending_asked);
        let (kept, offset) = match self.pending.take() {
            Some((key, offset)) => (Some(key), offset),
            None => (
                self.bar_row().map(RowKey::of),
                self.graph_scroll.bar.saturating_sub(self.graph_scroll.top),
            ),
        };
        self.graph = crate::git::graph::layout(&rows, first_parent);
        self.rows = rows;
        self.uncommitted = uncommitted;
        self.commits = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Commit { .. }))
            .count();
        self.head_row = self.rows.iter().position(|r| {
            matches!(
                r,
                Row::Commit {
                    checked_out: true,
                    ..
                }
            )
        });
        let found = kept
            .as_ref()
            .and_then(|k| self.rows.iter().position(|r| k.is(r)));
        match found {
            Some(at) => {
                self.graph_scroll.bar = at;
                self.graph_scroll.top = at.saturating_sub(offset);
            }
            None => {
                self.graph_scroll.bar =
                    self.graph_scroll.bar.min(self.rows.len().saturating_sub(1));
                self.label_cursor = 0;
                // It may come with a later page of the same load.
                self.pending = kept.map(|key| (key, offset));
                self.pending_asked = asked && self.pending.is_some();
            }
        }
        self.more_asked = false;
        if let Some(find) = &mut self.find {
            find.reset();
        }
    }

    /// The next page's rows, below the loaded ones: rows above never move (03 §4.3), the bar
    /// stays where it is, on screen.
    pub fn append_rows(&mut self, rows: Vec<Row>) {
        let base = self.rows.len();
        self.graph.extend(&rows);
        for (k, row) in rows.iter().enumerate() {
            if let Row::Commit { checked_out, .. } = row {
                self.commits += 1;
                if *checked_out && self.head_row.is_none() {
                    self.head_row = Some(base + k);
                }
            }
        }
        self.rows.extend(rows);
        self.more_asked = false;
        // The bar's row of a load read again, or a row asked for, in this page.
        let found = self.pending.as_ref().and_then(|(key, offset)| {
            let at = self.rows[base..].iter().position(|r| key.is(r))?;
            Some((base + at, *offset))
        });
        if let Some((at, offset)) = found {
            self.pending = None;
            self.pending_asked = false;
            self.graph_scroll.bar = at;
            self.graph_scroll.top = at.saturating_sub(offset);
        }
    }

    /// The bar to `key`'s row (`SOPEN_STATE` `select=REV`, 02 §9): at once when it is loaded
    /// (`true`), else once a page brings it ([`GitView::selecting`]).
    pub fn select(&mut self, key: RowKey) -> bool {
        match self.rows.iter().position(|r| key.is(r)) {
            Some(at) => {
                self.pending = None;
                self.pending_asked = false;
                self.label_cursor = 0;
                self.graph_scroll.to(at);
                true
            }
            None => {
                let offset = self.graph_scroll.visible / 2;
                self.pending = Some((key, offset));
                self.pending_asked = true;
                false
            }
        }
    }

    /// The row the bar waits for, not loaded yet.
    pub fn selecting(&self) -> Option<&RowKey> {
        self.pending.as_ref().map(|(key, _)| key)
    }

    /// The label cursor on the bar's row's `n`th label (0: the commit itself), as far as it has
    /// labels (`SOPEN_STATE` `label=N`, 02 §9).
    pub fn set_label_cursor(&mut self, n: usize) {
        let labels = self.bar_row().map_or(0, |r| self.labels_of(r).len());
        self.label_cursor = n.min(labels);
    }

    /// Enter or F3 on the graph's bar, from outside (`SOPEN_STATE` `details=1`, 02 §9): its details
    /// pane, or a comparison with one marked row, as the key does.
    pub fn open_details(&mut self) -> Vec<GitAction> {
        self.details_of_bar()
    }

    /// Where the stream stands (the paging line). Once it has ended, a bar's row kept across
    /// the reload that never came is given up (the bar stays where it fell); a row asked for
    /// stays asked, for its asker to say it is not in the graph.
    pub fn set_paging(&mut self, paging: Paging) {
        let ended = !matches!(paging, Paging::Loading | Paging::More(_));
        if ended && !self.pending_asked {
            self.pending = None;
        }
        self.paging = paging;
        self.more_asked = false;
    }

    pub fn paging(&self) -> &Paging {
        &self.paging
    }

    /// The details read for `details.subject`: shown when the pane still shows that subject,
    /// else dropped (the bar moved on meanwhile).
    pub fn set_details(&mut self, details: Details) {
        if let Some(pane) = &mut self.pane {
            pane.set(details);
        }
    }

    /// A history search's first rows (03 §4.7), shown as a flat list in place of the graph;
    /// dropped unless `query` is the search shown (left or replaced meanwhile).
    pub fn set_search_results(
        &mut self,
        query: &SearchQuery,
        rows: Vec<CommitRow>,
        paging: Paging,
    ) {
        if let Some(results) = self.search.as_mut().filter(|s| s.query == *query) {
            results.rows.clear();
            results.scroll = Scroll::default();
            results
                .rows
                .extend(rows.into_iter().map(|commit| Row::Commit {
                    commit,
                    checked_out: false,
                }));
            results.paging = paging;
            results.more_asked = false;
        }
    }

    /// The search's next page (dropped as [`GitView::set_search_results`]'s).
    pub fn append_search_results(
        &mut self,
        query: &SearchQuery,
        rows: Vec<CommitRow>,
        paging: Paging,
    ) {
        if let Some(results) = self.search.as_mut().filter(|s| s.query == *query) {
            results
                .rows
                .extend(rows.into_iter().map(|commit| Row::Commit {
                    commit,
                    checked_out: false,
                }));
            results.paging = paging;
            results.more_asked = false;
        }
    }

    /// The rows of the history search shown (S2-F: a search ended while hidden reads as many
    /// again).
    pub fn search_rows(&self) -> usize {
        self.search.as_ref().map_or(0, |s| s.rows.len())
    }

    /// The commits of the history search's results, in order (tests).
    #[cfg(test)]
    pub fn search_commits(&self) -> Vec<Oid> {
        self.search.as_ref().map_or_else(Vec::new, |s| {
            s.rows
                .iter()
                .filter_map(|r| match r {
                    Row::Commit { commit, .. } => Some(commit.oid.clone()),
                    _ => None,
                })
                .collect()
        })
    }

    /// The history search shown, if any.
    pub fn search(&self) -> Option<&SearchQuery> {
        self.search.as_ref().map(|s| &s.query)
    }

    /// The filters chosen (03 §4.6).
    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    /// Opens the filters' dialog (the menu's `filters`, S3; 03 §7.7).
    pub fn open_filters(&mut self) {
        // The dialog has the typing: Find, which would keep it, closes.
        self.find = None;
        self.bump(Front::Filters);
        self.filters_dialog = Some(filters::Dialog::new(
            &self.filters,
            &self.refs,
            &self.remotes,
        ));
    }

    /// The label menu's "Show only this branch" (S3; 03 §7.2): the Branches filter on it alone.
    pub fn only_branch(&mut self, refname: &str) -> GitAction {
        self.filters.only(refname);
        self.relabel();
        GitAction::FiltersChanged(self.filters.clone())
    }

    /// The label menu's "Show all branches".
    pub fn all_branches(&mut self) -> GitAction {
        self.filters.all_branches();
        self.relabel();
        GitAction::FiltersChanged(self.filters.clone())
    }

    /// The loaded rows.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The lane layout of the loaded rows.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// The row under the graph's bar.
    pub fn bar_row(&self) -> Option<&Row> {
        self.rows.get(self.graph_scroll.bar)
    }

    /// The label under the label cursor, on the bar's row (S3's label actions).
    pub fn bar_label(&self) -> Option<Label> {
        let k = self.label_cursor.checked_sub(1)?;
        graph_view::row_labels(self.bar_row()?, &self.labels)
            .get(k)
            .cloned()
    }

    /// The graph's bar's row and its place on screen (rows below the first one shown).
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn bar_place(&self) -> (usize, usize) {
        let s = &self.graph_scroll;
        (s.bar, s.bar.saturating_sub(s.top))
    }

    /// The rows the last pass drew.
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn on_screen(&self) -> std::ops::Range<usize> {
        self.graph_scroll.range(self.rows.len())
    }

    /// Whether `row` is marked.
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn marked(&self, row: &RowKey) -> bool {
        self.marks.contains(row)
    }

    /// The status row while the git panel has the keyboard: the view's own word, else the
    /// integration's, else what the keys do.
    pub fn status_line(&self) -> Status {
        if let Some(menu) = &self.menu {
            return Status::info(menu_hint(menu));
        }
        if self.filters_dialog.is_some() {
            return Status::info(filters::HINT);
        }
        if let Some(said) = self.said.as_ref().or(self.status.as_ref()) {
            return said.clone();
        }
        // The planner's keys are at its foot; here, what else works while it is open.
        if self.planner.is_some() {
            return Status::info(PLANNER_HINT);
        }
        if self.find.is_some() {
            return Status::info(find::FIND_HINT);
        }
        // The banner's keys (03 §8.5: "then its keys"), while it shows: an edit stop's Amend and
        // Split first, as its buttons read (S4).
        if let Some(banner) = &self.banner {
            let stop = self.stop.as_ref().map_or_else(Vec::new, |s| s.offers());
            let steps: Vec<String> =
                stop.into_iter()
                    .map(|s| {
                        let key = in_progress::stop_key_text(s);
                        format!("{key} {}", in_progress::stop_word(s))
                    })
                    .chain(
                        banner.offers().iter().map(|&p| {
                            format!("{} {}", in_progress::key_text(p), in_progress::word(p))
                        }),
                    )
                    .collect();
            return Status::info(format!("{} · {HINT}", steps.join(" · ")));
        }
        Status::info(HINT)
    }

    /// Row `i` as text (`SOPEN_LIST=git`).
    pub fn line(&self, i: usize) -> Option<RowText> {
        let row = self.rows.get(i)?;
        let (subject, date, author, commit) = graph_view::cells(row, self.uncommitted);
        Some(RowText {
            labels: graph_view::row_labels(row, &self.labels)
                .iter()
                .map(|l| graph_view::printable(&l.text).into_owned())
                .collect(),
            subject,
            date,
            author,
            commit,
        })
    }

    /// The paging line as the foot says it (`N commits · M more`, the shallow note, an
    /// error): `SOPEN_LIST=git`'s last line (02 §9).
    pub fn paging_line(&self) -> String {
        graph_view::paging_text(&self.paging, self.commits, "commits").0
    }

    /// Find's text while its field is open.
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn find_text(&self) -> Option<&str> {
        self.find.as_ref().map(|f| f.text.as_str())
    }

    /// Find's case toggle: case-sensitive or not (03 §4.7).
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn set_find_case(&mut self, case: bool) {
        if let Some(find) = &mut self.find {
            find.case = case;
        }
    }

    /// The file row under the details pane's bar (S2-F's tests and receipts).
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn pane_file(&self) -> Option<&FileRow> {
        let pane = self.pane.as_ref()?;
        match pane.line()? {
            details::Line::File { group, file } => pane.file(*group, *file).map(|(_, f)| f),
            _ => None,
        }
    }

    /// Whether the details pane is open.
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn details_open(&self) -> bool {
        self.pane.is_some()
    }

    /// Where the last pass drew the graph's rows.
    fn geometry(&self) -> Option<graph_view::Geometry> {
        self.geometry
    }

    /// The rows the keyboard's bar is on: a history search's results while one is shown.
    fn shown(&self) -> (&[Row], usize) {
        match &self.search {
            Some(s) => (&s.rows, s.scroll.bar),
            None => (&self.rows, self.graph_scroll.bar),
        }
    }

    /// The graph's bar to row `to`; the pane, following the bar, asks for its details.
    fn move_bar(&mut self, to: usize) -> Option<GitAction> {
        let last = self.rows.len().checked_sub(1)?;
        // The owner moved it: no row waited for takes it back.
        self.pending = None;
        self.pending_asked = false;
        let to = to.min(last);
        if to != self.graph_scroll.bar {
            self.label_cursor = 0;
        }
        self.graph_scroll.to(to);
        self.follow()
    }

    /// The pane, when it follows the bar (its keyboard given back), to the bar's row.
    fn follow(&mut self) -> Option<GitAction> {
        let (rows, bar) = self.shown();
        let key = RowKey::of(rows.get(bar)?);
        let pane = self.pane.as_mut().filter(|p| !p.keyboard)?;
        let subject = Subject::Row(key);
        if pane.subject == subject {
            return None;
        }
        *pane = details::Pane::new(subject.clone(), false, Vec::new()).in_panel(self.find_key);
        Some(GitAction::Details(subject))
    }

    /// The labels `row` draws.
    fn labels_of<'a>(&'a self, row: &'a Row) -> std::borrow::Cow<'a, [Label]> {
        graph_view::row_labels(row, &self.labels)
    }

    /// Find's matches brought up to date; whether its text changed.
    fn refresh_find(&mut self) -> bool {
        let Some(mut find) = self.find.take() else {
            return false;
        };
        let changed = find.refresh(&self.rows, &self.labels);
        self.find = Some(find);
        changed
    }

    /// Enter (`forward`) or Shift+Enter in Find: the next or the previous match — after the jump
    /// to the first match a text typed before it in the pass owes (X4-find).
    fn find_step(&mut self, forward: bool) -> Vec<GitAction> {
        let mut asked = self.find_jump();
        let at = self
            .find
            .as_ref()
            .and_then(|f| f.step(self.graph_scroll.bar, forward));
        asked.extend(at.and_then(|at| self.move_bar(at)));
        asked
    }

    /// Find's matches brought up to date, and the bar on the first match from where Find opened
    /// when its text or case changed.
    fn find_jump(&mut self) -> Vec<GitAction> {
        if !self.refresh_find() {
            return Vec::new();
        }
        let first = self.find.as_ref().and_then(find::Find::first);
        first.and_then(|to| self.move_bar(to)).into_iter().collect()
    }

    /// Esc in Find: closed, with the bar where what was typed before it put it (X4-find).
    fn find_close(&mut self) -> Vec<GitAction> {
        let asked = self.find_jump();
        self.find = None;
        asked
    }

    /// Enter or F3 on the graph's bar (03 §6.1, §6.4): one other marked row compares it with the
    /// bar's, more than one says so and opens the bar's own; the pane opens with the keyboard.
    fn details_of_bar(&mut self) -> Vec<GitAction> {
        let bar = self.graph_scroll.bar;
        let Some(row) = self.rows.get(bar) else {
            return Vec::new();
        };
        let key = RowKey::of(row);
        let mut header = Vec::new();
        let subject = match self.marks.len() {
            1 if !self.marks.contains(&key) => {
                let mark = self
                    .marks
                    .iter()
                    .next()
                    .cloned()
                    .unwrap_or(RowKey::Uncommitted);
                let at = self.rows.iter().position(|r| mark.is(r));
                // The lower row is the older end; a mark no longer loaded counts as older.
                let mark_lower = at.is_none_or(|at| at > bar);
                let (from, to) = if mark_lower { (mark, key) } else { (key, mark) };
                let text = |k: &RowKey| {
                    self.rows
                        .iter()
                        .find(|r| k.is(r))
                        .map_or_else(String::new, |r| end_text(r, self.uncommitted))
                };
                header = vec![("From", text(&from)), ("To", text(&to))];
                Subject::Compare { from, to }
            }
            0 | 1 => Subject::Row(key),
            _ => {
                self.said = Some(Status::info("compare takes exactly one marked row"));
                Subject::Row(key)
            }
        };
        // The pane already shows it (following the bar): the keyboard goes there, nothing is
        // read again.
        if let Some(pane) = self.pane.as_mut().filter(|p| p.subject == subject) {
            pane.keyboard = true;
            return Vec::new();
        }
        self.pane = Some(details::Pane::new(subject.clone(), true, header).in_panel(self.find_key));
        vec![GitAction::Details(subject)]
    }

    /// Carries out `command` (the key table's git context) on the view in a pass of `ctx`, and
    /// gives what is left for the integration.
    fn run(&mut self, ctx: &egui::Context, command: Command) -> Vec<GitAction> {
        use GitAction as A;
        match command {
            Command::Help => return vec![A::Help],
            Command::SwitchMode => return vec![A::SwitchMode],
            Command::OtherSide => return vec![A::OtherSide],
            Command::Reread => return vec![A::Refresh],
            Command::Pin => return vec![A::Pin],
            Command::Wide => return vec![A::Wide],
            // An edit stop's Amend and Split act on the banner, as its Continue, Skip and Abort
            // do, whatever shows in the body (the log, the Backups list).
            Command::Amend => return self.stop_step(in_progress::StopStep::Amend),
            Command::Split => return self.stop_step(in_progress::StopStep::Split),
            _ => {}
        }
        // The log or the Backups list in the body takes the keyboard (review 1): its own keys,
        // Cmd/Ctrl+L and B to switch or close it; every other key does nothing — the graph under
        // it, and so its bar's object, is not on screen.
        if self.log.is_some() || self.backups.is_some() {
            return self.run_list(ctx, command);
        }
        match command {
            Command::HistorySearch => {
                self.bump(Front::Search);
                self.search_dialog = Some(find::SearchDialog::default());
                return Vec::new();
            }
            // Cmd/Ctrl+K (03 §9.2): the filters' dialog, in S2 before S3's menu offers it.
            Command::Filters => {
                self.open_filters();
                return Vec::new();
            }
            _ => {}
        }
        if let Some(asked) = self.run_actions(command) {
            return asked;
        }
        if self.pane.as_ref().is_some_and(|p| p.keyboard) {
            return self.run_pane(command);
        }
        if self.search.is_some() {
            return self.run_results(command);
        }
        self.run_graph(command)
    }

    /// A command with the keyboard in the graph.
    fn run_graph(&mut self, command: Command) -> Vec<GitAction> {
        use GitAction as A;
        let bar = self.graph_scroll.bar;
        match command {
            Command::Bar(step) => {
                let Some(last) = self.rows.len().checked_sub(1) else {
                    return Vec::new();
                };
                // ↓ on the last row: the next page now (a retry after an error too).
                let more = matches!(self.paging, Paging::More(_) | Paging::Error(_));
                if step == Step::Down && bar == last && more && !self.more_asked {
                    self.more_asked = true;
                    return vec![A::MoreRows];
                }
                let to = bar.saturating_add_signed(self.graph_scroll.delta(step));
                self.move_bar(to).into_iter().collect()
            }
            Command::Details => self.details_of_bar(),
            Command::Edit => vec![A::Edit(None)],
            Command::LabelPrev => {
                self.label_cursor = self.label_cursor.saturating_sub(1);
                Vec::new()
            }
            Command::LabelNext => {
                let n = self.bar_row().map_or(0, |r| self.labels_of(r).len());
                self.label_cursor = (self.label_cursor + 1).min(n);
                Vec::new()
            }
            Command::Mark => {
                let Some(row) = self.bar_row() else {
                    return Vec::new();
                };
                let key = RowKey::of(row);
                if !self.marks.remove(&key) {
                    self.marks.insert(key);
                }
                self.move_bar(bar + 1).into_iter().collect()
            }
            Command::Back => match self.pane.take() {
                Some(_) => Vec::new(),
                None => vec![A::Unpin],
            },
            Command::FindWith(c) => {
                self.find = Some(find::Find::new(c.to_string(), bar));
                Vec::new()
            }
            Command::Find => {
                self.find = Some(find::Find::new(String::new(), bar));
                Vec::new()
            }
            Command::ToHead => match self.head_row {
                Some(at) => self.move_bar(at).into_iter().collect(),
                None => {
                    self.said = Some(Status::info("HEAD's commit is not loaded"));
                    Vec::new()
                }
            },
            _ => Vec::new(),
        }
    }

    /// A command with the keyboard in a history search's results.
    fn run_results(&mut self, command: Command) -> Vec<GitAction> {
        use GitAction as A;
        let Some(results) = &mut self.search else {
            return Vec::new();
        };
        let bar = results.scroll.bar;
        match command {
            Command::Bar(step) => {
                let Some(last) = results.rows.len().checked_sub(1) else {
                    return Vec::new();
                };
                let more = matches!(results.paging, Paging::More(_) | Paging::Error(_));
                if step == Step::Down && bar == last && more && !results.more_asked {
                    results.more_asked = true;
                    return vec![A::MoreResults];
                }
                let to = bar
                    .saturating_add_signed(results.scroll.delta(step))
                    .min(last);
                results.scroll.to(to);
                self.follow().into_iter().collect()
            }
            Command::Details => {
                let Some(row) = results.rows.get(bar) else {
                    return Vec::new();
                };
                let subject = Subject::Row(RowKey::of(row));
                let pane = details::Pane::new(subject.clone(), true, Vec::new());
                self.pane = Some(pane.in_panel(self.find_key));
                vec![A::Details(subject)]
            }
            Command::Edit => vec![A::Edit(None)],
            Command::Back => {
                if self.pane.take().is_some() {
                    return Vec::new();
                }
                // Back to the graph, the bar on the result's commit when it is loaded.
                let key = results.rows.get(bar).map(RowKey::of);
                self.search = None;
                let at = key.and_then(|k| self.rows.iter().position(|r| k.is(r)));
                match at {
                    Some(at) => self.move_bar(at).into_iter().collect(),
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// A command with the keyboard in the details pane.
    fn run_pane(&mut self, command: Command) -> Vec<GitAction> {
        use GitAction as A;
        let Some(pane) = &mut self.pane else {
            return Vec::new();
        };
        match command {
            Command::Bar(step) => {
                let len = pane.lines.len();
                let delta = pane.scroll.delta(step);
                pane.scroll.step(delta, len);
                return Vec::new();
            }
            Command::Back => {
                pane.keyboard = false;
                return Vec::new();
            }
            // Find and Cmd/Ctrl+J are the graph's: not while a history search's results hide it.
            Command::FindWith(_) | Command::Find | Command::ToHead if self.search.is_none() => {
                pane.keyboard = false;
                return self.run_graph(command);
            }
            _ => {}
        }
        let subject = pane.subject.clone();
        let line = pane.line().cloned();
        let file = match &line {
            Some(details::Line::File { group, file }) => {
                pane.file(*group, *file).map(|(g, f)| (g, f.clone()))
            }
            _ => None,
        };
        match (command, line, file) {
            (Command::Details, Some(details::Line::Parent { oid, .. }), _) => {
                let key = RowKey::Commit(oid);
                let Some(at) = self.rows.iter().position(|r| key.is(r)) else {
                    self.said = Some(Status::info("that parent is not loaded"));
                    return Vec::new();
                };
                self.search = None;
                self.graph_scroll.to(at);
                self.label_cursor = 0;
                let subject = Subject::Row(key);
                let pane = details::Pane::new(subject.clone(), true, Vec::new());
                self.pane = Some(pane.in_panel(self.find_key));
                vec![A::Details(subject)]
            }
            // A name that is not UTF-8 names nothing: no diff, no file, no folder's listing.
            (
                Command::Details | Command::RevisionFile | Command::Edit | Command::LabelNext,
                _,
                Some((_, f)),
            ) if f.lossy => {
                self.said = Some(Status::error(not_utf8(&f.shown())));
                Vec::new()
            }
            (Command::Details | Command::LabelNext, _, Some((_, f))) if f.is_folder() => {
                vec![A::ListFolder {
                    subject,
                    dir: f.path,
                }]
            }
            (Command::Details, _, Some((group, file))) => {
                self.viewed.insert((subject.clone(), file.path.clone()));
                vec![A::Diff {
                    subject,
                    group,
                    file,
                }]
            }
            (Command::RevisionFile, _, Some((group, file))) => vec![A::RevisionFile {
                subject,
                group,
                file,
            }],
            (Command::Edit, _, Some((_, f))) => vec![A::Edit(Some(f.path))],
            (Command::Edit, _, None) => vec![A::Edit(None)],
            _ => Vec::new(),
        }
    }

    /// More rows, once, when the bar has come within one screen of the last loaded row (03 §4.5).
    fn ask_more(&mut self) -> Option<GitAction> {
        if let Some(s) = &mut self.search {
            let near = s.scroll.bar + s.scroll.visible >= s.rows.len().saturating_sub(1);
            if matches!(s.paging, Paging::More(_)) && !s.more_asked && near {
                s.more_asked = true;
                return Some(GitAction::MoreResults);
            }
            return None;
        }
        let near =
            self.graph_scroll.bar + self.graph_scroll.visible >= self.rows.len().saturating_sub(1);
        if matches!(self.paging, Paging::More(_))
            && !self.more_asked
            && near
            && !self.rows.is_empty()
        {
            self.more_asked = true;
            return Some(GitAction::MoreRows);
        }
        None
    }

    /// Whether another panel's dialog is up: the integration says so before each pass.
    pub fn set_dialog_elsewhere(&mut self, up: bool) {
        self.dialog_elsewhere = up;
    }

    /// Every dialog slot of the panel's, by [`Front`]: whether it is up, and its serial
    /// (F-git-listq). The integration compares them across its own pass (`Browser::ui`): a slot
    /// that opened, was replaced (by another of its kind too: the serial) or closed there holds
    /// what the owner's keys of that pass were not pressed at — whichever of the panel's dialogs
    /// those keys would reach, so no order of precedence between them is guessed at.
    pub fn fronts(&self) -> [(bool, u64); Front::KINDS] {
        let planner = self.planner.as_ref().is_some_and(|p| p.dialog_up());
        let entry = self.log.as_ref().is_some_and(|l| l.whole().is_some());
        // In `Front`'s order.
        let up = [
            planner,
            entry,
            self.filters_dialog.is_some(),
            self.search_dialog.is_some(),
            self.menu.is_some(),
            self.quick.is_some(),
            self.dialog.is_some(),
            self.result.is_some(),
            self.stop_question.is_some(),
        ];
        std::array::from_fn(|k| (up[k], self.front_serials[k]))
    }

    /// Slot `kind` set or cleared: its serial moves on ([`GitView::fronts`]).
    fn bump(&mut self, kind: Front) {
        self.front_serials[kind as usize] += 1;
    }

    /// A dialog is up — the filters', history search's, S3's menu, action dialog, result, "Stop
    /// git?", the log's whole entry, S4's quick edit dialog and the planner's own (its message,
    /// author, question and run): it has the keyboard (its own Enter and Esc), and Esc is its.
    pub fn dialog_up(&self) -> bool {
        self.other_dialog_up() || self.planner.as_ref().is_some_and(|p| p.dialog_up())
    }

    /// Whether this pass's draw may open a box of the panel's after a side drawn before the panel
    /// was painted (F-menu-own): a choice in the open menu (its dialog), a right click on a row
    /// (the row's menu), or a key after text typed into Find (an Esc there hands the pass's rest
    /// to [`keys`] in the draw, F10 the menu). The browser keeps that side's paints in those
    /// passes alone (`cells::keep`).
    pub fn may_open_in_draw(&self, ctx: &egui::Context) -> bool {
        let pressed = || {
            let key = |e: &egui::Event| matches!(e, egui::Event::Key { pressed: true, .. });
            ctx.input(|i| i.events.iter().any(key))
        };
        self.menu.is_some()
            || ctx.input(|i| i.pointer.secondary_clicked())
            || (self.find.is_some() && pressed())
    }

    /// The cells of `grid` (the panels') the panel's open dialogs cover, their boxes and shadows,
    /// each as its own file says ([`menu::HistoryMenu::covers`], …: nothing while egui draws it):
    /// the panel blanks them, and the other side's list, preview or tree
    /// (`Browser::git_covered`), before they are painted (10 §17, X4-graph).
    pub fn covered(&self, grid: &Grid) -> Vec<Cells> {
        let mut out = Vec::new();
        if let Some(d) = &self.filters_dialog {
            out.extend(d.covers(grid));
        }
        if let Some(d) = &self.search_dialog {
            out.extend(d.covers(grid));
        }
        // `modals` draws the first of these alone, in this order: the others wait behind it and
        // cover nothing (X4-dialogs2, round 1).
        if let Some(m) = &self.menu {
            out.extend(m.covers(grid));
        } else if let Some(d) = &self.quick {
            out.extend(d.covers(grid));
        } else if let Some(d) = &self.dialog {
            out.extend(d.covers(grid));
        } else if let Some(r) = &self.result {
            out.extend(r.covers(grid));
        } else if let Some((what, _)) = &self.stop_question {
            out.extend(stop_covers(grid, what));
        }
        // Under the planner the body is the planner's: the log, and an entry it shows whole, are
        // not drawn, so they cover nothing (F-dialog-wrap, round 1).
        if let (Some(log), None) = (&self.log, &self.planner) {
            out.extend(log.covers(grid));
        }
        if let Some(p) = &self.planner {
            out.extend(p.covers(grid));
        }
        #[cfg(test)]
        out.extend(self.test_covers.iter().copied());
        out
    }

    /// One of [`GitView::modals`]' dialogs is up — the menu, a quick edit's, an action's, the
    /// result, "Stop git?" — drawn over the filters and the history search.
    fn modal_up(&self) -> bool {
        self.menu.is_some()
            || self.quick.is_some()
            || self.dialog.is_some()
            || self.result.is_some()
            || self.stop_question.is_some()
    }

    /// A dialog is up that is not the planner's own: the pane then takes no key.
    fn other_dialog_up(&self) -> bool {
        self.filters_dialog.is_some()
            || self.search_dialog.is_some()
            || self.menu.is_some()
            || self.dialog.is_some()
            || self.quick.is_some()
            || self.result.is_some()
            || self.stop_question.is_some()
            || self.log.as_ref().is_some_and(|l| l.whole().is_some())
    }
}

// ---------------------------------------------------------------------------------------------
// S3: the actions (03 §7–§9; S3-C wires S3-B's screens).

/// The ids whose dialog starts from reads the integration makes ([`Prefill`], 03 §7): `amend`'s
/// last message, `commit`'s prepared squash message (after `merge --squash`), `move-tag`'s tag
/// messages, `edit-remote`'s URLs (masked, 03 §3.7), `user`'s name
/// and email, `push`'s push remote, `archive`'s download folder.
pub fn prefilled(id: ActionId) -> bool {
    use ActionId as A;
    matches!(
        id,
        A::Amend | A::Commit | A::MoveTag | A::EditRemote | A::User | A::Push | A::Archive
    )
}

/// The F-key slots whose action depends on the object under the bar (03 §9.1).
const ACTION_SLOTS: [Command; 6] = [
    Command::Reword,
    Command::Pick,
    Command::Rebase,
    Command::Branch,
    Command::Remove,
    Command::Menu,
];

impl GitView {
    /// The repository as a refresh read it (03 §5): the actions' snapshot and the banner (§8.5);
    /// an open dialog plans again against it. Its `seen` is the refresh's, empty (S3-A: reads
    /// beyond the refresh are made again after one).
    pub fn set_repo(&mut self, repo: Repo) {
        self.banner = in_progress::BannerView::probe(&repo.in_progress, &repo.status);
        self.stop = edit_stop(&repo);
        self.repo = Some(repo);
        self.repo_generation += 1;
        self.feed_dialogs();
    }

    /// The open dialogs plan again against the snapshot as it is now (with its reads): S3's
    /// action dialog, a quick edit's dialog, the planner (its model, the owner's plan, stays).
    fn feed_dialogs(&mut self) {
        let Some(repo) = &self.repo else {
            return;
        };
        if let Some(dialog) = &mut self.dialog {
            dialog.set_repo(repo.clone());
        }
        if let Some(quick) = &mut self.quick {
            quick.set_repo(repo.clone());
        }
        if let Some(planner) = &mut self.planner {
            planner.set_repo(repo.clone());
        }
    }

    /// The snapshot the actions plan against, and its generation ([`GitView::seen`]).
    pub fn repo(&self) -> Option<(&Repo, u64)> {
        self.repo.as_ref().map(|r| (r, self.repo_generation))
    }

    /// The reads made for snapshot `generation` (`git::actions::inspect`): kept with it, and the
    /// open dialog plans again with them. Dropped when a refresh replaced the snapshot meanwhile.
    pub fn seen(&mut self, generation: u64, seen: actions::Seen) {
        if generation != self.repo_generation {
            return;
        }
        let Some(repo) = &mut self.repo else {
            return;
        };
        for (need, stdout) in seen.reads {
            repo.seen.put(need, stdout);
        }
        self.feed_dialogs();
    }

    /// The open dialog's draft: what `git::actions::needs` reads for.
    pub fn draft(&self) -> Option<&Action> {
        self.dialog.as_ref().map(|d| d.draft())
    }

    /// The newest group this window wrote for this repository (Cmd/Ctrl+Z, 03 §8.3).
    pub fn set_undo(&mut self, stamp: Option<String>) {
        self.undo = stamp;
    }

    /// An action's dialog with what the integration read for it ([`GitAction::Open`]), or the
    /// action at once (03 §8.1's "No confirmation"), or a view.
    pub fn open_action(&mut self, id: ActionId, target: Target, read: Prefill) -> Vec<GitAction> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        let prefill = Prefill {
            checkout_unasked: self.unasked,
            ..read
        };
        let opened = dialogs::ActionDialog::open(id, &target, repo, &prefill, dialogs::PLAN);
        self.opened(opened, target)
    }

    /// Group `stamp`'s restore dialog (03 §8.3), its entries read now; `only`: one backup ref.
    pub fn open_restore(
        &mut self,
        stamp: String,
        entries: Vec<RestoreEntry>,
        only: Option<&str>,
    ) -> Vec<GitAction> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        let opened = dialogs::ActionDialog::restore(stamp, entries, only, repo, dialogs::PLAN);
        self.opened(opened, Target::Repository)
    }

    /// The Backups list (03 §8.2) in the body, or read again while it shows (the bar stays).
    pub fn open_backups(&mut self, records: Vec<Record>) {
        // Its keys are its own: Find, which would keep Enter and Esc, closes (X4-find).
        self.find = None;
        match &mut self.backups {
            Some(list) => list.set_records(records),
            None => {
                let list = backups::BackupsView::new(records).in_panel(self.find_key);
                self.backups = Some(list);
            }
        }
        self.log = None;
        self.bump(Front::Entry);
    }

    /// The Backups list's groups as it shows them (`None` while it is closed).
    pub fn backups(&self) -> Option<&[Record]> {
        self.backups.as_ref().map(|b| b.records())
    }

    /// The command log (03 §3.7) in the body, newest first.
    pub fn open_log(&mut self, entries: Vec<LogEntry>) {
        // Its keys are its own: Find, which would keep Enter and Esc, closes (X4-find).
        self.find = None;
        self.log = Some(log_view::LogView::new(entries).in_panel(self.find_key));
        self.bump(Front::Entry);
        self.backups = None;
    }

    /// The log's newest entry as it shows (`None` while it is closed): the integration hands the
    /// entries again only when the runner's log has a newer one.
    pub fn log_newest(&self) -> Option<Option<&LogEntry>> {
        self.log.as_ref().map(|l| l.entries().first())
    }

    /// The log's entries again, while it shows (its bar stays on its entry).
    pub fn set_log(&mut self, entries: Vec<LogEntry>) {
        // No slot changes here: the entry shown whole is found again by itself, or goes (its
        // slot's up flag says so, `GitView::fronts`).
        if let Some(log) = &mut self.log {
            log.set_entries(entries);
        }
    }

    /// A run ended (03 §3.8, §8.3): the error dialog when a step failed or the backup is
    /// incomplete (S3-A's `Outcome::warning`, said there in place of the undo line), waiting
    /// behind a result up ([`GitView::show_result`]). The status row says this run's words
    /// whatever result is up: `<title>: failed` in the error colour when a step failed, else
    /// `<what> · backup <stamp> · Cmd/Ctrl+Z undoes`, or the warning in the error colour. A run
    /// with no dialog leaves a result up as it is (F-git-outcome).
    pub fn show_outcome(&mut self, title: &str, what: &str, outcome: &Outcome) -> Status {
        if let Some(result) = dialogs::ResultDialog::of(title, outcome) {
            self.show_result(result);
        }
        let failed = outcome
            .steps
            .iter()
            .any(|s| s.outcome == actions::StepOutcome::Failed);
        match (failed, outcome.warning()) {
            (true, _) => Status::error(format!("{title}: failed")),
            (false, Some(warning)) => Status::error(format!("{what} · {warning}")),
            (false, None) => Status::info(dialogs::status_text(what, outcome)),
        }
    }

    /// A text in a dialog of its own: a read's failure, a tag's details (03 §3.8, §7.4), waiting
    /// behind a result up ([`GitView::show_result`]).
    pub fn show_text(&mut self, title: &str, text: &str, command_line: Option<&str>) {
        self.show_result(dialogs::ResultDialog::error(title, text, command_line));
    }

    /// `result` up, or — while one is up, which stays until the owner closes it — waiting behind
    /// it, in the order they came (F-git-outcome); past [`RESULTS_WAITING`] the oldest waiting
    /// goes, counted for the next one to come up.
    fn show_result(&mut self, result: dialogs::ResultDialog) {
        if self.result.is_none() {
            self.result = Some(result);
            self.result_screen.next();
            self.bump(Front::Result);
            return;
        }
        if self.waiting.len() >= RESULTS_WAITING {
            self.waiting.pop_front();
            self.results_dropped += 1;
        }
        self.waiting.push_back(result);
    }

    /// "Stop git?" (03 §3.4) over the write `what`, the run of id `run`, running for 10 s.
    pub fn ask_stop(&mut self, what: String, run: u64) {
        self.stop_question = Some((what, run));
        self.bump(Front::Stop);
    }

    /// "Stop git?" goes once the run it asks about no longer runs — `runs` says whether the run of
    /// that id still does — whatever another run's words are (X4-dialogs2, round 1, fix-check).
    pub fn forget_stop_unless(&mut self, runs: impl Fn(u64) -> bool) {
        if self
            .stop_question
            .as_ref()
            .is_some_and(|(_, id)| !runs(*id))
        {
            self.stop_question = None;
            self.bump(Front::Stop);
        }
    }

    /// The confirmation of `action` again, as a dialog (review 1: a run whose shown plan was lost
    /// and whose plan now asks for a confirmation). Never a run from here: a plan that asks for
    /// none says so on the status row instead.
    pub fn reconfirm(&mut self, action: Action) -> Vec<GitAction> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        match dialogs::ActionDialog::confirm(action, repo, dialogs::PLAN) {
            dialogs::Opened::Dialog(dialog) => {
                self.dialog = Some(dialog);
                self.bump(Front::Dialog);
            }
            dialogs::Opened::Run(action) => {
                let what = menu::label(action.id()).trim_end_matches('…');
                self.said = Some(Status::info(format!("{what}: run it again")));
            }
            dialogs::Opened::View(_) | dialogs::Opened::NotHere(_) => {}
        }
        Vec::new()
    }

    /// A second dialog for an action git refused (03 §7.2: `delete-branch`'s "not fully merged":
    /// the same action, forced), in place of the error dialog it answers: the newest result,
    /// which the run's end has just put up or set waiting — never a result up before it
    /// (F-git-outcome).
    pub fn confirm_again(&mut self, action: Action) -> Vec<GitAction> {
        if self.waiting.pop_back().is_none() {
            self.result = None;
            self.bump(Front::Result);
        }
        self.confirm(action)
    }

    /// `SOPEN_STATE dialog=<id>` (02 §9): an action's id on the object under the bar, else on the
    /// first of the repository, the uncommitted row and its changed files whose menu offers it;
    /// the banner's steps through the banner. A history edit's id (03 §7.8.1, S4) on the bar's
    /// commit, else on HEAD's, where its history row offers it. An id offered on none of them
    /// is said on the status row.
    pub fn open_by_id(&mut self, id: DialogId) -> Vec<GitAction> {
        match id {
            DialogId::Action(id) => self.open_action_by_id(id),
            DialogId::Edit(id) => self.open_edit_by_id(id),
        }
    }

    /// [`GitView::open_by_id`] of an action of 03 §7.1–§7.7.
    fn open_action_by_id(&mut self, id: ActionId) -> Vec<GitAction> {
        use ActionId as A;
        self.said = None;
        let step = match id {
            A::Continue => Some(Proceed::Continue),
            A::Skip => Some(Proceed::Skip),
            A::Abort => Some(Proceed::Abort),
            A::BisectReset => Some(Proceed::BisectReset),
            _ => None,
        };
        if let Some(p) = step {
            return self.proceed(p);
        }
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        let files: Vec<StatusEntry> = repo
            .status
            .entries
            .iter()
            .filter(|e| !e.lossy)
            .cloned()
            .collect();
        let mut targets: Vec<Target> = self.object().into_iter().collect();
        targets.extend([
            Target::Repository,
            Target::Uncommitted,
            Target::Files(files),
        ]);
        let offered = targets
            .into_iter()
            .find(|t| menu::items(t, repo).iter().any(|item| item.id == id));
        match offered {
            Some(target) => self.act(id, target),
            None => {
                self.said = Some(Status::error(format!(
                    "SOPEN_STATE dialog={}: not offered on the row under the bar",
                    id.as_str()
                )));
                Vec::new()
            }
        }
    }

    /// The plan the confirmation showed for `action` (taken once), when it showed one.
    pub fn take_plan(&mut self, action: &Action) -> Option<Plan> {
        let (shown, plan) = self.confirmed.take()?;
        (shown == *action).then_some(plan)
    }

    /// The details pane's subject, while it is open.
    pub fn pane_subject(&self) -> Option<&Subject> {
        self.pane.as_ref().map(|p| &p.subject)
    }

    /// The ref under the label cursor (03 §9.2): the label's `RefLabel` on the bar's commit.
    fn bar_ref(&self) -> Option<RefLabel> {
        let label = self.bar_label()?;
        let Row::Commit { commit, .. } = self.bar_row()? else {
            return None;
        };
        let kind = match label.kind {
            LabelKind::Local | LabelKind::CheckedOut => RefKind::LocalBranch,
            LabelKind::Remote => RefKind::RemoteBranch,
            LabelKind::Tag => RefKind::Tag,
            LabelKind::Head | LabelKind::Stash => return None,
        };
        let text = label.text.trim_start_matches(chrome::git::CURRENT);
        let name = text.split(chrome::git::BOTH).next().unwrap_or(text);
        self.refs
            .iter()
            .filter(|r| r.peeled.as_ref().unwrap_or(&r.object) == &commit.oid)
            .find(|r| r.kind == kind && filters::short_name(&r.refname) == name)
            .cloned()
    }

    /// What the F-keys and the menu act on (03 §9.1): a history search's commit; the details
    /// pane's file of the uncommitted row while the pane has the keyboard; the label under the
    /// label cursor; else the bar's row — a commit with the marked commits, newest first.
    fn object(&self) -> Option<Target> {
        let repo = self.repo.as_ref()?;
        if let Some(results) = &self.search {
            return match results.rows.get(results.scroll.bar)? {
                Row::Commit { commit, .. } => Some(Target::Commit {
                    commit: commit.clone(),
                    marked: Vec::new(),
                }),
                _ => None,
            };
        }
        let pane = self.pane.as_ref().filter(|p| p.keyboard);
        if let Some(pane) = pane.filter(|p| p.subject == Subject::Row(RowKey::Uncommitted)) {
            if let Some(details::Line::File { group, file }) = pane.line() {
                let (_, f) = pane.file(*group, *file)?;
                let entry = repo.status.entries.iter().find(|e| e.path == f.path)?;
                return Some(Target::Files(vec![entry.clone()]));
            }
            return Some(Target::Uncommitted);
        }
        match self.bar_row()? {
            Row::Uncommitted { .. } => Some(Target::Uncommitted),
            Row::Stash(stash) => Some(Target::Stash(stash.clone())),
            Row::Commit { commit, .. } => {
                if let Some(r) = self.bar_ref() {
                    return Some(Target::Label(r));
                }
                let marked = self
                    .rows
                    .iter()
                    .filter_map(|r| match r {
                        Row::Commit { commit, .. }
                            if self.marks.contains(&RowKey::Commit(commit.oid.clone())) =>
                        {
                            Some(commit.clone())
                        }
                        _ => None,
                    })
                    .collect();
                Some(Target::Commit {
                    commit: commit.clone(),
                    marked,
                })
            }
        }
    }

    /// The action an F-key's slot gives on `object` and the target it acts on (03 §9.1's table);
    /// `None` where the slot does nothing.
    fn slot(&self, command: Command, object: &Target) -> Option<(ActionId, Target)> {
        use ActionId as A;
        let here = |id| Some((id, object.clone()));
        let local = |r: &RefLabel| r.kind == RefKind::LocalBranch;
        match (command, object) {
            (Command::Reword, Target::Label(r)) if local(r) => here(A::Rename),
            (Command::Pick, Target::Commit { .. }) => here(A::Pick),
            // A label: the pick of its commit.
            (Command::Pick, Target::Label(_)) => match self.bar_row()? {
                Row::Commit { commit, .. } => Some((
                    A::Pick,
                    Target::Commit {
                        commit: commit.clone(),
                        marked: Vec::new(),
                    },
                )),
                _ => None,
            },
            (Command::Pick, Target::Stash(_)) => here(A::StashApply),
            (Command::Rebase, Target::Commit { .. }) => here(A::Rebase),
            (Command::Rebase, Target::Label(r)) if local(r) => here(A::Rebase),
            (Command::Branch, Target::Commit { .. }) => here(A::Branch),
            (Command::Branch, Target::Uncommitted | Target::Files(_)) => {
                Some((A::Commit, Target::Uncommitted))
            }
            (Command::Branch, Target::Stash(_)) => here(A::StashBranch),
            (Command::Remove, Target::Label(r)) => here(match r.kind {
                RefKind::LocalBranch => A::DeleteBranch,
                RefKind::RemoteBranch => A::DeleteRemoteBranch,
                RefKind::Tag => A::DeleteTag,
            }),
            (Command::Remove, Target::Uncommitted) => here(A::ResetChanges),
            (Command::Remove, Target::Files(_)) => here(A::Discard),
            (Command::Remove, Target::Stash(_)) => here(A::StashDrop),
            _ => None,
        }
    }

    /// The F-key row while this panel has the keyboard (03 §9.1): [`fkeys`]' slots, and the
    /// action slots that do something on the object under the bar — F7 says `Commit` on the
    /// uncommitted row; a slot that does nothing there is left empty (02 §7: the row lists what
    /// works). Without a snapshot, [`fkeys`]' alone.
    pub fn fkeys(&self, mode: Mode) -> Vec<FKey> {
        let mut row = fkeys(mode);
        // The planner over the body: only the window's slots work (F1, F9); the graph's object
        // is not on screen.
        if self.planner.is_some() {
            let window: Vec<u8> = window_rows()
                .filter_map(|r| match r.trigger {
                    Trigger::Key(key, Mods::None) => chrome::fkey_number(key),
                    _ => None,
                })
                .collect();
            row.retain(|k| window.contains(&k.n));
            return row;
        }
        // The log or the Backups list: what it does (F3 shows; the Backups list's F8 deletes).
        if self.log.is_some() || self.backups.is_some() {
            if self.backups.is_some() {
                row.push(FKey {
                    n: 8,
                    label: "Delete",
                });
                row.sort_by_key(|k| k.n);
            }
            return row;
        }
        let Some(object) = self.object() else {
            return row;
        };
        for r in keys::rows(Context::Git, UPTO) {
            let Trigger::Key(key, Mods::None) = r.trigger else {
                continue;
            };
            if r.label.is_empty() || !ACTION_SLOTS.contains(&r.command) {
                continue;
            }
            if r.command != Command::Menu
                && self.slot(r.command, &object).is_none()
                && self.edit_slot(r.command, &object).is_none()
            {
                continue;
            }
            let label = match (r.command, &object) {
                (Command::Branch, Target::Uncommitted | Target::Files(_)) => "Commit",
                _ => r.label,
            };
            row.extend(chrome::fkey_number(key).map(|n| FKey { n, label }));
        }
        row.sort_by_key(|k| k.n);
        row
    }

    /// `id` on `target`: its dialog — through the integration first when it starts from reads
    /// ([`prefilled`]) — the action at once, or a view.
    fn act(&mut self, id: ActionId, target: Target) -> Vec<GitAction> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        if prefilled(id) && menu::items(&target, repo).iter().any(|i| i.id == id) {
            return vec![GitAction::Open { id, target }];
        }
        let prefill = Prefill {
            checkout_unasked: self.unasked,
            ..Prefill::default()
        };
        let opened = dialogs::ActionDialog::open(id, &target, repo, &prefill, dialogs::PLAN);
        self.opened(opened, target)
    }

    /// An action built elsewhere, through its confirmation (`ActionDialog::confirm`): the
    /// banner's steps, the Backups list's Delete, Fetch all.
    fn confirm(&mut self, action: Action) -> Vec<GitAction> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        let opened = dialogs::ActionDialog::confirm(action, repo, dialogs::PLAN);
        self.opened(opened, Target::Repository)
    }

    fn opened(&mut self, opened: dialogs::Opened, target: Target) -> Vec<GitAction> {
        match opened {
            dialogs::Opened::View(id) => self.view_id(id, target),
            dialogs::Opened::NotHere(id) => {
                let what = menu::label(id).trim_end_matches('…');
                self.said = Some(Status::info(format!("{what}: not offered here")));
                Vec::new()
            }
            dialogs::Opened::Run(action) => vec![GitAction::Run(action)],
            dialogs::Opened::Dialog(dialog) => {
                self.dialog = Some(dialog);
                self.bump(Front::Dialog);
                Vec::new()
            }
        }
    }

    /// A view id of the menu (`menu::is_view`): the panel's own, no plan (03 §8.1).
    fn view_id(&mut self, id: ActionId, target: Target) -> Vec<GitAction> {
        use ActionId as A;
        match (id, &target) {
            (A::Compare, _) => self.details_of_bar(),
            (A::CopyHash, Target::Commit { commit, .. }) => self.copy(commit.oid.as_str()),
            (A::CopyHash, Target::Stash(s)) => self.copy(s.oid.as_str()),
            (A::CopySubject, Target::Commit { commit, .. }) => self.copy(&commit.subject.clone()),
            (A::CopyName, Target::Label(r)) => self.copy(r.name()),
            (A::CopyName, Target::Stash(s)) => self.copy(&s.selector.clone()),
            (A::OnlyBranch, Target::Label(r)) => vec![self.only_branch(&r.refname.clone())],
            (A::AllBranches, _) => vec![self.all_branches()],
            (A::TagDetails, Target::Label(r)) => vec![GitAction::TagDetails(r.refname.clone())],
            (A::Editor, Target::Files(files)) if files.len() == 1 => {
                vec![GitAction::Edit(Some(files[0].path.clone()))]
            }
            (A::Editor, _) => vec![GitAction::Edit(None)],
            (A::RemoteVisibility | A::Filters, _) => {
                self.open_filters();
                Vec::new()
            }
            (A::Refresh, _) => vec![GitAction::Refresh],
            (A::Find, _) => {
                self.find = Some(find::Find::new(String::new(), self.graph_scroll.bar));
                Vec::new()
            }
            (A::Search, _) => {
                self.bump(Front::Search);
                self.search_dialog = Some(find::SearchDialog::default());
                Vec::new()
            }
            (A::Log, _) => vec![GitAction::Log],
            (A::Backups, _) => vec![GitAction::Backups],
            (A::Undo, _) => self.undo_newest(),
            _ => Vec::new(),
        }
    }

    /// `text` to the clipboard on the next pass, and said.
    fn copy(&mut self, text: &str) -> Vec<GitAction> {
        let shown: String = text.chars().take(40).collect();
        self.said = Some(Status::info(format!("copied {shown}")));
        self.to_copy = Some(text.to_string());
        Vec::new()
    }

    /// Cmd/Ctrl+Z (03 §8.3): the restore of the newest group this window wrote here.
    fn undo_newest(&mut self) -> Vec<GitAction> {
        match &self.undo {
            Some(stamp) => vec![GitAction::Restore {
                stamp: stamp.clone(),
                only: None,
            }],
            None => {
                if self.repo.is_some() {
                    self.said = Some(Status::info(
                        "nothing to undo: this window wrote no backup here (Cmd/Ctrl+B lists them all)"
                            .replace("Cmd/Ctrl+", &Mods::Command.prefix()),
                    ));
                }
                Vec::new()
            }
        }
    }

    /// A banner step (03 §8.5, §9.2): through its dialog, or the status row's words.
    fn proceed(&mut self, p: Proceed) -> Vec<GitAction> {
        match &self.banner {
            Some(banner) => {
                let pressed = banner.press(p);
                self.pressed(pressed)
            }
            None => {
                if self.repo.is_some() {
                    let step = in_progress::word(p);
                    self.said = Some(Status::info(format!("{step}: nothing is in progress")));
                }
                Vec::new()
            }
        }
    }

    fn pressed(&mut self, pressed: in_progress::Pressed) -> Vec<GitAction> {
        use in_progress::Pressed as P;
        match (pressed, self.split.clone()) {
            // A kept split (03 §7.8.1 `split`): Continue is refused while a recorded path still
            // differs from HEAD — `Edit::ContinueSplit`'s plan, through its dialog. The one
            // place every Continue (key, button, `dialog=continue`) goes through.
            (P::Proceed(Action::Proceed(Proceed::Continue)), Some(split)) => {
                self.confirm_edit(Edit::ContinueSplit(split))
            }
            (P::Proceed(action), _) => self.confirm(action),
            (P::NotOffered(text), _) => {
                self.said = Some(Status::info(text));
                Vec::new()
            }
        }
    }

    /// Space on a file of the uncommitted row (03 §7.6, §9.2): a staged file is unstaged, any
    /// other staged, then the bar steps down; a conflict says where its steps are.
    fn stage_toggle(&mut self, target: Target) -> Vec<GitAction> {
        let group = self.pane.as_ref().and_then(|p| match p.line()? {
            details::Line::File { group, file } => p.file(*group, *file).map(|(g, _)| g),
            _ => None,
        });
        let id = match group {
            Some(Group::Staged) => ActionId::Unstage,
            Some(Group::Conflicts) => {
                self.said = Some(Status::info(
                    "a conflict: Take ours, Take theirs and Mark resolved are in its menu (F10)",
                ));
                return Vec::new();
            }
            _ => ActionId::Stage,
        };
        let asked = self.act(id, target);
        if let Some(pane) = &mut self.pane {
            let len = pane.lines.len();
            pane.scroll.step(1, len);
        }
        asked
    }

    /// The F-keys' actions (03 §9.1) on the object under the bar: on a commit F2 and F8 are its
    /// history edits `reword` and `drop` (S4), and say why where they are not offered.
    fn fkey(&mut self, command: Command) -> Vec<GitAction> {
        let Some(object) = self.object() else {
            return Vec::new();
        };
        if let Some((id, commit)) = self.edit_slot(command, &object) {
            return self.act_edit(id, commit);
        }
        match self.slot(command, &object) {
            Some((id, target)) => self.act(id, target),
            None => {
                let refused = match (command, &object) {
                    (Command::Reword, Target::Commit { commit, .. }) => {
                        Some((EditId::Reword, commit.clone()))
                    }
                    (Command::Remove, Target::Commit { commit, .. }) => {
                        Some((EditId::Drop, commit.clone()))
                    }
                    _ => None,
                };
                if let Some((id, commit)) = refused {
                    self.said = Some(Status::info(self.not_offered(id, &commit)));
                }
                Vec::new()
            }
        }
    }

    /// The menu of the object under the bar (03 §7, §9: F10, Shift+F10, a right click), a
    /// commit's with its history row (S4) as the loaded graph allows it.
    fn open_menu(&mut self) {
        let Some(target) = self.object() else {
            return;
        };
        let eligible = match &target {
            Target::Commit { commit, .. } => self.eligibility(&commit.oid),
            _ => menu::Eligibility::default(),
        };
        if let Some(repo) = &self.repo {
            self.menu = Some(menu::HistoryMenu::new(target, eligible, repo));
            self.bump(Front::Menu);
        }
    }

    /// What the Backups list asks for (03 §8.2–§8.3).
    fn backups_end(&mut self, end: Option<backups::BackupsEnd>) -> Vec<GitAction> {
        use backups::BackupsEnd as E;
        match end {
            None => Vec::new(),
            Some(E::Close) => {
                self.backups = None;
                Vec::new()
            }
            Some(E::Restore { stamp, only }) => vec![GitAction::Restore {
                stamp,
                only: only.map(|r| r.refname),
            }],
            Some(E::Show(refs)) => {
                self.backups = None;
                vec![GitAction::ShowBackup(refs)]
            }
            Some(E::Delete(action)) => self.confirm(action),
        }
    }

    /// A command while the log or the Backups list shows in the body (03 §9.2): the list's own
    /// rows (the log: the bar, Enter or F3 to show an entry whole, Esc; the Backups list: also
    /// Cmd/Ctrl+Z and F8 on the line under its bar); Cmd/Ctrl+L or B closes its list or opens the
    /// other; anything else is taken and does nothing.
    fn run_list(&mut self, ctx: &egui::Context, command: Command) -> Vec<GitAction> {
        if let Some(log) = &mut self.log {
            return match command {
                Command::Bar(_) | Command::Details | Command::Back => {
                    if log.run(command) == Some(log_view::LogEnd::Close) {
                        self.log = None;
                        self.bump(Front::Entry);
                    }
                    Vec::new()
                }
                Command::Log => {
                    self.log = None;
                    self.bump(Front::Entry);
                    Vec::new()
                }
                Command::Backups => vec![GitAction::Backups],
                _ => Vec::new(),
            };
        }
        if let Some(list) = &mut self.backups {
            return match command {
                Command::Bar(_)
                | Command::Details
                | Command::Undo
                | Command::Remove
                | Command::Back => {
                    let end = list.run(ctx, command);
                    self.backups_end(end)
                }
                Command::Backups => {
                    self.backups = None;
                    Vec::new()
                }
                Command::Log => vec![GitAction::Log],
                _ => Vec::new(),
            };
        }
        Vec::new()
    }

    /// S3's commands (03 §9) on the graph: the F-keys' actions, the menu, a tag's details, Space
    /// on a file of the uncommitted row, Cmd/Ctrl+Z, B, L, Shift+R and the banner's keys. `None`
    /// for a command left to S2's keys. (The log and the Backups list take theirs first,
    /// [`GitView::run_list`].)
    fn run_actions(&mut self, command: Command) -> Option<Vec<GitAction>> {
        let asked = match command {
            Command::Menu => {
                self.open_menu();
                Vec::new()
            }
            Command::Reword
            | Command::Pick
            | Command::Rebase
            | Command::Branch
            | Command::Remove => self.fkey(command),
            Command::Details => match self.object() {
                Some(Target::Label(r))
                    if r.kind == RefKind::Tag && r.object_type == ObjectType::Tag =>
                {
                    vec![GitAction::TagDetails(r.refname)]
                }
                _ => return None,
            },
            Command::Mark => match self.object() {
                Some(target @ Target::Files(_)) => self.stage_toggle(target),
                _ => return None,
            },
            Command::Undo => self.undo_newest(),
            Command::Backups => vec![GitAction::Backups],
            Command::Log => vec![GitAction::Log],
            Command::FetchAll => self.confirm(Action::Fetch {
                prune: false,
                prune_tags: false,
            }),
            Command::Continue => self.proceed(Proceed::Continue),
            Command::Skip => self.proceed(Proceed::Skip),
            Command::Abort => self.proceed(Proceed::Abort),
            Command::BisectReset => self.proceed(Proceed::BisectReset),
            // S4: the planner from the bar's commit.
            Command::Planner => self.plan_from_bar(),
            _ => return None,
        };
        Some(asked)
    }

    /// S3's modals for one pass on `grid` (the panels'; each takes its keys before anything else
    /// sees them): the menu, the action dialog, the result, "Stop git?"; the clipboard's text.
    /// `blank`: blanks the panel's cells under a box before that box is drawn, where the panel
    /// painted them before `modals` ran (F-menu-own).
    fn modals(
        &mut self,
        ctx: &egui::Context,
        grid: Grid,
        blank: &mut dyn FnMut(&[Cells]),
    ) -> Vec<GitAction> {
        if let Some(text) = self.to_copy.take() {
            ctx.copy_text(text);
        }
        if let Some(menu) = &mut self.menu {
            let mut asked = match menu.ui(ctx, grid) {
                Some(menu::HistoryMenuEnd::Menu(menu::MenuEnd::Chose { id, target })) => {
                    self.menu = None;
                    self.act(id, target)
                }
                // A key line of the repository's menu (10 T17): as its key runs it.
                Some(menu::HistoryMenuEnd::Menu(menu::MenuEnd::Key(command))) => {
                    self.menu = None;
                    self.run(ctx, command)
                }
                Some(menu::HistoryMenuEnd::Menu(menu::MenuEnd::Cancel)) => {
                    self.menu = None;
                    Vec::new()
                }
                Some(menu::HistoryMenuEnd::Edit { id, commit }) => {
                    self.menu = None;
                    self.act_edit(id, commit)
                }
                None => Vec::new(),
            };
            // The dialog a choice opened, in the same pass: the menu handed it what the pass held
            // after the choosing Enter, typed text in its place (X4-menu). The panel painted its
            // cells with the menu's covers blanked, so the cells this dialog covers are blanked
            // and painted again first, the dialog over them (F-menu-own). Drawing it from the
            // next pass instead would carry the rest of this pass's keys across a pass and blank,
            // on a side drawn after the panel, cells under a dialog not drawn yet; painting the
            // panel after `modals` would record its cells after its dialogs' in every pass a
            // dialog is up.
            if self.menu.is_none() && (self.quick.is_some() || self.dialog.is_some()) {
                let covers = match (&self.quick, &self.dialog) {
                    (Some(quick), _) => quick.covers(&grid),
                    (None, Some(dialog)) => dialog.covers(&grid),
                    (None, None) => Vec::new(),
                };
                blank(&covers);
                asked.extend(self.modals(ctx, grid, blank));
            }
            return asked;
        }
        if let Some(quick) = &mut self.quick {
            return match quick.ui(ctx, grid, planner::PLAN) {
                Some(planner::QuickEnd::Run(edit)) => {
                    // The plan the dialog showed for exactly this edit is what runs.
                    let shown = match quick.planned() {
                        Some(Ok(plan)) if *quick.draft() == edit => Some(plan.clone()),
                        _ => None,
                    };
                    self.quick = None;
                    self.confirmed_edit = shown.map(|plan| (edit.clone(), plan));
                    vec![GitAction::RunEdit(edit)]
                }
                Some(planner::QuickEnd::Cancel) => {
                    self.quick = None;
                    Vec::new()
                }
                None => Vec::new(),
            };
        }
        if let Some(dialog) = &mut self.dialog {
            return match dialog.ui(ctx, grid, dialogs::PLAN) {
                Some(dialogs::DialogEnd::Run {
                    action,
                    dont_ask_again,
                }) => {
                    self.unasked |= dont_ask_again;
                    let shown = match dialog.planned() {
                        Some(Ok(plan)) if *dialog.draft() == action => Some(plan.clone()),
                        _ => None,
                    };
                    self.dialog = None;
                    self.confirmed = shown.map(|plan| (action.clone(), plan));
                    vec![GitAction::Run(action)]
                }
                Some(dialogs::DialogEnd::Cancel) => {
                    self.dialog = None;
                    Vec::new()
                }
                // Rebase's "plan the commits first" (03 §7.1): the planner instead, nothing run.
                Some(dialogs::DialogEnd::PlanFirst(onto)) => {
                    self.dialog = None;
                    vec![GitAction::PlanBeforeRebase(onto)]
                }
                None => Vec::new(),
            };
        }
        if let Some(result) = &mut self.result {
            // Closed only once the screen showed it ([`ResultScreen`]); the next one waiting comes
            // up, drawn from the next pass, saying how many before it were dropped
            // (F-git-outcome).
            let seen = self.result_screen.shown(ctx);
            if result.ui(ctx, grid) && seen {
                self.result = self.waiting.pop_front();
                self.result_screen.next();
                // The owner's own close, in the draw: each result keeps a serial of its own.
                self.bump(Front::Result);
                let dropped = std::mem::take(&mut self.results_dropped);
                if let Some(next) = &mut self.result {
                    next.say_dropped(dropped);
                }
            }
            return Vec::new();
        }
        if let Some((what, run)) = self.stop_question.clone() {
            return match stop_question(ctx, grid, &what) {
                Some(true) => {
                    self.stop_question = None;
                    vec![GitAction::StopWrite(run)]
                }
                Some(false) => {
                    self.stop_question = None;
                    Vec::new()
                }
                None => Vec::new(),
            };
        }
        Vec::new()
    }
}

// ---------------------------------------------------------------------------------------------
// S4: the history edits (03 §7.8; S4-C wires S4-B's screens to S4-A's plans).

/// The edit stop of `repo`'s banner (03 §8.5), its Split on HEAD's full id, the commit checked
/// out (`EditStop::probe`: git's own name for the stop is not HEAD once an earlier commit was
/// rewritten, and a Split of it was always refused; S4-C review, round 3).
fn edit_stop(repo: &Repo) -> Option<in_progress::EditStop> {
    in_progress::EditStop::probe(&repo.in_progress, &repo.status, repo.head.oid())
}

impl GitView {
    /// What the history row may offer on commit `oid` (`menu::Eligibility`), as the loaded
    /// graph says — false wherever it cannot tell (S4-B: "when in doubt, pass false"):
    /// - `ancestor`: `oid` is HEAD's commit or reached from it through the loaded rows' parents;
    /// - `linear`: HEAD's first-parent line reaches `oid` through loaded rows with no commit of
    ///   more than one parent, `oid` itself included — then `oid^..HEAD` is exactly that line;
    /// - `below_merge`: `oid`'s first parent has more than one parent, or is not loaded.
    pub fn eligibility(&self, oid: &Oid) -> menu::Eligibility {
        let mut eligible = menu::Eligibility::default();
        let Some(Row::Commit { commit: head, .. }) = self.head_row.and_then(|i| self.rows.get(i))
        else {
            return eligible;
        };
        let commits: HashMap<&Oid, &CommitRow> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Commit { commit, .. } => Some((&commit.oid, commit)),
                _ => None,
            })
            .collect();
        // HEAD's ancestors among the loaded rows.
        let mut seen: HashSet<&Oid> = HashSet::new();
        let mut next = vec![&head.oid];
        while let Some(at) = next.pop() {
            if !seen.insert(at) {
                continue;
            }
            if let Some(c) = commits.get(at) {
                next.extend(c.parents.iter());
            }
        }
        eligible.ancestor = seen.contains(oid) && commits.contains_key(oid);
        if !eligible.ancestor {
            return eligible;
        }
        // HEAD's first-parent line down to `oid`.
        let mut at = Some(head);
        while let Some(c) = at {
            if c.parents.len() > 1 {
                break;
            }
            if c.oid == *oid {
                eligible.linear = true;
                break;
            }
            at = c.parents.first().and_then(|p| commits.get(p).copied());
        }
        let below = commits[oid].parents.first();
        eligible.below_merge =
            below.is_some_and(|p| commits.get(p).is_none_or(|c| c.parents.len() > 1));
        eligible
    }

    /// The history row of the commit `commit` as the menu would offer it (03 §7.8.1).
    fn history_of(&self, commit: &CommitRow) -> Vec<EditId> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        menu::history_items(commit, self.eligibility(&commit.oid), repo)
            .into_iter()
            .map(|i| i.id)
            .collect()
    }

    /// F2 and F8 on a commit (03 §9.1): its `reword` and `drop`, where the history row offers
    /// them; `None` otherwise.
    fn edit_slot(&self, command: Command, object: &Target) -> Option<(EditId, CommitRow)> {
        let Target::Commit { commit, .. } = object else {
            return None;
        };
        let id = match command {
            Command::Reword => EditId::Reword,
            Command::Remove => EditId::Drop,
            _ => return None,
        };
        self.history_of(commit)
            .contains(&id)
            .then(|| (id, commit.clone()))
    }

    /// Why history edit `id` is not offered on `commit`, for the status row.
    fn not_offered(&self, id: EditId, commit: &CommitRow) -> String {
        let what = menu::history_label(id).trim_end_matches('…');
        let short = commit.oid.short();
        let eligible = self.eligibility(&commit.oid);
        let why = if self.repo.as_ref().and_then(|r| r.head.oid()).is_none() {
            "the branch has no commit yet"
        } else if !eligible.ancestor {
            "it is not HEAD or an ancestor of HEAD in the loaded graph"
        } else if !eligible.linear {
            "a merge is between it and HEAD (Edit history from here… names it)"
        } else {
            "not on this commit"
        };
        format!("{what}: not offered on {short} — {why}")
    }

    /// History edit `id` on `commit` (the menu's history row, F2, F8, Cmd/Ctrl+E): the planner's
    /// range, or the range a quick edit's dialog reads its commit from — above `commit`'s parent,
    /// or for `squash` its grandparent (it shows the older message too), or from the root where
    /// there is none.
    fn act_edit(&mut self, id: EditId, commit: CommitRow) -> Vec<GitAction> {
        if id == EditId::Plan {
            return vec![GitAction::OpenPlanner(Base::Commit(commit.oid))];
        }
        let base = match (id, commit.parents.first()) {
            (EditId::Squash, Some(parent)) => {
                let parent_row = self.rows.iter().find_map(|r| match r {
                    Row::Commit { commit, .. } if commit.oid == *parent => Some(commit),
                    _ => None,
                });
                match parent_row {
                    Some(p) => p.parents.first().cloned().map_or(Base::Root, Base::Commit),
                    None => {
                        self.said = Some(Status::info(self.not_offered(id, &commit)));
                        return Vec::new();
                    }
                }
            }
            (_, Some(parent)) => Base::Commit(parent.clone()),
            (_, None) => Base::Root,
        };
        vec![GitAction::OpenEdit {
            id,
            commit: commit.oid,
            base,
        }]
    }

    /// Cmd/Ctrl+E (03 §9.2): the planner from the bar's commit, where its history row offers it
    /// (not on HEAD: nothing is above it).
    fn plan_from_bar(&mut self) -> Vec<GitAction> {
        let Some(Target::Commit { commit, .. }) = self.object() else {
            if self.repo.is_some() {
                self.said = Some(Status::info(
                    "Edit history from here: the bar is not on a commit",
                ));
            }
            return Vec::new();
        };
        if self.history_of(&commit).contains(&EditId::Plan) {
            return self.act_edit(EditId::Plan, commit);
        }
        let said = match self.repo.as_ref().and_then(|r| r.head.oid()) {
            Some(head) if *head == commit.oid => {
                "Edit history from here: nothing is above HEAD's commit to plan".to_string()
            }
            _ => self.not_offered(EditId::Plan, &commit),
        };
        self.said = Some(Status::info(said));
        Vec::new()
    }

    /// `SOPEN_STATE dialog=<edit id>`: on the bar's commit, else on HEAD's.
    fn open_edit_by_id(&mut self, id: EditId) -> Vec<GitAction> {
        self.said = None;
        let mut candidates: Vec<CommitRow> = Vec::new();
        if let Some(Target::Commit { commit, .. }) = self.object() {
            candidates.push(commit);
        }
        if let Some(Row::Commit { commit, .. }) = self.head_row.and_then(|i| self.rows.get(i)) {
            candidates.push(commit.clone());
        }
        match candidates
            .into_iter()
            .find(|c| self.history_of(c).contains(&id))
        {
            Some(commit) => self.act_edit(id, commit),
            None => {
                self.said = Some(Status::error(format!(
                    "SOPEN_STATE dialog={}: not offered on the row under the bar",
                    id.as_str()
                )));
                Vec::new()
            }
        }
    }

    /// A quick edit's dialog (03 §7.8.1) on `commit`, from `model`, the range its reads opened:
    /// its row and, for `squash`, the row below it. A commit the range does not hold is said.
    pub fn open_edit(&mut self, id: EditId, commit: &Oid, model: &Model) -> Vec<GitAction> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        let at = model.rows.iter().position(|r| r.commit.row.oid == *commit);
        let Some(at) = at else {
            let what = menu::history_label(id).trim_end_matches('…');
            self.said = Some(Status::error(format!(
                "{what}: {} is not in the range read; refresh (Cmd/Ctrl+R) and try again",
                commit.short()
            )));
            return Vec::new();
        };
        let this = &model.rows[at].commit;
        let below = model.rows.get(at + 1).map(|r| &r.commit);
        match planner::QuickDialog::open(id, this, below, repo, planner::PLAN) {
            planner::QuickOpened::Dialog(dialog) => {
                self.quick = Some(dialog);
                self.bump(Front::Quick);
                Vec::new()
            }
            planner::QuickOpened::Run(edit) => vec![GitAction::RunEdit(edit)],
            planner::QuickOpened::Planner(base) => vec![GitAction::OpenPlanner(base)],
        }
    }

    /// The planner (03 §7.8.2) on `model`, the range read, over the body; it has the keyboard.
    pub fn open_planner(&mut self, model: Model) {
        let Some(repo) = &self.repo else {
            return;
        };
        self.find = None;
        self.planner = Some(planner::PlannerView::new(model, repo));
        self.bump(Front::Planner);
    }

    /// The open action dialog's "Plan the commits first…" button, when it draws one (tests).
    #[cfg(test)]
    pub fn plan_first_button(&self) -> Option<egui::Id> {
        self.dialog.as_ref()?.plan_first_button()
    }

    /// The planner's model as its keys have made it, while it is open: what the switch's
    /// question asks about (`Browser::git_dropped`), and the tests'.
    pub fn planner_model(&self) -> Option<&Model> {
        self.planner.as_ref().map(|p| p.model())
    }

    /// The planner's note (an op's refusal, a key's hint), while it is open (tests).
    #[cfg_attr(not(test), allow(dead_code))] // Only the tests use it.
    pub fn planner_note(&self) -> Option<&str> {
        self.planner.as_ref().and_then(|p| p.note())
    }

    /// How the planner ended this pass: closed, or run — the plan its run dialog showed is what
    /// runs, and the pane closes (the history it planned is rewritten).
    fn planner_end(&mut self, end: Option<planner::PlannerEnd>) -> Vec<GitAction> {
        match end {
            None => Vec::new(),
            Some(planner::PlannerEnd::Close) => {
                self.planner = None;
                self.bump(Front::Planner);
                Vec::new()
            }
            Some(planner::PlannerEnd::Run(edit)) => {
                let shown = self.planner.as_mut().and_then(|p| p.take_shown());
                self.planner = None;
                self.bump(Front::Planner);
                self.confirmed_edit = shown.filter(|(e, _)| *e == edit);
                vec![GitAction::RunEdit(edit)]
            }
        }
    }

    /// An edit built elsewhere (an edit stop's Split, a kept split's Continue), through its
    /// confirmation (`QuickDialog::confirm`).
    fn confirm_edit(&mut self, edit: Edit) -> Vec<GitAction> {
        let Some(repo) = &self.repo else {
            return Vec::new();
        };
        match planner::QuickDialog::confirm(edit, repo, planner::PLAN) {
            planner::QuickOpened::Dialog(dialog) => {
                self.quick = Some(dialog);
                self.bump(Front::Quick);
                Vec::new()
            }
            planner::QuickOpened::Run(edit) => vec![GitAction::RunEdit(edit)],
            planner::QuickOpened::Planner(base) => vec![GitAction::OpenPlanner(base)],
        }
    }

    /// The Split of the edit stop the banner shows, through its confirmation: what the
    /// integration opens at the stop a quick `split` leaves (03 §7.8.1: "at the stop sopen
    /// records the commit's paths and runs `reset --mixed`" — shown first, as every plan is).
    /// `None` when the banner shows no edit stop with its commit in full.
    pub fn split_at_the_stop(&mut self) -> Option<Vec<GitAction>> {
        let commit = self.stop.as_ref().and_then(|s| s.commit()).cloned()?;
        Some(self.confirm_edit(Edit::SplitStop { commit }))
    }

    /// A history edit's confirmation again, as a dialog: its run's shown plan was lost and the
    /// plan made now asks for a confirmation (S3-C's review rule). Never a run from here.
    pub fn reconfirm_edit(&mut self, edit: Edit) {
        let Some(repo) = &self.repo else {
            return;
        };
        match planner::QuickDialog::confirm(edit, repo, planner::PLAN) {
            planner::QuickOpened::Dialog(dialog) => {
                self.quick = Some(dialog);
                self.bump(Front::Quick);
            }
            planner::QuickOpened::Run(_) | planner::QuickOpened::Planner(_) => {
                self.said = Some(Status::info("the history edit: run it again"));
            }
        }
    }

    /// The plan the confirmation showed for `edit` (taken once), when it showed one.
    pub fn take_edit_plan(&mut self, edit: &Edit) -> Option<Plan> {
        let (shown, plan) = self.confirmed_edit.take()?;
        (shown == *edit).then_some(plan)
    }

    /// The history edit an open dialog plans now, what `rebase::needs` reads for: the quick
    /// edit's draft, or the planner's run's while its dialog is up — the model with the run's
    /// options (03 §7.8.2: the run's reads on Cmd/Ctrl+Enter). Borrowed, as [`planner::Draft`]:
    /// a copy of it counts in a test build (F-git-drafted).
    pub fn held_edit(&self) -> Option<&planner::Draft> {
        if let Some(quick) = &self.quick {
            return Some(quick.held_draft());
        }
        self.planner.as_ref().and_then(|p| p.run_draft())
    }

    /// [`GitView::held_edit`] as the edit itself (the tests).
    #[cfg(test)]
    pub fn edit_draft(&self) -> Option<&Edit> {
        self.held_edit().map(|d| &**d)
    }

    /// The key of the draft [`GitView::draft`] or else [`GitView::held_edit`] gives, in their
    /// order — S3's action dialog first, then the quick edit's, then the planner's run: which
    /// dialog and its draft's revision (F-git-drafted). `None` with none of them up.
    pub fn draft_key(&self) -> Option<DraftKey> {
        let key = |(serial, revision)| DraftKey { serial, revision };
        if let Some(dialog) = &self.dialog {
            return Some(key(dialog.draft_revision()));
        }
        if let Some(quick) = &self.quick {
            return Some(key(quick.draft_revision()));
        }
        (self.planner.as_ref()).and_then(|p| p.run_revision().map(key))
    }

    /// The split this window keeps for this repository (03 §7.8.1 `split`), from the refresh.
    pub fn set_split(&mut self, split: Option<rebase::Split>) {
        self.split = split;
    }

    /// Cmd/Ctrl+Shift+M or P (03 §9.2): an edit stop's Amend or Split; with no edit stop they do
    /// nothing, and the status row says so.
    fn stop_step(&mut self, step: in_progress::StopStep) -> Vec<GitAction> {
        let pressed = match &self.banner {
            Some(banner) => in_progress::stop_step(banner, self.stop.as_ref(), step),
            None => {
                if self.repo.is_some() {
                    let word = in_progress::stop_word(step);
                    self.said = Some(Status::info(format!("{word}: nothing is in progress")));
                }
                return Vec::new();
            }
        };
        self.stop_pressed(pressed)
    }

    /// An edit stop's Amend (§7.6's `amend` dialog: keep the stopped commit's message or edit
    /// it, prefilled) or Split (its confirmation).
    fn stop_pressed(&mut self, pressed: in_progress::StopPressed) -> Vec<GitAction> {
        match pressed {
            in_progress::StopPressed::Amend => self.act(ActionId::Amend, Target::Uncommitted),
            in_progress::StopPressed::Split(edit) => self.confirm_edit(edit),
            in_progress::StopPressed::NotOffered(text) => {
                self.said = Some(Status::info(text));
                Vec::new()
            }
        }
    }
}

/// "Stop git?" (03 §3.4) over the write `what`, on `grid` (the panels'), in cells
/// ([`dialogs::stop_question`]): `Some(true)` on Enter or Yes, `Some(false)` on Esc, No or a click
/// outside.
fn stop_question(ctx: &egui::Context, grid: Grid, what: &str) -> Option<bool> {
    dialogs::stop_question(ctx, grid, what)
}

/// The cells of `grid` (the panels') "Stop git?" over `what` covers, its box and its shadow, for
/// the panel and the other side to blank (10 §17, X4-graph's seam).
fn stop_covers(grid: &Grid, what: &str) -> Vec<Cells> {
    dialogs::stop_covers(grid, what)
}

/// The key table's rows the panel reads: the landed slices' (S2's own among them since S2-F);
/// later slices' rows (actions, the planner) only once they land.
const UPTO: keys::Slices = keys::LANDED_SLICES;

/// The git context's rows that act while the planner is open: the window's own (F1, F9, Tab,
/// Cmd/Ctrl+R, G, Shift+G), none of which is one of the pane's keys.
fn window_rows() -> impl Iterator<Item = &'static keys::Row> {
    keys::rows(Context::Git, UPTO).filter(|r| {
        matches!(
            r.command,
            Command::Help
                | Command::SwitchMode
                | Command::OtherSide
                | Command::Reread
                | Command::Pin
                | Command::Wide
        )
    })
}

/// What the status row says while nothing else is to be said.
const HINT: &str =
    "Enter details · Space mark · a letter finds · ← → labels · Tab the list · Esc back · F1 the keys";

/// What the status row says while the planner is open (its own keys are at its foot).
const PLANNER_HINT: &str =
    "The planner: its keys are at its foot · F1 the panel's keys · Tab the list · Esc closes it";

/// What the status row says while the menu is open: what it is the menu of, and its keys (a
/// commit's history row counted).
fn menu_hint(history: &menu::HistoryMenu) -> String {
    let menu = history.menu();
    let of = match menu.target() {
        Target::Commit { commit, marked } if marked.len() > 1 => {
            format!("{} and {} marked commits", commit.oid.short(), marked.len())
        }
        Target::Commit { commit, .. } => format!("commit {}", commit.oid.short()),
        Target::Label(r) => r.name().to_string(),
        Target::Stash(s) => s.selector.clone(),
        Target::Uncommitted => "the uncommitted changes".to_string(),
        Target::Files(files) => match &files[..] {
            [one] => one.path.clone(),
            many => format!("{} files", many.len()),
        },
        Target::Repository => "the repository".to_string(),
    };
    match history.lines() {
        0 => format!("Menu of {of}: nothing to do · Esc closes"),
        n => format!("Menu of {of}: {n} items · ↑ ↓ choose · Enter runs · Esc closes"),
    }
}

/// The git panel's keys this frame, through the key table's git context (04 §3: exact
/// modifiers) — while Find's field has the typing, only Find's Enter, Shift+Enter or Esc that
/// comes before anything typed (`find::take_leading`; the rest are the field's and [`find_ui`]'s),
/// nothing while a dialog is up (it takes its own) — in order: each carried out on the view, what
/// is left for the integration returned.
pub fn keys(ctx: &egui::Context, view: &mut GitView) -> Vec<GitAction> {
    let mut asked = Vec::new();
    while !view.dialog_up() {
        if view.planner.is_some() {
            // The planner takes its keys first (in [`draw`]): here only the window's rows, each
            // by its exact key — never the git context's lookup, whose rows (↑ ↓, Enter, Esc, a
            // typed letter starting Find) are the pane's (03 §7.8.2, S4-0 keys §C).
            let Some(command) = ctx.input_mut(|i| {
                window_rows().find_map(|r| match r.trigger {
                    Trigger::Key(key, mods) if keys::take(i, key, mods) => Some(r.command),
                    _ => None,
                })
            }) else {
                break;
            };
            view.said = None;
            asked.extend(view.run(ctx, command));
            continue;
        }
        if view.search.is_some() {
            // Find is over the graph's rows: never left open under a history search's results.
            view.find = None;
        }
        if view.find.is_some() {
            // Find's field has the typing: Enter, Shift+Enter and Esc only (04 §3 rule 4), each
            // in the pass's order with what is typed into it (X4-find): one before anything typed
            // acts here, an Esc's rest going on to the graph; the others wait for the field to
            // type what came before them (`graph_ui`).
            match ctx.input_mut(|i| find::take_leading(&mut i.events)) {
                Some(find::FindKey::Next) => asked.extend(view.find_step(true)),
                Some(find::FindKey::Previous) => asked.extend(view.find_step(false)),
                Some(find::FindKey::Close) => asked.extend(view.find_close()),
                None => break,
            }
            continue;
        }
        // The banner's keys while it shows (03 §9.2: Cmd/Ctrl+Shift+C, S, A, B), before the
        // table's rows of the same keys (which say "nothing is in progress" without it).
        let pressed = view.banner.as_ref().and_then(|b| in_progress::keys(ctx, b));
        if let Some(pressed) = pressed {
            view.said = None;
            asked.extend(view.pressed(pressed));
            continue;
        }
        let Some(command) = ctx.input_mut(|i| keys::find(&mut i.events, Context::Git, UPTO)) else {
            break;
        };
        view.said = None;
        asked.extend(view.run(ctx, command));
    }
    asked
}

/// Where a pass lays the panel's parts, in cells of the panels' grid (03 §4.2, §6.1): inside the
/// frame, the banner's rows at the top, the heads' row (or Find's field), the body — the graph's
/// rows, then the rule and the details pane when it is open, or the log, the Backups list or the
/// planner over the heads' row and the body — the rule above the foot, and the foot. A function
/// of the frame's inside and of what is open only (10 §4): no font is measured.
struct Layout {
    banner: Cells,
    head: Cells,
    /// The graph's rows (or a history search's).
    rows: Cells,
    /// The rule between the graph and the pane, and the pane's rows.
    pane: Option<(u16, Cells)>,
    /// The log's, the Backups list's or the planner's cells: the heads' row and the body.
    listing: Cells,
    rule: u16,
    foot: u16,
}

impl Layout {
    /// The parts inside `inner` with `banner` rows of the banner, the pane open (`pane`) or not:
    /// `None` where the frame has no room for the heads' row, the rule and the foot.
    fn of(inner: Cells, banner: usize, pane: bool) -> Option<Layout> {
        let banner = u16::try_from(banner).unwrap_or(u16::MAX).min(inner.height);
        if inner.width == 0 || inner.height < banner.saturating_add(3) {
            return None;
        }
        let row = |y: u16, height: u16| Cells::new(inner.x, y, inner.width, height);
        let head_y = inner.y + banner;
        let foot = inner.bottom() - 1;
        let rule = foot - 1;
        let body = row(head_y + 1, rule - (head_y + 1));
        let total = body.height;
        // The pane below the graph, the graph keeping at least 8 rows (03 §6.1), a rule between.
        let (rows, pane) = if pane && total > 10 {
            let graph = (total / 2).max(8).min(total - 2);
            let pane_y = body.y + graph + 1;
            (
                row(body.y, graph),
                Some((body.y + graph, row(pane_y, rule - pane_y))),
            )
        } else {
            (body, None)
        };
        Some(Layout {
            banner: row(inner.y, banner),
            head: row(head_y, 1),
            rows,
            pane,
            listing: row(head_y, rule - head_y),
            rule,
            foot,
        })
    }
}

/// `c` and the border cell right of it on each of its rows, where the details pane egui draws
/// puts its scroll bar (`details::draw`, while egui draws it; the planner, the log and the Backups
/// list, in cells, put theirs on the border as the graph does).
fn with_border(c: Cells) -> Cells {
    Cells::new(c.x, c.y, c.width.saturating_add(1), c.height)
}

/// Cells `r` of `buf` given to what egui draws there: no glyph and no colour, so the window
/// paints nothing over it and `term::compose` makes a hand-off box of them (10 §17, X4-graph).
fn leave_to_egui(buf: &mut Buffer, r: Cells) {
    let r = r.intersection(buf.area);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].reset();
        }
    }
}

/// `covers` blanked in `buf`, cells of a side `painter` has painted this pass (the panel's, or
/// one the browser kept), and what that changed painted over that painting (F-menu-own): the
/// side's body first under each cell that changed and under the rest of a wide glyph it held, so
/// no half of a glyph painted before shows, then those cells alone, in a buffer over just them —
/// every other cell of it empty, which leaves what is under it (`cells::paint`, `term::compose`),
/// and no finder takes it for the side's. Only changed cells are painted again, which is what
/// leaves a viewer's cells inside a preview's frame alone: the frame's own cells under the viewer
/// are blanks in the body colour (`cells::frame` fills its side), so blanking changes none of them
/// and nothing is painted over the viewer's layer. The rule that skips a cell `buf` left without
/// a colour of its own guards only such cells — those left to egui (`leave_to_egui`).
fn blank_over(painter: &egui::Painter, grid: &Grid, buf: &mut Buffer, covers: &[Cells], body: Pen) {
    let before = buf.clone();
    for r in covers {
        cells::fill(buf, *r, body);
    }
    let a = buf.area;
    let mut runs: Vec<Cells> = Vec::new();
    for y in a.top()..a.bottom() {
        let mut x = a.left();
        while x < a.right() {
            let own = matches!(before[(x, y)].bg, ratatui_core::style::Color::Rgb(..));
            if !own || before[(x, y)] == buf[(x, y)] {
                x += 1;
                continue;
            }
            let wide = cells::width(before[(x, y)].symbol()).max(1) as u16;
            let end = (x + wide).min(a.right());
            match runs.last_mut() {
                Some(run) if run.y == y && run.right() >= x => run.width = end - run.x,
                _ => runs.push(Cells::new(x, y, end - x, 1)),
            }
            x = end;
        }
    }
    let Some(first) = runs.first().copied() else {
        return;
    };
    let mut patch = Buffer::empty(runs.iter().fold(first, |all, r| all.union(*r)));
    for r in &runs {
        painter.rect_filled(grid.rect(*r), 0.0, body.bg);
        for x in r.left()..r.right() {
            patch[(x, r.y)] = buf[(x, r.y)].clone();
        }
    }
    cells::paint(painter, grid.origin, &patch, None);
}

/// `covers` blanked on the other side of `grid` than `side`, the panel's, where that side was
/// painted before the panel this pass (F-menu-own): it read the panel's covers then
/// (`Browser::git_covered`), before a box the panel's draw opened, and the browser kept what it
/// painted (`cells::keep`) — its layer of cells over its whole side, on `painter`'s egui layer —
/// which [`blank_over`] blanks as the panel's own. A side painted after the panel reads the
/// covers with that box in them.
fn blank_beside(painter: &egui::Painter, grid: &Grid, side: Cells, covers: &[Cells], body: Pen) {
    let beside: Vec<Cells> = (cells::halves(grid.all()).into_iter())
        .filter(|h| *h != side)
        .collect();
    let ctx = painter.ctx();
    for l in cells::kept(ctx) {
        let theirs = beside.contains(&l.buf.area) && l.origin == grid.origin;
        if theirs && l.layer == painter.layer_id() {
            let on = egui::Painter::new(ctx.clone(), l.layer, l.clip);
            blank_over(&on, grid, &mut l.buf.clone(), covers, body);
        }
    }
}

/// The git panel at cells `side` of `grid` (the panels' grid, between the title and status
/// rows), in cells (10 §4, X4-graph): its frame titled with the top level, the find row or the
/// column heads, the visible rows with their lanes, nodes and labels, the bar, the details pane
/// below them when it is open (the graph keeping at least 8 rows), the scroll bars, the rule and
/// the paging line; and the dialogs, on `grid`. Each part draws at its cells ([`Layout`]); one
/// egui still draws ([`details::PAINTS_CELLS`] and the other parts' answers false) has its cells
/// left to `term::compose` — a hand-off box inside the frame in a terminal — and a dialog egui
/// draws is a hand-off box where it stands. The cells `under` a dialog of the list's, those under
/// the panel's own dialogs ([`GitView::covered`]) and under the key help are blanked first. A
/// click puts the bar on a row, a double click is Enter there, a right click opens the row's
/// menu, the wheel scrolls and a click on the paging line reads more, all by cells.
pub fn draw(
    ui: &mut egui::Ui,
    grid: &Grid,
    side: Cells,
    view: &mut GitView,
    active: bool,
    under: &[Cells],
) -> Vec<GitAction> {
    let mut asked = Vec::new();
    let ctx = ui.ctx().clone();
    // A menu opened in the body this pass is drawn over a side painted before it (F-menu-own).
    let menu_before = view.menu.is_some();
    // While another dialog has the keyboard — another panel's, the list's question, the tree —
    // no dialog of this panel's reads a key or text this pass (X4-menu fix-check): those are that
    // dialog's. Taken before the body, which draws the planner's own dialogs and the log's entry
    // shown whole (F-git-listq), and put back after the panel's other dialogs.
    let kept = view.dialog_elsewhere.then(|| take_keys(&ctx));
    // While one of `modals`' dialogs is up (the menu, a quick edit's, an action's, the result,
    // "Stop git?") it has the keys and the keyboard, drawn last over the rest — one the
    // integration opened over the log's entry shown whole, the filters or the history search
    // (F-git-covered review 1). Nothing drawn before it reads the pass's keys: they are taken here
    // and put back just before `modals`, as `kept` keeps them for another's dialog. (The body's
    // other readers act on nothing while a dialog is up: the planner's keys, the log's rows, the
    // details pane, the banner's keys, Find's field.)
    let under_modal = view.modal_up();
    let held = (under_modal && kept.is_none()).then(|| take_keys(&ctx));
    // A screen of the panel's counts what it paints as shown only while it has the keyboard
    // (F-git-covered): the draw says before each part whether another dialog has it — another
    // panel's, the list's question, the tree, F12's — or one of the panel's own drawn over the
    // part, which then has its keys: the planner's dialogs and the Backups list in the body yield
    // them to any other, the filters and the history search to `modals`' (below). Said here, by
    // `cells::set_covered`, rather than passed to each guard: one rule for every `OnScreen` of the
    // panel, a new one's included.
    let elsewhere = view.dialog_elsewhere;
    cells::set_covered(&ctx, elsewhere || view.other_dialog_up());
    let p = chrome::PALETTE;
    let look = chrome::git::LOOK;
    let title = if view.filters.is_default() {
        view.title.clone()
    } else {
        format!("{} [filtered]", view.title)
    };
    let rect = grid.rect(side);
    let painter = ui.painter_at(rect);
    // The side's body under its cells, and under a part egui still draws.
    painter.rect_filled(rect, 0.0, p.back);
    let mut buf = Buffer::empty(side);
    let inner = cells::frame(&mut buf, side, &title, active);
    // The log, the Backups list or the planner in the body, in place of the graph (03 §3.7,
    // §8.2, §7.8.2).
    let listing = view.log.is_some() || view.backups.is_some() || view.planner.is_some();
    let banner_rows = view.banner.as_ref().map_or(0, |b| b.rows());
    let layout = Layout::of(inner, banner_rows, view.pane.is_some() && !listing);
    let pane_keyboard = view.pane.as_ref().is_some_and(|p| p.keyboard);
    let list_active = active && !pane_keyboard && !view.dialog_up();
    // A dialog of this panel's, or of another panel's beside it: no click or right click here
    // opens one or acts (one dialog at a time, X4-menu round 1).
    let dialog = view.dialog_up() || view.dialog_elsewhere;
    // The cells egui draws (the parts not in cells yet), and Find's caret.
    let mut eguis: Vec<Cells> = Vec::new();
    let mut caret = None;
    view.geometry = None;
    if let Some(at) = &layout {
        // The banner; at an edit stop with Amend and Split before its steps (03 §8.5, S4).
        let panel = view.find_key;
        let ended = match (&view.banner, &view.stop) {
            (Some(b), Some(stop)) => {
                in_progress::draw_stop(ui, grid, at.banner, &mut buf, b, stop, panel)
            }
            (Some(b), None) => in_progress::draw(ui, grid, at.banner, &mut buf, b, panel)
                .map(in_progress::StopEnd::Banner),
            (None, _) => None,
        };
        if view.banner.is_some() && !in_progress::PAINTS_CELLS {
            eguis.push(at.banner);
        }
        match ended.filter(|_| !dialog) {
            Some(in_progress::StopEnd::Banner(pressed)) => {
                view.said = None;
                asked.extend(view.pressed(pressed));
            }
            Some(in_progress::StopEnd::Stop(pressed)) => {
                view.said = None;
                asked.extend(view.stop_pressed(pressed));
            }
            None => {}
        }
        // What covers the Backups list this pass — a dialog of the list's or the panel's own, the
        // key help — blanked over it below: its bar's line under one is not on screen (X4-lists).
        let hidden: Vec<Cells> = match view.backups.is_some() {
            true => (under.iter().copied())
                .chain(view.covered(grid))
                .chain(help::covered(&ctx, grid))
                .collect(),
            false => Vec::new(),
        };
        if view.planner.is_some() {
            // The planner over the body (03 §7.8.2): its keys first while it has the keyboard and
            // no other dialog is up, then the pane, then its own dialog.
            let pane_active = active && !view.other_dialog_up() && !view.dialog_elsewhere;
            let end = view.planner.as_mut().and_then(|p| {
                planner::draw(
                    ui,
                    grid,
                    at.listing,
                    &mut buf,
                    p,
                    pane_active,
                    &planner::SEAMS,
                )
            });
            if !planner::PAINTS_CELLS {
                eguis.push(at.listing);
            }
            asked.extend(view.planner_end(end));
        } else if let Some(log) = &mut view.log {
            clicks_off(ui, dialog, |ui| {
                log_view::draw(ui, grid, at.listing, &mut buf, log, active && !dialog)
            });
            if !log_view::PAINTS_CELLS {
                eguis.push(at.listing);
            }
        } else if let Some(list) = &mut view.backups {
            let shown = active && !dialog;
            let end = backups::draw(ui, grid, at.listing, &mut buf, list, shown, &hidden);
            if !backups::PAINTS_CELLS {
                eguis.push(at.listing);
            }
            let end = end.filter(|_| !dialog);
            asked.extend(view.backups_end(end));
        } else {
            asked.extend(graph_ui(
                ui,
                grid,
                &mut buf,
                at,
                view,
                (list_active, active && !dialog),
                dialog,
                &mut eguis,
                &mut caret,
            ));
            if let (Some((rule, pane_cells)), Some(pane)) = (at.pane, view.pane.as_mut()) {
                cells::rule(&mut buf, side, rule, active);
                let subject = pane.subject.clone();
                let viewed = &view.viewed;
                let seen = |f: &FileRow| viewed.contains(&(subject.clone(), f.path.clone()));
                let pane_active = active && pane.keyboard && !dialog;
                let clicked =
                    details::draw(ui, grid, pane_cells, &mut buf, pane, &seen, pane_active);
                if !details::PAINTS_CELLS {
                    eguis.push(with_border(pane_cells));
                }
                if let Some((line, twice)) = clicked.filter(|_| !dialog) {
                    pane.keyboard = true;
                    pane.scroll.to(line);
                    if twice {
                        asked.extend(view.run_pane(Command::Details));
                    }
                }
            }
            asked.extend(view.ask_more());
        }
        // The paging line: the rule across the frame, then what the stream says (Find's `k of n`
        // while it is open) and the branch's ahead / behind at the right when it fits.
        cells::rule(&mut buf, side, at.rule, active);
        let (n, noun, paging) = match &view.search {
            Some(s) => (s.rows.len(), "found", &s.paging),
            None => (view.commits, "commits", &view.paging),
        };
        let (mut foot, error) = graph_view::paging_text(paging, n, noun);
        if let Some(find) = view.find.as_ref().filter(|f| !f.text.is_empty()) {
            foot = find.foot(view.graph_scroll.bar);
        }
        let colour = if error { look.error } else { look.foot };
        let room = usize::from(inner.width);
        let shown = cells::fit_start(&foot, room);
        let used = cells::put(
            &mut buf,
            inner.x,
            at.foot,
            &shown,
            room,
            Pen::new(colour, p.back),
        );
        if let Some(t) = &view.tracking {
            let right = graph_view::tracking_text(t, cells::ambiguous_wide(&ctx));
            if used + 2 + cells::width(&right) <= room {
                let pen = Pen::new(look.foot, p.back);
                cells::put_right(&mut buf, inner.right(), at.foot, &right, pen);
            }
        }
        // A click on the paging line reads more (03 §4.5; the bar never stands on it, ↓ on the
        // last row reads more from the keyboard): clicks alone, its id the panel's own.
        let foot_cells = Cells::new(inner.x, at.foot, inner.width, 1);
        let foot_click = ui.interact(
            grid.rect(foot_cells),
            view.find_key.with("sopen-git-foot"),
            egui::Sense::CLICK,
        );
        if foot_click.clicked() && view.search.is_none() && !dialog {
            let more = matches!(view.paging, Paging::More(_) | Paging::Error(_));
            if more && (!view.more_asked || matches!(view.paging, Paging::Error(_))) {
                view.more_asked = true;
                asked.push(GitAction::MoreRows);
            }
        }
    } else {
        // No room for the parts: those that hold the keyboard still run, at no cells — the
        // planner's keys and its dialogs, the log's entry shown whole, Find's field and its bar —
        // so no key or typing goes nowhere (review round 1); the Backups list reads no key of
        // its own (`keys` → `run_list`) and is not drawn. They run in a child clipped to no
        // cells, so nothing of them is painted or takes a click outside the panel's (round 2):
        // the keyboard is by id, and the dialogs are areas of their own.
        let none = Cells::new(inner.x, inner.y, 0, 0);
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(grid.rect(none)));
        ui.set_clip_rect(grid.rect(none));
        let ui = &mut ui;
        if view.planner.is_some() {
            let pane_active = active && !view.other_dialog_up() && !view.dialog_elsewhere;
            let end = view.planner.as_mut().and_then(|p| {
                planner::draw(ui, grid, none, &mut buf, p, pane_active, &planner::SEAMS)
            });
            asked.extend(view.planner_end(end));
        } else if let Some(log) = &mut view.log {
            clicks_off(ui, dialog, |ui| {
                log_view::draw(ui, grid, none, &mut buf, log, active && !dialog)
            });
        } else if view.backups.is_some() {
        } else if view.find.is_some() && view.search.is_none() {
            let typing = active && !dialog;
            let modes = (typing, dialog);
            asked.extend(find_ui(ui, grid, none, &mut buf, view, modes, &mut caret).0);
        }
    }
    for r in eguis {
        leave_to_egui(&mut buf, r);
    }
    // A dialog — the list's, the panel's own — or the key help over the panel: the cells under it
    // blank first (cells.rs, one buffer over another), so a wide glyph half under its edge goes
    // as a terminal erases it.
    let body = Pen::new(p.text, p.back);
    let own = view.covered(grid);
    for r in under
        .iter()
        .chain(&own)
        .copied()
        .chain(help::covered(&ctx, grid))
    {
        cells::fill(&mut buf, r, body);
    }
    cells::paint(&painter, grid.origin, &buf, caret);
    // A menu the body opened this pass (a right click on a row, F10 after Find's Esc): `own` held
    // it, the other side's cells under it are blanked from what the browser kept.
    if let Some(menu) = view.menu.as_ref().filter(|_| !menu_before) {
        blank_beside(&painter, grid, side, &menu.covers(grid), body);
    }
    // The dialogs, over everything, on the panels' grid (with no key while `kept` or `held` holds
    // them). The filters and the history search under another's dialog or one of `modals`' have
    // neither the keyboard — they take none for their field, which would leave the dialog in
    // front without it — nor a painting the screen showed.
    let keyboard = !(elsewhere || under_modal);
    cells::set_covered(&ctx, !keyboard);
    if let Some(dialog) = &mut view.filters_dialog {
        match dialog.ui(&ctx, *grid, keyboard) {
            Some(filters::DialogEnd::Apply(f)) => {
                view.filters_dialog = None;
                if f != view.filters {
                    view.filters = f.clone();
                    view.relabel();
                    asked.push(GitAction::FiltersChanged(f));
                }
            }
            Some(filters::DialogEnd::Cancel) => view.filters_dialog = None,
            None => {}
        }
    }
    if let Some(dialog) = &mut view.search_dialog {
        match dialog.ui(&ctx, *grid, keyboard) {
            Some(find::SearchEnd::Run(query)) => {
                view.search_dialog = None;
                view.find = None;
                view.search = Some(find::Results::new(query.clone()));
                asked.push(GitAction::Search(query));
            }
            Some(find::SearchEnd::Cancel) => view.search_dialog = None,
            None => {}
        }
    }
    if let Some(held) = held {
        ctx.input_mut(|i| i.events.extend(held));
    }
    cells::set_covered(&ctx, elsewhere);
    // Under a dialog a menu choice opens in `modals`: the panel's own cells, and the other side's
    // ([`blank_beside`]).
    let mut blank = |covers: &[Cells]| {
        blank_over(&painter, grid, &mut buf, covers, body);
        blank_beside(&painter, grid, side, covers, body);
    };
    asked.extend(view.modals(&ctx, *grid, &mut blank));
    // Not a value from before `draw` restored: nothing outside `draw` sets the flag, so each pass
    // of the panel's starts from `false` (an `OnScreen` asked outside it, as the Backups list's
    // keys ask, reads `false`).
    cells::set_covered(&ctx, false);
    if let Some(kept) = kept {
        ctx.input_mut(|i| i.events.extend(kept));
    }
    asked
}

/// `f` in `ui`, its clicks off while `off` — a dialog up, this panel's or another panel's beside
/// it: the command log's rows act on nothing then, not only where egui's modal lies over them
/// (X4-menu). egui reports no click to a disabled `Ui`'s widgets; the log paints cells into the
/// panel's buffer, which a disabled `Ui` does not dim, and its own entry shown whole is a modal of
/// its own.
fn clicks_off<R>(ui: &mut egui::Ui, off: bool, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    match off {
        true => ui.add_enabled_ui(false, f).inner,
        false => f(ui),
    }
}

/// The pass's keys and typed text, taken out of its input to be put back after the dialogs.
fn take_keys(ctx: &egui::Context) -> Vec<egui::Event> {
    ctx.input_mut(|i| {
        let (keys, rest) = std::mem::take(&mut i.events).into_iter().partition(|e| {
            matches!(
                e,
                egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::Ime(_)
                    | egui::Event::Copy
                    | egui::Event::Cut
                    | egui::Event::Paste(_)
            )
        });
        i.events = rest;
        keys
    })
}

/// Find's field at cells `cells` of `grid` (the heads' row, none in a panel too short for its
/// parts), typing while `typing` (its panel active, no dialog of the panel's up), and — while it
/// types — its keys in the pass's order with what is typed into it (X4-find): those after
/// anything typed are taken out first ([`find::take_trailing`]: the field's engine would end its
/// typing at an Enter), the field types, the bar jumps to the first match of a changed text, then
/// each acts — Enter the next match, Shift+Enter the previous, Esc closes Find (the caret gone),
/// what came after the Esc going on to the graph's keys ([`keys`]). Where those open Find again (a
/// letter typed after the Esc), the reopened field is laid in the same pass, so the rest of the
/// pass types into it in order and its cells show what it holds (fix-check: `[x, Esc, y, z]` lost
/// the z, the row blank for that pass) — the same id at the same cells again, which egui takes for
/// one widget seen twice, no clash. Its caret in `caret`; what the keys ask, and whether an Esc
/// left Find closed. `dialog`: a dialog of the panel's, or another's beside it, is up — the `Aa`
/// toggle takes no click then (F-git-listq review).
fn find_ui(
    ui: &mut egui::Ui,
    grid: &Grid,
    cells: Cells,
    buf: &mut Buffer,
    view: &mut GitView,
    (typing, dialog): (bool, bool),
    caret: &mut Option<cells::Position>,
) -> (Vec<GitAction>, bool) {
    let ctx = ui.ctx().clone();
    let key = view.find_key;
    let mut asked = Vec::new();
    // Each turn ends at an Esc after typing; the next lays Find again where the rest opened it.
    let mut typing = typing;
    loop {
        // Only the keyboard's own: a panel without it, or under a dialog of its own, leaves the
        // pass's Enter and Esc to what has the keyboard.
        let (after, rest) = match typing {
            true => ctx.input_mut(|i| find::take_trailing(&mut i.events)),
            false => Default::default(),
        };
        if let Some(find) = &mut view.find {
            *caret = find::field(ui, grid, cells, buf, find, (typing, dialog), key);
        }
        asked.extend(view.find_jump());
        let mut closed = false;
        for pressed in after {
            match pressed {
                find::FindKey::Next => asked.extend(view.find_step(true)),
                find::FindKey::Previous => asked.extend(view.find_step(false)),
                find::FindKey::Close => {
                    asked.extend(view.find_close());
                    *caret = None;
                    ctx.input_mut(|i| find::closed_with(&mut i.events, rest));
                    asked.extend(keys(&ctx, view));
                    closed = true;
                    break;
                }
            }
        }
        if !closed {
            return (asked, false);
        }
        if view.find.is_none() || view.search.is_some() {
            return (asked, true);
        }
        // Opened again by what followed the Esc: its field this pass, typing unless the rest
        // opened a dialog of the panel's.
        typing = typing && !view.dialog_up();
    }
}

/// The graph's part of a pass ([`draw`]): Find's field in the heads' row ([`find_ui`]: its text
/// changed, the bar jumps before the rows are drawn) or the column heads, the graph's rows — a
/// history search's flat list instead while one is shown — and what their clicks ask; the cells
/// egui draws go in `eguis`, Find's caret in `caret`. `(list_active, typing)`: the rows have the
/// keyboard; Find's field has it (its panel active, no dialog of the panel's up).
#[allow(clippy::too_many_arguments)]
fn graph_ui(
    ui: &mut egui::Ui,
    grid: &Grid,
    buf: &mut Buffer,
    at: &Layout,
    view: &mut GitView,
    (list_active, typing): (bool, bool),
    dialog: bool,
    eguis: &mut Vec<Cells>,
    caret: &mut Option<cells::Position>,
) -> Vec<GitAction> {
    let mut asked = Vec::new();
    let mut heads = Some(at.head);
    if view.find.is_some() && view.search.is_none() {
        let (found, closed) = find_ui(ui, grid, at.head, buf, view, (typing, dialog), caret);
        asked.extend(found);
        if closed {
            // An Esc after what was typed closed it: this pass's cells show the heads.
            let p = chrome::PALETTE;
            cells::fill(buf, at.head, Pen::new(p.text, p.back));
        } else {
            heads = None;
            if !find::PAINTS_CELLS {
                eguis.push(at.head);
            }
        }
    }
    let no_marks = HashSet::new();
    let key = view.find_key;
    let drawn = match &mut view.search {
        Some(results) => graph_view::draw(
            ui,
            grid,
            buf,
            heads,
            at.rows,
            &mut results.scroll,
            &graph_view::List {
                rows: &results.rows,
                graph: None,
                labels: &view.labels,
                marks: &no_marks,
                uncommitted: view.uncommitted,
                label_cursor: 0,
                active: list_active,
                ids: key.with("sopen-git-results"),
            },
        ),
        None => graph_view::draw(
            ui,
            grid,
            buf,
            heads,
            at.rows,
            &mut view.graph_scroll,
            &graph_view::List {
                rows: &view.rows,
                graph: Some(&view.graph),
                labels: &view.labels,
                marks: &view.marks,
                uncommitted: view.uncommitted,
                label_cursor: view.label_cursor,
                active: list_active,
                ids: key.with("sopen-git-rows"),
            },
        ),
    };
    view.geometry = drawn.geometry;
    // A right click on a row of the graph (the translator's `Secondary` in a terminal): the bar
    // on it, then its menu (03 §9.2).
    let right = ui.input(|i| {
        i.pointer
            .secondary_clicked()
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    if let (Some(pos), Some(g)) = (right.filter(|_| !dialog), view.geometry()) {
        let row = g.row_at(pos).filter(|&r| r < view.rows.len());
        if let (Some(row), true) = (row, view.search.is_none()) {
            // The keyboard back to the graph first, as a left click gives it, so the menu is the
            // clicked row's, never the pane's file (PR #90 review).
            if let Some(pane) = &mut view.pane {
                pane.keyboard = false;
            }
            asked.extend(view.move_bar(row));
            view.open_menu();
        }
    }
    if let Some((to, twice)) = drawn.clicked.filter(|_| !dialog) {
        if let Some(pane) = &mut view.pane {
            pane.keyboard = false;
        }
        match &mut view.search {
            Some(results) => {
                results.scroll.to(to);
                asked.extend(view.follow());
            }
            None => asked.extend(view.move_bar(to)),
        }
        if twice {
            let details = match view.search.is_some() {
                true => view.run_results(Command::Details),
                false => view.details_of_bar(),
            };
            asked.extend(details);
        }
    }
    asked
}

/// The slots of the git panel's F-key row in `mode` that act on every row (03 §9.1): the landed
/// plain F-keys of the git context but the action slots, by number — `1Help 3View 4Edit 9Dual`.
/// [`GitView::fkeys`] adds the action slots of the object under the bar.
pub fn fkeys(mode: Mode) -> Vec<FKey> {
    let mut row: Vec<FKey> = keys::rows(Context::Git, UPTO)
        .filter(|r| !ACTION_SLOTS.contains(&r.command))
        .filter_map(|r| match r.trigger {
            Trigger::Key(key, Mods::None) if !r.label.is_empty() => {
                chrome::fkey_number(key).map(|n| FKey {
                    n,
                    label: if r.command == Command::SwitchMode {
                        keys::switch_label(mode)
                    } else {
                        r.label
                    },
                })
            }
            _ => None,
        })
        .collect();
    row.sort_by_key(|k| k.n);
    row
}
