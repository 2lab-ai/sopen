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
//! Nothing uses it until S2-F; the `dead_code` allowance below goes then, as a hand-over declared
//! with it (this file is S2-D's).
#![allow(dead_code)]

mod details;
mod filters;
mod find;
mod graph_view;
#[cfg(test)]
mod tests;

// The integration's inputs (S2-F); the allowance goes with `dead_code`'s.
#[allow(unused_imports)]
pub use details::{Details, FileGroup, FileRow, Group, Summary};
#[allow(unused_imports)]
pub use filters::{BranchChoice, Filters, Order};
pub use find::SearchQuery;

use super::keys::{self, Command, Context, Mods, Step, Trigger};
use super::view::Mode;
use crate::git::graph::{Graph, Row};
use crate::git::model::{CommitRow, Oid, RefLabel};
use crate::gui::chrome::{self, FKey};
use crate::gui::Status;
use eframe::egui::{self, Key, Rect};
use std::collections::{HashMap, HashSet};

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
}

/// A label of a row, as drawn (03 §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub text: String,
    pub kind: LabelKind,
    /// The ref's name is not valid UTF-8 (shown with U+FFFD; the read model's `RefLabel::lossy`).
    pub lossy: bool,
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
            search_dialog: None,
            search: None,
            pane: None,
            viewed: HashSet::new(),
            said: None,
            geometry: None,
            pending: None,
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
                self.label_cursor = 0;
                self.graph_scroll.to(at);
                true
            }
            None => {
                let offset = self.graph_scroll.visible / 2;
                self.pending = Some((key, offset));
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

    /// Where the stream stands (the paging line).
    pub fn set_paging(&mut self, paging: Paging) {
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
    pub fn bar_place(&self) -> (usize, usize) {
        let s = &self.graph_scroll;
        (s.bar, s.bar.saturating_sub(s.top))
    }

    /// The rows the last pass drew.
    pub fn on_screen(&self) -> std::ops::Range<usize> {
        self.graph_scroll.range(self.rows.len())
    }

    /// Whether `row` is marked.
    pub fn marked(&self, row: &RowKey) -> bool {
        self.marks.contains(row)
    }

    /// The status row while the git panel has the keyboard: the view's own word, else the
    /// integration's, else what the keys do.
    pub fn status_line(&self) -> Status {
        if let Some(said) = self.said.as_ref().or(self.status.as_ref()) {
            return said.clone();
        }
        if self.find.is_some() {
            return Status::info(FIND_HINT);
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

    /// Find's text while its field is open.
    pub fn find_text(&self) -> Option<&str> {
        self.find.as_ref().map(|f| f.text.as_str())
    }

    /// Find's case toggle: case-sensitive or not (03 §4.7).
    pub fn set_find_case(&mut self, case: bool) {
        if let Some(find) = &mut self.find {
            find.case = case;
        }
    }

    /// Whether the details pane is open.
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
        *pane = details::Pane::new(subject.clone(), false, Vec::new());
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

    /// Enter (`forward`) or Shift+Enter in Find: the next or the previous match.
    fn find_step(&mut self, forward: bool) -> Option<GitAction> {
        self.refresh_find();
        let at = self.find.as_ref()?.step(self.graph_scroll.bar, forward)?;
        self.move_bar(at)
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
        self.pane = Some(details::Pane::new(subject.clone(), true, header));
        vec![GitAction::Details(subject)]
    }

    /// Carries out `command` (the key table's git context) on the view, and gives what is left
    /// for the integration.
    fn run(&mut self, command: Command) -> Vec<GitAction> {
        use GitAction as A;
        match command {
            Command::Help => return vec![A::Help],
            Command::SwitchMode => return vec![A::SwitchMode],
            Command::OtherSide => return vec![A::OtherSide],
            Command::Reread => return vec![A::Refresh],
            Command::Pin => return vec![A::Pin],
            Command::Wide => return vec![A::Wide],
            Command::HistorySearch => {
                self.search_dialog = Some(find::SearchDialog::default());
                return Vec::new();
            }
            _ => {}
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
                self.pane = Some(details::Pane::new(subject.clone(), true, Vec::new()));
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
                self.pane = Some(details::Pane::new(subject.clone(), true, Vec::new()));
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

    /// A dialog (the filters', history search's) is up: it has the keyboard.
    fn dialog_up(&self) -> bool {
        self.filters_dialog.is_some() || self.search_dialog.is_some()
    }
}

/// The slice the read-only git panel lands with (02 §8.1).
const SLICE: u8 = 2;

/// The key table's rows the panel reads: up to its own slice whatever [`keys::LANDED`] says yet;
/// later slices' rows (actions, the planner) only once they land.
const UPTO: u8 = if keys::LANDED > SLICE {
    keys::LANDED
} else {
    SLICE
};

/// What the status row says while nothing else is to be said.
const HINT: &str =
    "Enter details · Space mark · a letter finds · ← → labels · Tab the list · Esc back · F1 the keys";

/// What the status row says while Find is open.
const FIND_HINT: &str = "Enter next · Shift+Enter previous · Esc closes Find";

/// The git panel's keys this frame, through the key table's git context (04 §3: exact
/// modifiers) — the dialog context while Find's field has the typing, nothing while a dialog is
/// up (it takes its own) — in order: each carried out on the view, what is left for the
/// integration returned.
pub fn keys(ctx: &egui::Context, view: &mut GitView) -> Vec<GitAction> {
    let mut asked = Vec::new();
    while !view.dialog_up() {
        if view.search.is_some() {
            // Find is over the graph's rows: never left open under a history search's results.
            view.find = None;
        }
        if view.find.is_some() {
            // Find's field has the typing: Enter and Esc, and Shift+Enter, only (04 §3 rule 4).
            if ctx.input_mut(|i| keys::take(i, Key::Enter, Mods::Shift)) {
                asked.extend(view.find_step(false));
                continue;
            }
            match ctx.input_mut(|i| keys::find(&mut i.events, Context::Dialog, UPTO)) {
                Some(Command::Accept) => asked.extend(view.find_step(true)),
                Some(Command::Cancel) => view.find = None,
                _ => break,
            }
            continue;
        }
        let Some(command) = ctx.input_mut(|i| keys::find(&mut i.events, Context::Git, UPTO)) else {
            break;
        };
        view.said = None;
        asked.extend(view.run(command));
    }
    asked
}

/// The git panel in `rect`: its frame titled with the top level, the find row or the column
/// heads, the visible rows with their lanes, nodes and labels, the bar, the details pane below
/// them when it is open (the graph keeping at least 8 rows), the scroll bars, the paging line;
/// and the dialogs. A click puts the bar on a row, a double click is Enter there and the wheel
/// scrolls, as in the list.
pub fn draw(ui: &mut egui::Ui, rect: Rect, view: &mut GitView, active: bool) -> Vec<GitAction> {
    let mut asked = Vec::new();
    let ctx = ui.ctx().clone();
    let title = if view.filters.is_default() {
        view.title.clone()
    } else {
        format!("{} [filtered]", view.title)
    };
    let painter = ui.painter_at(rect);
    let inner = chrome::panel_frame(&painter, rect, &title, active);
    let font = chrome::mono(ui);
    let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = text_h.ceil();
    let head = Rect::from_min_size(inner.min, egui::vec2(inner.width(), row_h));
    let foot_y = inner.bottom() - row_h;
    let rule_y = (foot_y - row_h).max(head.bottom());
    let body = Rect::from_min_max(
        egui::pos2(inner.left(), head.bottom()),
        egui::pos2(inner.right(), rule_y),
    );
    let total = (body.height() / row_h).floor().max(0.0) as usize;
    let half = chrome::CELL / 2.0;
    // The pane below the graph, the graph keeping at least 8 rows (03 §6.1), a rule between.
    let (list_area, pane_area) = match view.pane.is_some() {
        true if total > 10 => {
            let graph_rows = (total / 2).max(8).min(total - 2);
            let list = Rect::from_min_size(
                body.min,
                egui::vec2(body.width(), row_h * graph_rows as f32),
            );
            let pane = Rect::from_min_max(egui::pos2(body.left(), list.bottom() + row_h), body.max);
            chrome::rule(
                &painter,
                inner.left() - half.x,
                inner.right() + half.x,
                list.bottom() + half.y,
                active,
            );
            (list, Some(pane))
        }
        _ => (body, None),
    };
    let pane_keyboard = view.pane.as_ref().is_some_and(|p| p.keyboard);
    let list_active = active && !pane_keyboard && !view.dialog_up();
    // Find's field over the graph, in the heads' row; its text changed, the bar jumps.
    let dialog = view.dialog_up();
    let heads = match &mut view.find {
        Some(find) if view.search.is_none() => {
            find::field(ui, head, find, cw, !dialog);
            None
        }
        _ => Some(head),
    };
    if view.find.is_some() && view.refresh_find() {
        if let Some(at) = view.find.as_ref().and_then(find::Find::first) {
            asked.extend(view.move_bar(at));
        }
    }
    let no_marks = HashSet::new();
    let drawn = match &mut view.search {
        Some(results) => graph_view::draw(
            ui,
            heads,
            list_area,
            &mut results.scroll,
            &graph_view::List {
                rows: &results.rows,
                graph: None,
                labels: &view.labels,
                marks: &no_marks,
                uncommitted: view.uncommitted,
                label_cursor: 0,
                active: list_active,
                salt: "sopen-git-results",
            },
        ),
        None => graph_view::draw(
            ui,
            heads,
            list_area,
            &mut view.graph_scroll,
            &graph_view::List {
                rows: &view.rows,
                graph: Some(&view.graph),
                labels: &view.labels,
                marks: &view.marks,
                uncommitted: view.uncommitted,
                label_cursor: view.label_cursor,
                active: list_active,
                salt: "sopen-git-rows",
            },
        ),
    };
    view.geometry = drawn.geometry;
    if let Some((at, twice)) = drawn.clicked.filter(|_| !dialog) {
        if let Some(pane) = &mut view.pane {
            pane.keyboard = false;
        }
        match &mut view.search {
            Some(results) => {
                results.scroll.to(at);
                asked.extend(view.follow());
            }
            None => asked.extend(view.move_bar(at)),
        }
        if twice {
            let details = match view.search.is_some() {
                true => view.run_results(Command::Details),
                false => view.details_of_bar(),
            };
            asked.extend(details);
        }
    }
    if let (Some(area), Some(pane)) = (pane_area, view.pane.as_mut()) {
        let subject = pane.subject.clone();
        let viewed = &view.viewed;
        let seen = |f: &FileRow| viewed.contains(&(subject.clone(), f.path.clone()));
        let pane_active = active && pane.keyboard && !dialog;
        if let Some((line, twice)) = details::draw(ui, area, pane, &seen, pane_active) {
            pane.keyboard = true;
            pane.scroll.to(line);
            if twice {
                asked.extend(view.run_pane(Command::Details));
            }
        }
    }
    asked.extend(view.ask_more());
    // The paging line: the line across the window, then what the stream says (Find's `k of n`
    // while it is open) and the branch's ahead / behind at the right when it fits.
    chrome::rule(
        &painter,
        inner.left() - half.x,
        inner.right() + half.x,
        rule_y + half.y,
        active,
    );
    let (n, noun, paging) = match &view.search {
        Some(s) => (s.rows.len(), "found", &s.paging),
        None => (view.commits, "commits", &view.paging),
    };
    let (mut foot, error) = graph_view::paging_text(paging, n, noun);
    if let Some(find) = view.find.as_ref().filter(|f| !f.text.is_empty()) {
        foot = find.foot(view.graph_scroll.bar);
    }
    let foot_rect = Rect::from_min_size(
        egui::pos2(inner.left(), foot_y),
        egui::vec2(inner.width(), row_h),
    );
    let foot_painter = ui.painter_at(foot_rect);
    let colour = if error {
        chrome::git::LOOK.error
    } else {
        chrome::git::LOOK.foot
    };
    let used = foot_painter
        .text(
            foot_rect.left_top(),
            egui::Align2::LEFT_TOP,
            &foot,
            font.clone(),
            colour,
        )
        .width();
    if let Some(t) = &view.tracking {
        let right = graph_view::tracking_text(t);
        let w = ctx.fonts_mut(|f| {
            f.layout_no_wrap(right.clone(), font.clone(), colour)
                .size()
                .x
        });
        if used + cw * 2.0 + w <= foot_rect.width() {
            foot_painter.text(
                foot_rect.right_top(),
                egui::Align2::RIGHT_TOP,
                right,
                font.clone(),
                chrome::git::LOOK.foot,
            );
        }
    }
    // A click on the paging line reads more (Enter on it, 03 §4.5).
    let foot_click = ui.interact(
        foot_rect,
        ui.scope_id().with("sopen-git-foot"),
        egui::Sense::click(),
    );
    if foot_click.clicked() && view.search.is_none() && !dialog {
        let more = matches!(view.paging, Paging::More(_) | Paging::Error(_));
        if more && (!view.more_asked || matches!(view.paging, Paging::Error(_))) {
            view.more_asked = true;
            asked.push(GitAction::MoreRows);
        }
    }
    // The dialogs, over everything.
    if let Some(dialog) = &mut view.filters_dialog {
        match dialog.ui(&ctx) {
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
        match dialog.ui(&ctx) {
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
    asked
}

/// The git panel's F-key row in `mode` (03 §9.1): the landed plain F-keys of the git context, by
/// number — `1Help 3View 4Edit 9Dual` in S2.
pub fn fkeys(mode: Mode) -> Vec<FKey> {
    let mut row: Vec<FKey> = keys::rows(Context::Git, UPTO)
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
