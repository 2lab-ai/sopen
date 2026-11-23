//! The git panel's side of the browser (03; 02 §3, §6): the window's git host, detection of the
//! active folder, a panel per repository (at most 3 kept), their loads, pages, details, history
//! searches, diffs and files at a revision (S2-F); the actions of S3 on a worker under the
//! repository's write lock, the in-progress probe, the window's undo, the Backups list and the
//! command log (S3-C). Moved out of `browser.rs` by S3-C; a child of it, so it reads the
//! browser's own fields.

use super::*;
use crate::git::actions::{self, Action, ActionId, Outcome, Plan, Repo, Seen, Step};
use crate::git::backup::{self, Record, RestoreEntry};
use crate::git::model::{BackupRef, GitVersion, InProgress};

/// Makes the window's git runner (02 §6 `GitHost`): on a worker, the first time detection needs
/// it (`ssh -G` for a remote, `xcode-select -p` for the macOS shim); why there is none, else.
pub type GitConnect = Arc<dyn Fn() -> std::result::Result<GitRunner, Unavailable> + Send + Sync>;

/// The git runner as the window's own connection reaches the folder: this machine's git (03
/// §2.4: `SOPEN_GIT`, else `git` on `PATH`, the shim only with the developer tools), or the
/// remote's over the ssh master (03 §3.2), the browser's `connect` bringing a missing master back.
pub fn git_connect(
    destination: Option<String>,
    label: Option<String>,
    state_dir: PathBuf,
    connect: Connect,
) -> GitConnect {
    Arc::new(move || match (&destination, &label) {
        (Some(dest), Some(label)) => {
            let ssh = SshGit::new(dest, label, &state_dir)
                .map_err(|e| Unavailable::Failed(format!("{e:#}")))?;
            let connect = connect.clone();
            let revive: crate::git::runner::Revive = Arc::new(move || connect().map(|_| ()));
            Ok(GitRunner::ssh(ssh).with_revive(revive))
        }
        _ => Ok(GitRunner::local(LocalGit::new(detect::which_git()?))),
    })
}

/// A history read off the window's thread: `rx` gets what it found; `handle` stops it (a newer
/// one replaces it, the panel goes).
pub(super) struct Pending<T> {
    pub(super) rx: Receiver<T>,
    pub(super) handle: Handle,
}

impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

/// The reads a load starts from (03 §4.1): refs, HEAD, stashes, status, the remotes, and the
/// in-progress probe (02 §6.1 step 5, 03 §8.5; S3-C).
#[derive(Debug, Clone)]
pub(super) struct Snapshot {
    pub(super) refs: Vec<RefLabel>,
    pub(super) head: Head,
    pub(super) stashes: Vec<StashRow>,
    pub(super) status: GitStatus,
    pub(super) remotes: Vec<String>,
    pub(super) in_progress: InProgress,
    /// The probe's seven `--git-path`s, read once per repository (03 §8.5).
    pub(super) paths: Vec<String>,
}

impl Snapshot {
    /// The same graph: refs, HEAD and stashes unchanged (03 §5's soft refresh keeps the rows).
    pub(super) fn same_graph(&self, other: &Snapshot) -> bool {
        self.refs == other.refs && self.head == other.head && self.stashes == other.stashes
    }
}

/// A load's snapshot read: a full load (`soft` false) starts a new stream; a soft refresh
/// starts one only when the refs, HEAD or stashes moved (03 §5).
pub(super) struct SnapshotRead {
    pub(super) soft: bool,
    /// Pages the new stream reads at once (a reload reads as many as were loaded, 03 §4.5).
    pub(super) pages: usize,
    pub(super) pending: Pending<std::result::Result<Snapshot, String>>,
}

/// The log stream of one load on a worker of its own (03 §4.5): asked for pages, it reads them;
/// between asks git waits on the full pipe. Dropping the link stops git (the handle) and ends
/// the worker (the sender).
pub(super) struct StreamLink {
    pub(super) generation: u64,
    pub(super) handle: Handle,
    pub(super) ask: mpsc::Sender<usize>,
    pub(super) pages: Receiver<Page>,
    /// Pages asked for and not come back yet.
    pub(super) asked: usize,
    pub(super) rows: RowStream,
    /// The next page is the load's first: it replaces the rows.
    pub(super) first: bool,
    /// The stream has ended: all loaded, an error, or stopped.
    pub(super) ended: bool,
}

impl Drop for StreamLink {
    fn drop(&mut self) {
        self.handle.stop();
    }
}

/// A history search's stream (03 §4.7), paged like the graph's.
pub(super) struct SearchLink {
    pub(super) query: git_panel::SearchQuery,
    pub(super) link: StreamLink,
}

/// What a file row's Enter or Shift+F3 found (03 §6.2–§6.3).
pub(super) enum Revealed {
    /// A diff or a blob written under `.revisions`: opened read-only, titled `host:/top/path @
    /// <rev>`; F4 opens the path now.
    File {
        file: PathBuf,
        title: String,
        edit: String,
    },
    /// The working file itself (the uncommitted row's untracked file, a comparison's work-tree
    /// end of one): opened as Enter in the list opens it.
    Working(String),
    /// Nothing to open, and why (`not in this version`, a read that failed).
    Refused(String),
}

/// The git panel of one repository (03), kept while the window lives (at most [`KEPT_PANELS`]).
pub(super) struct RepoPanel {
    /// Its top level.
    pub(super) top: String,
    /// Its git directory (detection's `--absolute-git-dir`): the actions' `Repo::git_dir`.
    pub(super) git_dir: String,
    /// Detection said it is a shallow clone: the last page's end is `history ends here`.
    pub(super) shallow: bool,
    pub(super) view: GitView,
    pub(super) generation: Generation,
    pub(super) snapshot: Option<Snapshot>,
    pub(super) read: Option<SnapshotRead>,
    pub(super) stream: Option<StreamLink>,
    /// The stream was ended while the panel was hidden (03 §4.5: after 30 s, or for a newer
    /// panel's): shown again, a new one reads as many pages as were loaded.
    pub(super) stream_dropped: bool,
    pub(super) search: Option<SearchLink>,
    /// A history search whose stream was ended while hidden: its next page runs it again.
    pub(super) search_dropped: Option<git_panel::SearchQuery>,
    pub(super) details: Option<Pending<Details>>,
    /// The details the pane was last given (an untracked folder's files go into them).
    pub(super) last_details: Option<Details>,
    pub(super) reveal: Option<Pending<Revealed>>,
    /// `SOPEN_STATE select=REV` resolved: the commit's id.
    pub(super) resolving: Option<Pending<std::result::Result<Oid, String>>>,
    /// Shown now (egui time it was last shown; `None` while it shows).
    pub(super) hidden_since: Option<std::time::Instant>,
    /// When the panel last asked for its refs (a refresh younger than 10 s is not repeated).
    pub(super) refreshed: Option<std::time::Instant>,
    /// Least recently shown first, by this counter.
    pub(super) last_shown: u64,
    // S3 (S3-C): the actions.
    /// Actions running or waiting for the write lock (02 §6), oldest first; never stopped by a
    /// drop (see [`Running`]).
    pub(super) running: Vec<Running>,
    /// A dialog's prefill being read ([`GitAction::Open`]).
    pub(super) prefill: Option<Pending<Prefilled>>,
    /// A dialog's reads beyond the refresh (`actions::inspect`), for the snapshot's generation.
    pub(super) inspect: Option<(Pending<Inspected>, u64)>,
    /// The reads that failed for the snapshot of that generation: never recorded, never asked
    /// again for it — the plan stays "not read yet" and the dialog says so.
    pub(super) failed_reads: Option<(u64, Vec<actions::Need>)>,
    /// The draft, snapshot generation and number of reads `actions::needs` was last asked about.
    pub(super) checked: Option<(Action, u64, usize)>,
    /// The Backups list being read.
    pub(super) backups_read: Option<Pending<std::result::Result<Vec<Record>, backup::Failed>>>,
    /// A restore's entries being read: the group's stamp, the one ref asked for.
    pub(super) restore_read: Option<Pending<RestoreRead>>,
    /// A text being read for a dialog of its own (a tag's details): its title.
    pub(super) text_read: Option<Pending<(String, std::result::Result<String, String>)>>,
    /// Backup refs the Backups list's Show added to the graph's revisions, until the next
    /// refresh (03 §8.2).
    pub(super) shown: Vec<Oid>,
}

/// Panels kept per window (02 §3.1: the least recently shown is dropped).
pub(super) const KEPT_PANELS: usize = 3;
/// A hidden panel's stream ends after this long (03 §4.5).
pub(super) const HIDDEN_STREAM: Duration = Duration::from_secs(30);
/// A refresh younger than this is not repeated when the panel is shown again (03 §5).
pub(super) const REFRESH_AGAIN: Duration = Duration::from_secs(10);
/// Parked log streams per host at most: each holds one of the host's 3 command slots (02 §6),
/// and a read must always find one.
pub(super) const PARKED_STREAMS: usize = 2;

/// A running command silent this long is said to be maybe waiting for the owner, and a write
/// running this long can be stopped by Esc (03 §3.4).
pub(super) const QUIET_AFTER: Duration = Duration::from_secs(10);

/// `SOPEN_STATE`'s git keys (02 §9): what is left to do once the panel is there.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct GitScript {
    pub(super) pin: bool,
    pub(super) select: Option<String>,
    pub(super) details: bool,
    pub(super) label: Option<usize>,
    /// `dialog=<id>`: that action's dialog on the object under the bar (S3).
    pub(super) dialog: Option<ActionId>,
}

impl GitScript {
    pub(super) fn done(&self) -> bool {
        *self == GitScript::default()
    }
}

/// The window's git state (02 §6's `GitHost` and its repositories).
pub(super) struct GitSide {
    pub(super) connect: Option<GitConnect>,
    /// The host's runner, made by the first detection's worker; why there is none, until the
    /// list's Cmd/Ctrl+R asks again (03 §2.2: a host without git is detected again then).
    pub(super) runner: Arc<Mutex<Option<std::result::Result<GitRunner, Unavailable>>>>,
    pub(super) detector: Arc<Detector>,
    /// The host as detection's cache names it: empty on this machine, else the label.
    pub(super) host: String,
    /// Each folder's detection this window had (03 §2.2), and those still asked.
    pub(super) detections: HashMap<String, Detection>,
    pub(super) asking: HashMap<String, Receiver<Detection>>,
    /// Folders whose reason the status row has said (once per folder, 03 §2.3).
    pub(super) said: std::collections::HashSet<String>,
    pub(super) panels: Vec<RepoPanel>,
    pub(super) shown_counter: u64,
    pub(super) pin: Option<view::Pin>,
    /// The keyboard is in the git panel on the side opposite the active list (the active list
    /// is the origin list then, 02 §3.1 `origin`).
    pub(super) focus: bool,
    /// That panel's repository, as the display last showed it (a viewer it opened covers it).
    pub(super) focus_top: Option<String>,
    /// The keyboard's git panel over both sides (Cmd/Ctrl+Shift+G).
    pub(super) wide: bool,
    /// The window's folder under `.revisions` (03 §6.2), made on the first diff's worker.
    pub(super) space: Arc<Mutex<Option<crate::cache::RevisionSpace>>>,
    pub(super) script: GitScript,
    /// The window had the focus at the last pass (a soft refresh when it gains it, 03 §5).
    pub(super) window_focused: Option<bool>,
    /// How long a hidden panel keeps its stream ([`HIDDEN_STREAM`]; tests shorten it).
    pub(super) hidden_stream: Duration,
    // S3 (S3-C).
    /// The backup groups this window wrote, per repository (Cmd/Ctrl+Z, 03 §8.3).
    pub(super) undo: HashMap<String, backup::Undo>,
    /// Each repository's write lock (02 §6): one repository-changing action at a time.
    pub(super) write_locks: HashMap<String, Arc<Mutex<()>>>,
    /// A running command silent this long gets the status row's "may be waiting for you" (03
    /// §3.4: 10 s; tests shorten it).
    pub(super) quiet_after: Duration,
    /// Esc asks "Stop git?" of a write running this long (03 §3.4: 10 s; tests shorten it).
    pub(super) stop_after: Duration,
}

impl GitSide {
    pub(super) fn new(connect: Option<GitConnect>, label: Option<&str>) -> Self {
        Self {
            connect,
            runner: Arc::default(),
            detector: Arc::new(Detector::new()),
            host: label.unwrap_or_default().to_string(),
            detections: HashMap::new(),
            asking: HashMap::new(),
            said: Default::default(),
            panels: Vec::new(),
            shown_counter: 0,
            pin: None,
            focus: false,
            focus_top: None,
            wide: false,
            space: Arc::default(),
            script: GitScript::default(),
            window_focused: None,
            hidden_stream: HIDDEN_STREAM,
            undo: HashMap::new(),
            write_locks: HashMap::new(),
            quiet_after: QUIET_AFTER,
            stop_after: QUIET_AFTER,
        }
    }

    /// The repository of `folder` when detection found a work tree there.
    pub(super) fn repo_of(&self, folder: &str) -> Option<&str> {
        match self.detections.get(folder) {
            Some(Detection::WorkTree { top, .. }) => Some(top),
            _ => None,
        }
    }

    pub(super) fn panel(&self, top: &str) -> Option<usize> {
        self.panels.iter().position(|p| p.top == top)
    }

    /// Nothing git is on its way that a receipt waits for.
    pub(super) fn settled(&self, active_folder: &str, shown: &[view::Content]) -> bool {
        if self.asking.contains_key(active_folder) || !self.script.done() {
            return false;
        }
        self.panels.iter().all(|p| {
            let showing = shown
                .iter()
                .any(|c| matches!(c, view::Content::Git(top) if *top == p.top));
            !showing
                || (p.read.is_none()
                    && p.stream.as_ref().is_none_or(|s| s.asked == 0)
                    && p.details.is_none()
                    && p.reveal.is_none()
                    && p.resolving.is_none()
                    && p.view.selecting().is_none()
                    && p.search.as_ref().is_none_or(|s| s.link.asked == 0)
                    && !p.acting()
                    && !p.dialog_reading())
        })
    }

    pub(super) fn busy(&self) -> bool {
        !self.asking.is_empty()
            || self.panels.iter().any(|p| {
                p.read.is_some()
                    || p.stream.as_ref().is_some_and(|s| s.asked > 0)
                    || p.details.is_some()
                    || p.reveal.is_some()
                    || p.resolving.is_some()
                    || p.search.as_ref().is_some_and(|s| s.link.asked > 0)
                    || p.acting()
            })
    }
}

impl RepoPanel {
    /// An action runs or waits for the write lock, or a read for one is on its way (a prefill,
    /// a dialog's reads, the Backups list, a restore's entries, a tag's details).
    pub(super) fn acting(&self) -> bool {
        !self.running.is_empty()
            || self.prefill.is_some()
            || self.inspect.is_some()
            || self.backups_read.is_some()
            || self.restore_read.is_some()
            || self.text_read.is_some()
    }

    /// The open dialog still lacks a read its plan needs (`actions::needs`) that has not failed:
    /// it says `not read yet` until the next pass asks for it.
    pub(super) fn dialog_reading(&self) -> bool {
        match (self.view.draft(), self.view.repo()) {
            (Some(draft), Some((repo, generation))) => actions::needs(draft, repo)
                .iter()
                .any(|n| !self.read_failed(generation, n)),
            _ => false,
        }
    }

    /// `need` failed for the snapshot of `generation` (it is not asked again for it).
    pub(super) fn read_failed(&self, generation: u64, need: &actions::Need) -> bool {
        self.failed_reads
            .as_ref()
            .is_some_and(|(g, failed)| *g == generation && failed.contains(need))
    }
}

/// HEAD as the panel's labels take it.
pub(super) fn head_ref(head: &Head) -> HeadRef {
    match head {
        Head::Branch { refname, .. } => HeadRef::Branch(refname.clone()),
        Head::Detached(oid) => HeadRef::Detached(oid.clone()),
        Head::Unborn { .. } => HeadRef::Unborn,
    }
}

/// The foot's right end from the status read's branch headers (03 §4.2).
pub(super) fn tracking(status: &GitStatus) -> Option<Tracking> {
    let b = &status.branch;
    let (Some(branch), Some(upstream), Some(ab)) = (&b.head, &b.upstream, &b.ahead_behind) else {
        return None;
    };
    Some(Tracking {
        branch: branch.clone(),
        upstream: upstream.clone(),
        ahead: ab.ahead,
        behind: ab.behind,
    })
}

/// The log's options from the panel's filters (03 §4.6).
pub(super) fn log_options(f: &git_panel::Filters) -> LogOptions {
    LogOptions {
        order: match f.order {
            git_panel::Order::Date => history::Order::Date,
            git_panel::Order::AuthorDate => history::Order::AuthorDate,
            git_panel::Order::Topo => history::Order::Topo,
        },
        first_parent: f.first_parent,
        reflog: f.reflog,
    }
}

/// The log's roots for the filters (03 §4.1, §4.6).
pub(super) fn roots_of(f: &git_panel::Filters, s: &Snapshot) -> Vec<Oid> {
    let revisions = f.revisions(&s.refs, &s.remotes);
    let head = f.head_is_revision().then_some(&s.head);
    history::roots(&revisions, head, f.stash_revisions(&s.stashes))
}

/// The four reads of a load, the remotes and the in-progress probe (03 §4.1, §8.5), at once (at
/// most 3 in flight per host). `paths`: the probe's `--git-path`s when this repository's were
/// read already (else they are read first, once).
pub(super) fn read_snapshot(
    git: &RunnerGit,
    top: &str,
    paths: Option<Vec<String>>,
) -> std::result::Result<Snapshot, String> {
    std::thread::scope(|s| {
        let refs = s.spawn(|| history::refs(git, top));
        let head = s.spawn(|| history::head(git, top));
        let stashes = s.spawn(|| history::stashes(git, top));
        let status = s.spawn(|| history::status(git, top));
        let probe = s.spawn(|| read_probe(git, top, paths));
        let remotes = history::remotes(git, top);
        let said = |what: &str, e: history::ReadError| format!("git panel: the {what} read: {e}");
        let (in_progress, paths) = probe
            .join()
            .unwrap_or_else(|_| Err("the read stopped".into()))
            .map_err(|e| format!("git panel: the in-progress probe: {e}"))?;
        Ok(Snapshot {
            refs: joined(refs.join()).map_err(|e| said("refs", e))?,
            head: joined(head.join()).map_err(|e| said("HEAD", e))?,
            stashes: joined(stashes.join()).map_err(|e| said("stashes", e))?,
            status: joined(status.join()).map_err(|e| said("status", e))?,
            remotes: remotes.map_err(|e| said("remotes", e))?,
            in_progress,
            paths,
        })
    })
}

/// The in-progress probe (03 §8.5): its paths once per repository (`rev-parse --git-path …`),
/// then the probe itself, a fixed sh snippet; its state and the paths.
fn read_probe(
    git: &RunnerGit,
    top: &str,
    paths: Option<Vec<String>>,
) -> std::result::Result<(InProgress, Vec<String>), String> {
    let paths = match paths {
        Some(paths) => paths,
        None => {
            let done = git
                .runner
                .run(&actions::probe_paths_command(top), &git.handle);
            if !done.ok() {
                return Err(done.entry.status_line());
            }
            actions::probe_paths(&done.stdout)
                .ok_or_else(|| "rev-parse --git-path printed something else".to_string())?
        }
    };
    let done = git
        .runner
        .run(&actions::probe_command(top, &paths), &git.handle);
    if !done.ok() {
        return Err(done.entry.status_line());
    }
    Ok((actions::parse_probe(&done.stdout), paths))
}

/// A read joined from its thread (a panic in it is a read that stopped).
pub(super) fn joined<T>(
    r: std::thread::Result<std::result::Result<T, history::ReadError>>,
) -> std::result::Result<T, history::ReadError> {
    r.unwrap_or_else(|_| {
        Err(history::ReadError::Run(history::RunError::Failed(
            "the read stopped".into(),
        )))
    })
}

/// A change letter as the pane draws it (03 §6.1).
pub(super) fn letter_of(letter: FileLetter) -> char {
    match letter {
        FileLetter::Change(c) => change_char(c),
        FileLetter::Unmerged => 'U',
        FileLetter::Untracked => '?',
    }
}

pub(super) fn change_char(c: Change) -> char {
    match c {
        Change::Added => 'A',
        Change::Modified => 'M',
        Change::Deleted => 'D',
        Change::Renamed => 'R',
        Change::TypeChanged => 'T',
        Change::Copied => 'C',
    }
}

pub(super) fn file_row(f: ChangedFile) -> FileRow {
    FileRow {
        letter: letter_of(f.letter),
        path: f.path,
        old_path: f.old_path,
        lines: f.lines,
        lossy: f.lossy,
    }
}

/// `+adds -dels` by path (a binary file has none).
pub(super) type LineCounts = HashMap<String, (u64, u64)>;

/// The uncommitted row's groups (03 §6.1) from the status read, with their line counts.
pub(super) fn uncommitted_groups(status: &GitStatus, counts: &[LineCounts; 2]) -> Vec<FileGroup> {
    let mut conflicts = Vec::new();
    let mut staged = Vec::new();
    let mut not_staged = Vec::new();
    let mut untracked = Vec::new();
    for e in &status.entries {
        let row = |letter: char, lines: Option<(u64, u64)>| FileRow {
            letter,
            path: e.path.clone(),
            old_path: e.old_path.clone(),
            lines,
            lossy: e.lossy,
        };
        match &e.state {
            StatusState::Conflict(_) => conflicts.push(row('U', None)),
            StatusState::Untracked => untracked.push(FileRow {
                old_path: None,
                ..row('?', None)
            }),
            StatusState::Changed {
                staged: s,
                unstaged: u,
            } => {
                if let Some(c) = s {
                    staged.push(row(change_char(*c), counts[0].get(&e.path).copied()));
                }
                if let Some(c) = u {
                    not_staged.push(FileRow {
                        old_path: None,
                        ..row(change_char(*c), counts[1].get(&e.path).copied())
                    });
                }
            }
        }
    }
    [
        (Group::Conflicts, conflicts),
        (Group::Staged, staged),
        (Group::NotStaged, not_staged),
        (Group::Untracked, untracked),
    ]
    .into_iter()
    .filter(|(_, files)| !files.is_empty())
    .map(|(group, files)| FileGroup { group, files })
    .collect()
}

/// A commit's summary as the pane takes it.
pub(super) fn summary(d: &CommitDetails) -> Summary {
    Summary {
        oid: d.oid.clone(),
        parents: d.parents.clone(),
        author_name: d.author_name.clone(),
        author_email: d.author_email.clone(),
        author_time: d.author_time,
        committer_name: d.committer_name.clone(),
        committer_email: d.committer_email.clone(),
        commit_time: d.commit_time,
        message: d.message.clone(),
    }
}

/// What the load knows that a details read needs: the stashes (a stash row's parents) and the
/// status (the uncommitted row's groups).
#[derive(Clone)]
pub(super) struct DetailsInputs {
    pub(super) stashes: Vec<StashRow>,
    pub(super) status: GitStatus,
}

/// The details of `subject` (03 §6.1, §6.4), told through `tell` — the summary and files first,
/// then again with `In` once `for-each-ref --contains` has answered.
pub(super) fn read_details(
    git: &RunnerGit,
    top: &str,
    subject: &Subject,
    inputs: &DetailsInputs,
    tell: &dyn Fn(Details),
) {
    let mut details = Details {
        subject: subject.clone(),
        summary: None,
        contains: None,
        groups: Vec::new(),
        error: None,
    };
    let fail = |mut d: Details, e: history::ReadError| {
        d.error = Some(e.to_string());
        tell(d)
    };
    let counts = || -> std::result::Result<[LineCounts; 2], history::ReadError> {
        Ok([
            history::uncommitted_counts(git, top, true)?,
            history::uncommitted_counts(git, top, false)?,
        ])
    };
    let files = |rows: Vec<ChangedFile>| FileGroup {
        group: Group::Files,
        files: rows.into_iter().map(file_row).collect(),
    };
    let oid_of = |key: &RowKey| match key {
        RowKey::Commit(oid) | RowKey::Stash(oid) => Some(oid.clone()),
        RowKey::Uncommitted => None,
    };
    match subject {
        Subject::Row(RowKey::Uncommitted) => {
            let counts = match counts() {
                Ok(c) => c,
                Err(e) => return fail(details, e),
            };
            details.groups = uncommitted_groups(&inputs.status, &counts);
            tell(details);
        }
        Subject::Row(RowKey::Stash(oid)) => {
            let d = match history::details(git, top, oid) {
                Ok(d) => d,
                Err(e) => return fail(details, e),
            };
            details.summary = Some(summary(&d));
            let stash = inputs.stashes.iter().find(|s| s.oid == *oid).cloned();
            let stash = stash.unwrap_or_else(|| StashRow {
                oid: oid.clone(),
                base: d.parents.first().cloned().unwrap_or_else(|| oid.clone()),
                index: d.parents.get(1).cloned(),
                untracked: d.parents.get(2).cloned(),
                selector: String::new(),
                commit_time: d.commit_time,
                subject: String::new(),
            });
            match history::stash_files(git, top, &stash) {
                Ok(rows) => {
                    let (untracked, tracked): (Vec<_>, Vec<_>) = rows
                        .into_iter()
                        .partition(|f| f.letter == FileLetter::Untracked);
                    details.groups.push(files(tracked));
                    if !untracked.is_empty() {
                        details.groups.push(FileGroup {
                            group: Group::Untracked,
                            ..files(untracked)
                        });
                    }
                    tell(details);
                }
                Err(e) => fail(details, e),
            }
        }
        Subject::Row(RowKey::Commit(oid)) => {
            let d = match history::details(git, top, oid) {
                Ok(d) => d,
                Err(e) => return fail(details, e),
            };
            details.summary = Some(summary(&d));
            match history::commit_files(git, top, &d) {
                Ok(rows) => details.groups.push(files(rows)),
                Err(e) => return fail(details, e),
            }
            tell(details.clone());
            // A failed read is said as such, never as "in no branch".
            details.contains = Some(
                history::contains(git, top, oid)
                    .unwrap_or_else(|e| vec![format!("unavailable: {e}")]),
            );
            tell(details);
        }
        Subject::Compare { from, to } => {
            let result = match (oid_of(from), oid_of(to)) {
                (Some(a), Some(b)) => {
                    history::files(git, top, &a, &b).map(|rows| vec![files(rows)])
                }
                (Some(a), None) | (None, Some(a)) => {
                    history::worktree_files(git, top, &a).map(|rows| {
                        let mut groups = vec![files(rows)];
                        let untracked: Vec<FileRow> = inputs
                            .status
                            .entries
                            .iter()
                            .filter(|e| e.state == StatusState::Untracked)
                            .map(|e| FileRow {
                                letter: '?',
                                path: e.path.clone(),
                                old_path: None,
                                lines: None,
                                lossy: e.lossy,
                            })
                            .collect();
                        if !untracked.is_empty() {
                            groups.push(FileGroup {
                                group: Group::Untracked,
                                files: untracked,
                            });
                        }
                        groups
                    })
                }
                (None, None) => Ok(Vec::new()),
            };
            match result {
                Ok(groups) => {
                    details.groups = groups;
                    tell(details);
                }
                Err(e) => fail(details, e),
            }
        }
    }
}

/// The two endpoints of a file row (03 §6.2's table); `None` for the working file.
pub(super) fn endpoints(
    git: &RunnerGit,
    top: &str,
    subject: &Subject,
    group: Group,
) -> std::result::Result<(Option<DiffSide>, Option<DiffSide>, String), history::ReadError> {
    let parents = |oid: &Oid| history::details(git, top, oid).map(|d| d.parents);
    let empty = || history::empty_tree(git, top);
    let short = |oid: &Oid| oid.as_str().chars().take(8).collect::<String>();
    Ok(match subject {
        Subject::Row(RowKey::Commit(c)) => {
            let old = match parents(c)?.first() {
                Some(p) => p.clone(),
                None => empty()?,
            };
            (
                Some(DiffSide::Tree(old)),
                Some(DiffSide::Tree(c.clone())),
                short(c),
            )
        }
        Subject::Row(RowKey::Stash(s)) => {
            let parents = parents(s)?;
            match (group, parents.get(2)) {
                (Group::Untracked, Some(untracked)) => (
                    Some(DiffSide::Tree(empty()?)),
                    Some(DiffSide::Tree(untracked.clone())),
                    format!("{}^3", short(s)),
                ),
                _ => (
                    Some(DiffSide::Tree(parents.first().cloned().unwrap_or(empty()?))),
                    Some(DiffSide::Tree(s.clone())),
                    short(s),
                ),
            }
        }
        Subject::Row(RowKey::Uncommitted) => match group {
            Group::Staged => {
                let head = history::head(git, top)?;
                let old = match head.oid() {
                    Some(oid) => oid.clone(),
                    None => empty()?,
                };
                (
                    Some(DiffSide::Tree(old)),
                    Some(DiffSide::Index),
                    "index".into(),
                )
            }
            Group::Untracked => (None, None, "work tree".into()),
            _ => (
                Some(DiffSide::Index),
                Some(DiffSide::WorkTree),
                "work tree".into(),
            ),
        },
        Subject::Compare { from, to } => {
            let oid = |k: &RowKey| match k {
                RowKey::Commit(o) | RowKey::Stash(o) => Some(o.clone()),
                RowKey::Uncommitted => None,
            };
            match (oid(from), oid(to)) {
                (Some(a), Some(b)) => {
                    let rev = short(&b);
                    (Some(DiffSide::Tree(a)), Some(DiffSide::Tree(b)), rev)
                }
                (Some(a), None) if group == Group::Untracked => {
                    let _ = a;
                    (None, None, "work tree".into())
                }
                (Some(a), None) | (None, Some(a)) => (
                    Some(DiffSide::Tree(a)),
                    Some(DiffSide::WorkTree),
                    "work tree".into(),
                ),
                (None, None) => (None, None, "work tree".into()),
            }
        }
    })
}

/// The most of a diff written (03 §6.2): a longer one is cut there with a last line saying so.
pub(super) const DIFF_CAP: usize = 256 << 20;

/// Writes `bytes` to a new file named `name` in the window's `.revisions` folder (made here, on
/// this worker, the first time).
pub(super) fn revision_file(
    space: &Mutex<Option<crate::cache::RevisionSpace>>,
    cache: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let mut space = space.lock().unwrap_or_else(|p| p.into_inner());
    if space.is_none() {
        *space = Some(crate::cache::RevisionSpace::new(cache)?);
    }
    let Some(space) = space.as_ref() else {
        bail!("no folder for files at a revision");
    };
    let (path, mut file) = space.create(name)?;
    use std::io::Write as _;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(path)
}

/// The file at `path`'s last component, `.diff` added for a diff.
pub(super) fn file_name(path: &str) -> &str {
    crate::remote::split_parent(path.trim_end_matches('/')).1
}

/// Enter (`diff`) or Shift+F3 on a file row (03 §6.2, §6.3): the diff of its endpoints, or its
/// new endpoint's bytes, written under `.revisions`; the working file itself where that is the
/// endpoint.
#[allow(clippy::too_many_arguments)]
pub(super) fn reveal(
    git: &RunnerGit,
    top: &str,
    shown_top: &str,
    subject: &Subject,
    group: Group,
    file: &FileRow,
    diff: bool,
    space: &Mutex<Option<crate::cache::RevisionSpace>>,
    cache: &std::path::Path,
) -> Revealed {
    let working = || Revealed::Working(format!("{}/{}", top.trim_end_matches('/'), file.path));
    let edit = format!("{}/{}", top.trim_end_matches('/'), file.path);
    let (old, new, rev) = match endpoints(git, top, subject, group) {
        Ok(e) => e,
        Err(e) => return Revealed::Refused(format!("{}: {e}", file.path)),
    };
    let title = format!("{}/{} @ {rev}", shown_top.trim_end_matches('/'), file.path);
    let written = |name: String, bytes: &[u8]| match revision_file(space, cache, &name, bytes) {
        Ok(path) => Revealed::File {
            file: path,
            title: title.clone(),
            edit: edit.clone(),
        },
        Err(e) => Revealed::Refused(format!("{}: {e:#}", file.path)),
    };
    let read = |args: Vec<String>| {
        let cmd = GitCommand::new(Class::Read, top, args).max_output(DIFF_CAP);
        let done = git.runner.run(&cmd, &git.handle);
        match &done.ending {
            Ending::Exit(0) => Ok((done.stdout, false)),
            Ending::Stopped(s) if matches!(s.by, StopBy::Overflow(_)) => Ok((done.stdout, true)),
            _ => Err(done.entry.status_line()),
        }
    };
    if diff {
        let (Some(old), Some(new)) = (old, new) else {
            // The uncommitted row's untracked file (and a comparison's): the working file.
            return working();
        };
        let paths: Vec<&str> = file
            .old_path
            .iter()
            .map(String::as_str)
            .chain([file.path.as_str()])
            .collect();
        return match read(history::argv::diff_text(&old, &new, &paths)) {
            Ok((mut bytes, cut)) => {
                if cut {
                    bytes.extend_from_slice(b"\n... the diff is cut here: it is over 256 MiB\n");
                }
                written(format!("{}.diff", file_name(&file.path)), &bytes)
            }
            Err(why) => Revealed::Refused(format!("{}: {why}", file.path)),
        };
    }
    // Shift+F3: the new endpoint; none for a deleted file.
    if file.letter == 'D' {
        return Revealed::Refused(format!("{}: not in this version", file.path));
    }
    let rev = match new {
        Some(DiffSide::Tree(oid)) => Some(oid),
        Some(DiffSide::Index) => None,
        Some(DiffSide::WorkTree) | None => return working(),
    };
    match read(history::argv::blob(rev.as_ref(), &file.path)) {
        Ok((bytes, false)) => written(file_name(&file.path).to_string(), &bytes),
        Ok((_, true)) => Revealed::Refused(format!("{}: over 256 MiB", file.path)),
        Err(why) => Revealed::Refused(format!("{}: {why}", file.path)),
    }
}

/// The first page of `folder`'s git panel as text (`SOPEN_LIST=git`, 02 §9; 03 §11 G-D1, G-D2,
/// G-G1): the title line, a line per row — the lanes in ASCII, the labels, the subject, the
/// author, the date, the hash — then the paging line. A folder outside a work tree: the listing
/// and one line saying why.
pub fn git_listing(
    fs: &mut dyn RemoteFs,
    label: Option<&str>,
    abs: &str,
    connect: &GitConnect,
) -> Result<Vec<String>> {
    let host = label.unwrap_or_default();
    let detection = match connect() {
        Ok(runner) => Detector::new().detect(host, abs, &RunnerGit::new(runner, Handle::new())),
        Err(why) => Detection::Unavailable(why),
    };
    let Detection::WorkTree { top, shallow, .. } = &detection else {
        let mut out = listing(fs, label, abs)?;
        let shown = label.unwrap_or("this machine");
        let why = detection
            .reason(shown)
            .unwrap_or_else(|| "git panel: not a work tree".into());
        out.push(why);
        return Ok(out);
    };
    let runner = connect().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let git = RunnerGit::new(runner, Handle::new());
    let snapshot = read_snapshot(&git, top, None).map_err(|e| anyhow::anyhow!(e))?;
    let title = format!("{}{}", show(label, top), detection.title_notes());
    let mut view = GitView::new(title.clone());
    view.set_remotes(snapshot.remotes.clone());
    view.set_refs(snapshot.refs.clone(), head_ref(&snapshot.head));
    view.tracking = tracking(&snapshot.status);
    let f = view.filters().clone();
    let mut rows = RowStream::new(
        snapshot.head.oid().cloned(),
        !snapshot.status.entries.is_empty(),
        snapshot.stashes.clone(),
        f.stashes,
    );
    let roots = roots_of(&f, &snapshot);
    let n = snapshot.status.entries.len();
    let current = Generation::new();
    let g = current.bump();
    match LogStream::start(&git, top, &log_options(&f), &roots, &current, g) {
        Ok(mut stream) => {
            let page = stream.page(history::PAGE);
            view.set_rows(rows.page(page.rows), f.first_parent, n);
            view.set_paging(paging_of(&page.end, *shallow));
        }
        Err(StartError::NoRoots) => {
            view.set_rows(rows.page(Vec::new()), f.first_parent, n);
            view.set_paging(Paging::NoCommits);
        }
        Err(e) => bail!("git panel: the log: {e:?}"),
    }
    let mut out = vec![title];
    for i in 0..view.rows().len() {
        out.extend(text_row(&view, i));
    }
    out.push(view.paging_line());
    Ok(out)
}

/// What the paging line says after a page that ended so.
pub(super) fn paging_of(end: &PageEnd, shallow: bool) -> Paging {
    match end {
        PageEnd::More => Paging::More(history::PAGE),
        PageEnd::AllLoaded if shallow => Paging::Shallow,
        PageEnd::AllLoaded => Paging::AllLoaded,
        PageEnd::Error(e) => Paging::Error(e.clone()),
        PageEnd::Superseded => Paging::Loading,
    }
}

/// Row `i` of `view` as `SOPEN_LIST=git` prints it: the lanes in ASCII (`*` a commit, `@` the
/// checked-out one, `=` a stash, `o` the uncommitted row, `|` a line, `/` `\` a lane change),
/// the labels in brackets, the subject, then the author, the date and the hash.
pub(super) fn text_row(view: &GitView, i: usize) -> Option<String> {
    use crate::git::graph::{End, Node};
    let row = view.line(i)?;
    let graph = view.graph();
    let width = (graph.width() as usize).max(1);
    let mut cells = vec![' '; width * 2];
    for p in graph.pass_throughs(i..i + 1) {
        if let Some(c) = cells.get_mut(p.slot as usize * 2) {
            *c = '|';
        }
    }
    let rg = graph.row(i);
    let column = rg.column as usize;
    for s in graph.segments(i) {
        let slot = s.slot as usize;
        if slot == column {
            continue;
        }
        let between = slot.min(column) * 2 + 1;
        let up = matches!(s.end, End::Top);
        let glyph = match (up, slot > column) {
            (true, true) | (false, false) => '/',
            _ => '\\',
        };
        if let Some(c) = cells.get_mut(between) {
            *c = glyph;
        }
    }
    if let Some(c) = cells.get_mut(column * 2) {
        *c = match rg.node {
            Node::Commit => '*',
            Node::CheckedOut => '@',
            Node::Stash => '=',
            Node::Uncommitted => 'o',
        };
    }
    let lanes: String = cells.into_iter().collect();
    let labels: String = row.labels.iter().map(|l| format!("[{l}] ")).collect();
    let mut line = format!(
        "{lanes}  {labels}{}  {}  {}  {}",
        row.subject, row.author, row.date, row.commit
    );
    let trimmed = line.trim_end().len();
    line.truncate(trimmed);
    Some(line)
}

impl Browser {
    /// The runner, once the first detection's worker made it (never made on a frame).
    /// Never waits: the lock is held only to read or store the result (the connect runs outside
    /// it), and a moment it is held reads as "not ready yet".
    pub(super) fn git_runner(&self) -> Option<GitRunner> {
        let made = match self.git.runner.try_lock() {
            Ok(made) => made,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        made.as_ref().and_then(|r| r.as_ref().ok()).cloned()
    }

    /// The git panel the keyboard is in (02 §3.1 `in_git`): the one shown on the side opposite
    /// the active list.
    pub(super) fn git_focused(&self) -> Option<usize> {
        if !self.git.focus || self.viewer.is_some() {
            return None;
        }
        match self.shown.as_ref().map(|s| &s[1 - self.active]) {
            Some(view::Content::Git(top)) => self.git.panel(top),
            _ => None,
        }
    }

    /// The keyboard back to the origin list (Tab, Esc's unpin, F9 from an automatic panel).
    pub(super) fn git_leave(&mut self) {
        self.git.focus = false;
        self.git.wide = false;
        self.moved_at = None;
    }

    /// The git pass before the keys (02 §6): detections that answered (a detection answer
    /// commits at once), the active folder's asked on a worker, each panel's reads, pages,
    /// details and files heard, a hidden panel's stream ended after 30 s.
    pub(super) fn git_pass(&mut self, ctx: &egui::Context) {
        let mut answered = Vec::new();
        self.git.asking.retain(|folder, rx| match rx.try_recv() {
            Ok(d) => {
                answered.push((folder.clone(), d));
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        for (folder, detection) in answered {
            if folder == self.panels[self.active].dir {
                self.moved_at = None;
            }
            // 03 §2.3: why there is no panel, once per folder.
            if detection.on_status_row() && self.git.said.insert(folder.clone()) {
                let host = self.label.clone().unwrap_or_else(|| "this machine".into());
                if let Some(why) = detection.reason(&host) {
                    self.status = Status::error(why);
                }
            }
            self.git.detections.insert(folder, detection);
        }
        let folder = self.panels[self.active].dir.clone();
        let listing = self.listing.iter().any(|l| l.dir == folder);
        if self.git.connect.is_some()
            && !listing
            && !self.git.detections.contains_key(&folder)
            && !self.git.asking.contains_key(&folder)
        {
            self.ask_detection(ctx, folder);
        }
        for i in 0..self.git.panels.len() {
            self.poll_panel(ctx, i);
        }
        self.git_script(ctx);
        // A window that gains the focus soft-refreshes what it shows (03 §5).
        let focused = ctx.input(|i| i.viewport().focused);
        if let Some(now) = focused {
            if self.git.window_focused == Some(false) && now {
                for i in 0..self.git.panels.len() {
                    if self.git.panels[i].hidden_since.is_none() {
                        self.start_read(ctx, i, true, 0);
                    }
                }
            }
            self.git.window_focused = Some(now);
        }
    }

    /// Detection of `folder` on a worker (03 §2): the runner is made there the first time.
    pub(super) fn ask_detection(&mut self, ctx: &egui::Context, folder: String) {
        let Some(connect) = self.git.connect.clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let (cell, detector, host) = (
            self.git.runner.clone(),
            self.git.detector.clone(),
            self.git.host.clone(),
        );
        let (waker, path) = (ctx.clone(), folder.clone());
        std::thread::spawn(move || {
            // The connect (`ssh -G`, finding git) runs outside the lock; the first result stored
            // wins, a second worker's is dropped.
            let known = cell.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let made = match known {
                Some(made) => made,
                None => {
                    let fresh = connect();
                    let mut cell = cell.lock().unwrap_or_else(|p| p.into_inner());
                    cell.get_or_insert(fresh).clone()
                }
            };
            let detection = match made {
                Ok(runner) => detector.detect(&host, &path, &RunnerGit::new(runner, Handle::new())),
                Err(why) => Detection::Unavailable(why),
            };
            let _ = tx.send(detection);
            waker.request_repaint();
        });
        self.git.asking.insert(folder, rx);
    }

    /// After the sides are resolved: each git panel shown exists (made and loaded the first
    /// time), one shown again is refreshed (03 §5) or its stream started again (03 §4.5); one
    /// hidden for 30 s has its stream ended; at most [`KEPT_PANELS`] are kept.
    pub(super) fn git_follow(&mut self, ctx: &egui::Context) {
        let shown: Vec<String> = self
            .shown
            .iter()
            .flatten()
            .filter_map(|c| match c {
                view::Content::Git(top) => Some(top.clone()),
                _ => None,
            })
            .collect();
        let now = std::time::Instant::now();
        for top in &shown {
            let i = match self.git.panel(top) {
                Some(i) => i,
                None => self.make_panel(ctx, top.clone()),
            };
            self.git.shown_counter += 1;
            let counter = self.git.shown_counter;
            let p = &mut self.git.panels[i];
            p.last_shown = counter;
            if p.hidden_since.take().is_some() {
                if p.stream_dropped {
                    let pages = p.view.rows().len().div_ceil(history::PAGE).max(1);
                    self.start_read(ctx, i, false, pages);
                } else if p.refreshed.is_none_or(|t| t.elapsed() >= REFRESH_AGAIN) {
                    self.start_read(ctx, i, true, 0);
                }
            }
        }
        let hidden_stream = self.git.hidden_stream;
        for p in self.git.panels.iter_mut() {
            if shown.contains(&p.top) {
                continue;
            }
            let since = *p.hidden_since.get_or_insert(now);
            if since.elapsed() < hidden_stream {
                continue;
            }
            if p.stream.as_ref().is_some_and(|s| !s.ended) {
                p.stream = None;
                p.stream_dropped = true;
            }
            // A history search's stream has the same deadline, whatever the graph's did: its
            // results stay, and ↓ on their last row (or the foot) reads them again.
            if let Some(search) = p.search.take_if(|s| !s.link.ended) {
                let paging = Paging::Error("stopped while hidden; ↓ reads it again".into());
                p.view
                    .append_search_results(&search.query, Vec::new(), paging);
                p.search_dropped = Some(search.query);
            }
        }
        // The least recently shown panel goes when a fourth is kept (its running reads stop).
        while self.git.panels.len() > KEPT_PANELS {
            let pinned = self.git.pin.as_ref().map(|p| p.repo.clone());
            let drop = self
                .git
                .panels
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    !shown.contains(&p.top)
                        && Some(&p.top) != pinned.as_ref()
                        && p.running.is_empty()
                })
                .min_by_key(|(_, p)| p.last_shown)
                .map(|(i, _)| i);
            match drop {
                Some(i) => {
                    self.git.panels.remove(i);
                }
                None => break,
            }
        }
        let hidden_any = self.git.panels.iter().any(|p| p.hidden_since.is_some());
        if hidden_any {
            // An idle window still ends a hidden stream on time.
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// A new panel for the repository at `top`, its load started; its index.
    pub(super) fn make_panel(&mut self, ctx: &egui::Context, top: String) -> usize {
        let detection = self
            .git
            .detections
            .values()
            .find(|d| matches!(d, Detection::WorkTree { top: t, .. } if *t == top));
        let notes = detection.map(Detection::title_notes).unwrap_or_default();
        let shallow = matches!(detection, Some(Detection::WorkTree { shallow: true, .. }));
        let git_dir = match detection {
            Some(Detection::WorkTree { git_dir, .. }) => git_dir.clone(),
            _ => format!("{}/.git", top.trim_end_matches('/')),
        };
        let title = format!("{}{notes}", show(self.label.as_deref(), &top));
        self.git.panels.push(RepoPanel {
            top,
            git_dir,
            shallow,
            view: GitView::new(title),
            generation: Generation::new(),
            snapshot: None,
            read: None,
            stream: None,
            stream_dropped: false,
            search: None,
            search_dropped: None,
            details: None,
            last_details: None,
            reveal: None,
            resolving: None,
            hidden_since: None,
            refreshed: None,
            last_shown: 0,
            running: Vec::new(),
            prefill: None,
            inspect: None,
            failed_reads: None,
            checked: None,
            backups_read: None,
            restore_read: None,
            text_read: None,
            shown: Vec::new(),
        });
        let i = self.git.panels.len() - 1;
        self.start_read(ctx, i, false, 1);
        i
    }

    /// The reads of a load (03 §4.1) on a worker: `soft` keeps the rows when the refs, HEAD and
    /// stashes did not move (03 §5); otherwise a new stream reads `pages` pages (at least one).
    pub(super) fn start_read(&mut self, ctx: &egui::Context, i: usize, soft: bool, pages: usize) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        let paths = p.snapshot.as_ref().map(|s| s.paths.clone());
        std::thread::spawn(move || {
            let _ = tx.send(read_snapshot(&git, &top, paths));
            waker.request_repaint();
        });
        // A newer read replaces an older one (its handle stops what it still runs).
        p.read = Some(SnapshotRead {
            soft: soft && p.snapshot.is_some() && p.stream.is_some(),
            pages: pages.max(1),
            pending: Pending { rx, handle },
        });
        p.refreshed = Some(std::time::Instant::now());
        if !soft && p.view.rows().is_empty() {
            p.view.set_paging(Paging::Loading);
        }
    }

    /// Everything panel `i` waits for, heard.
    pub(super) fn poll_panel(&mut self, ctx: &egui::Context, i: usize) {
        self.poll_actions(ctx, i);
        // The load's reads.
        let read = self.git.panels[i]
            .read
            .as_ref()
            .and_then(|r| r.pending.rx.try_recv().ok());
        if let Some(result) = read {
            let Some(read) = self.git.panels[i].read.take() else {
                return;
            };
            match result {
                Ok(snapshot) => self.snapshot_read(ctx, i, snapshot, read.soft, read.pages),
                Err(why) => {
                    let p = &mut self.git.panels[i];
                    p.view.status = Some(Status::error(why.clone()));
                    if p.view.rows().is_empty() {
                        p.view.set_paging(Paging::Error(why));
                    }
                }
            }
        }
        // The stream's pages.
        loop {
            let p = &mut self.git.panels[i];
            let Some(link) = p.stream.as_mut() else {
                break;
            };
            let Ok(page) = link.pages.try_recv() else {
                break;
            };
            link.asked = link.asked.saturating_sub(1);
            if page.generation != link.generation || page.end == PageEnd::Superseded {
                continue;
            }
            let n = p.snapshot.as_ref().map_or(0, |s| s.status.entries.len());
            let rows = link.rows.page(page.rows);
            if std::mem::take(&mut link.first) {
                let first_parent = p.view.filters().first_parent;
                p.view.set_rows(rows, first_parent, n);
            } else {
                p.view.append_rows(rows);
            }
            if page.end != PageEnd::More {
                link.ended = true;
                link.asked = 0;
            }
            p.view.set_paging(paging_of(&page.end, p.shallow));
        }
        // A history search's pages.
        loop {
            let p = &mut self.git.panels[i];
            let Some(search) = p.search.as_mut() else {
                break;
            };
            let Ok(page) = search.link.pages.try_recv() else {
                break;
            };
            let link = &mut search.link;
            link.asked = link.asked.saturating_sub(1);
            if page.end != PageEnd::More {
                link.ended = true;
                link.asked = 0;
            }
            let paging = paging_of(&page.end, false);
            if std::mem::take(&mut link.first) {
                p.view.set_search_results(&search.query, page.rows, paging);
            } else {
                p.view
                    .append_search_results(&search.query, page.rows, paging);
            }
        }
        if self.git.panels[i]
            .search
            .as_ref()
            .is_some_and(|s| self.git.panels[i].view.search() != Some(&s.query))
        {
            // The search was left (Esc back to the graph): its stream goes.
            self.git.panels[i].search = None;
        }
        // Details.
        let p = &mut self.git.panels[i];
        if let Some(pending) = &p.details {
            loop {
                match pending.rx.try_recv() {
                    Ok(details) => {
                        p.last_details = Some(details.clone());
                        p.view.set_details(details);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        p.details = None;
                        break;
                    }
                }
            }
        }
        // A file row's diff or file.
        let revealed = p.reveal.as_ref().and_then(|r| r.rx.try_recv().ok());
        if let Some(revealed) = revealed {
            p.reveal = None;
            self.revealed(ctx, revealed);
        }
        // `select=REV` resolved.
        let p = &mut self.git.panels[i];
        let resolved = p.resolving.as_ref().and_then(|r| r.rx.try_recv().ok());
        if let Some(resolved) = resolved {
            p.resolving = None;
            match resolved {
                Ok(oid) => {
                    // A stash's commit is its row's (`select=stash@{0}`).
                    let stash = p
                        .view
                        .rows()
                        .iter()
                        .any(|r| matches!(r, Row::Stash(s) if s.oid == oid));
                    p.view.select(match stash {
                        true => RowKey::Stash(oid),
                        false => RowKey::Commit(oid),
                    });
                }
                Err(why) => {
                    p.view.status = Some(Status::error(why));
                    self.git.script = GitScript::default();
                }
            }
        }
    }

    /// The load's refs, HEAD, stashes, status and remotes came: the labels and the foot now; the
    /// rows kept (a soft refresh with the graph unchanged, the uncommitted row put in or taken
    /// out) or a new stream started.
    pub(super) fn snapshot_read(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        snapshot: Snapshot,
        soft: bool,
        pages: usize,
    ) {
        let version = self.git.detector.version(&self.git.host);
        let p = &mut self.git.panels[i];
        p.view.set_remotes(snapshot.remotes.clone());
        p.view
            .set_refs(snapshot.refs.clone(), head_ref(&snapshot.head));
        p.view.tracking = tracking(&snapshot.status);
        // The actions' snapshot (S3-A's `Repo`): this refresh's reads, no read beyond them.
        p.view.set_repo(Repo {
            top: p.top.clone(),
            git_dir: p.git_dir.clone(),
            version: version.unwrap_or_else(|| GitVersion {
                major: 2,
                minor: 20,
                patch: 0,
                text: "2.20 (not known)".into(),
            }),
            head: snapshot.head.clone(),
            refs: snapshot.refs.clone(),
            stashes: snapshot.stashes.clone(),
            status: snapshot.status.clone(),
            in_progress: snapshot.in_progress.clone(),
            remotes: snapshot.remotes.clone(),
            seen: Seen::default(),
        });
        let top = p.top.clone();
        p.view.set_undo(
            self.git
                .undo
                .get(&top)
                .and_then(|u| u.newest().map(String::from)),
        );
        let p = &mut self.git.panels[i];
        // Show's backups stay until the next refresh (03 §8.2).
        let showed = !std::mem::take(&mut p.shown).is_empty();
        // The details pane of the uncommitted row follows the status (a file staged).
        if p.view.pane_subject() == Some(&Subject::Row(RowKey::Uncommitted)) && p.details.is_none()
        {
            self.ask_details(ctx, i, Subject::Row(RowKey::Uncommitted));
        }
        let p = &mut self.git.panels[i];
        let keep = !showed
            && soft
            && p.stream.is_some()
            && p.snapshot
                .as_ref()
                .is_some_and(|old| old.same_graph(&snapshot));
        let was = p.snapshot.replace(snapshot);
        if !keep {
            // 03 §4.5: a refresh that moved the graph reads as many pages as were loaded.
            let loaded = p.view.rows().len().div_ceil(history::PAGE);
            self.start_stream(ctx, i, pages.max(loaded));
            return;
        }
        // 03 §5: the rows stay; the uncommitted row and its count follow the status.
        let p = &mut self.git.panels[i];
        let Some(now) = p.snapshot.as_ref() else {
            return;
        };
        let n = now.status.entries.len();
        let had = was.as_ref().map_or(0, |w| w.status.entries.len());
        if n != had {
            let mut rows = p.view.rows().to_vec();
            let first_is = matches!(rows.first(), Some(Row::Uncommitted { .. }));
            if n == 0 && first_is {
                rows.remove(0);
            } else if n > 0 && !first_is {
                rows.insert(
                    0,
                    Row::Uncommitted {
                        head: now.head.oid().cloned(),
                    },
                );
            }
            let first_parent = p.view.filters().first_parent;
            p.view.set_rows(rows, first_parent, n);
        }
    }

    /// A new log stream for panel `i` (03 §4.5), reading `pages` pages: the old one ends, and a
    /// hidden panel's parked stream gives way when the host has [`PARKED_STREAMS`] already.
    pub(super) fn start_stream(&mut self, ctx: &egui::Context, i: usize, pages: usize) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        self.git.panels[i].stream = None;
        self.git.panels[i].stream_dropped = false;
        self.make_room_for_a_stream(i);
        let p = &mut self.git.panels[i];
        let Some(snapshot) = p.snapshot.clone() else {
            return;
        };
        let f = p.view.filters().clone();
        let mut rows = RowStream::new(
            snapshot.head.oid().cloned(),
            !snapshot.status.entries.is_empty(),
            snapshot.stashes.clone(),
            f.stashes,
        );
        let mut roots = roots_of(&f, &snapshot);
        for oid in &p.shown {
            if !roots.contains(oid) {
                roots.push(oid.clone());
            }
        }
        if roots.is_empty() {
            // 03 §4.1: no id is ever sent — git would take HEAD.
            let n = snapshot.status.entries.len();
            p.view.set_rows(rows.page(Vec::new()), f.first_parent, n);
            let nothing = snapshot.refs.is_empty() && snapshot.head.oid().is_none();
            p.view.set_paging(if nothing {
                Paging::NoCommits
            } else {
                Paging::NoRefsChosen
            });
            return;
        }
        let g = p.generation.bump();
        let handle = Handle::new();
        let link = spawn_stream(
            ctx,
            RunnerGit::new(runner, handle.clone()),
            p.top.clone(),
            history::argv::log(&log_options(&f)),
            roots,
            p.generation.clone(),
            g,
        );
        let mut link = StreamLink {
            generation: g,
            handle,
            ask: link.0,
            pages: link.1,
            asked: 0,
            rows,
            first: true,
            ended: false,
        };
        if link.ask.send(pages).is_ok() {
            link.asked = pages;
        }
        p.stream = Some(link);
        if p.view.rows().is_empty() {
            p.view.set_paging(Paging::Loading);
        }
    }

    /// At most [`PARKED_STREAMS`] streams per host: the least recently shown hidden panel's ends
    /// first (it reads its pages again when it is shown).
    pub(super) fn make_room_for_a_stream(&mut self, for_panel: usize) {
        loop {
            let live = self
                .git
                .panels
                .iter()
                .map(|p| {
                    usize::from(p.stream.as_ref().is_some_and(|s| !s.ended))
                        + usize::from(p.search.as_ref().is_some_and(|s| !s.link.ended))
                })
                .sum::<usize>();
            if live < PARKED_STREAMS {
                return;
            }
            let victim = self
                .git
                .panels
                .iter()
                .enumerate()
                .filter(|(i, p)| {
                    *i != for_panel
                        && p.hidden_since.is_some()
                        && (p.stream.as_ref().is_some_and(|s| !s.ended) || p.search.is_some())
                })
                .min_by_key(|(_, p)| p.last_shown)
                .map(|(i, _)| i);
            let Some(v) = victim else {
                return;
            };
            let p = &mut self.git.panels[v];
            p.stream = None;
            p.search = None;
            p.stream_dropped = true;
        }
    }

    /// The next page of panel `i`'s stream (03 §4.5); a stream that ended on an error or was
    /// dropped is started again, reading as many pages as were loaded and one more.
    pub(super) fn more_rows(&mut self, ctx: &egui::Context, i: usize) {
        let p = &mut self.git.panels[i];
        let retry = matches!(p.view.paging(), Paging::Error(_)) || p.stream_dropped;
        match p.stream.as_mut() {
            Some(link) if !link.ended && !retry => {
                if link.asked == 0 && link.ask.send(1).is_ok() {
                    link.asked = 1;
                }
            }
            _ if retry => {
                let pages = p.view.rows().len().div_ceil(history::PAGE) + 1;
                self.start_read(ctx, i, false, pages);
            }
            _ => {}
        }
    }

    /// Esc in the git panel, before anything that arrived is heard (03 §3.4: reads stop on Esc):
    /// what the panel waits for — a page, a history search's page, a load's reads, a file row's
    /// diff or file — is stopped, and a file row's result is dropped unread, so a diff whose git
    /// had already ended opens no viewer. Only with nothing waited for is Esc the panel's chain.
    pub(super) fn git_stop_on_esc(&mut self, ctx: &egui::Context) {
        if self.ops.holds_keyboard() || self.viewer.is_some() || self.tree.is_some() {
            return;
        }
        let Some(i) = self.git_focused() else {
            return;
        };
        // A dialog's own Esc (the menu, an action's dialog, a result, the filters', …) first.
        if self.git.panels[i].view.dialog_up() {
            return;
        }
        let waiting = git_waiting(&self.git.panels[i]);
        let running = !self.git.panels[i].running.is_empty();
        if (waiting.is_empty() && !running)
            || !ctx.input_mut(|inp| keys::take(inp, Key::Escape, keys::Mods::None))
        {
            return;
        }
        for handle in waiting {
            handle.stop();
        }
        let p = &mut self.git.panels[i];
        p.reveal = None;
        p.prefill = None;
        p.backups_read = None;
        p.restore_read = None;
        p.text_read = None;
        self.status = Status::info("stopped");
        // An action (03 §3.4): one waiting for the lock, or running a network command, stops at
        // once; a write after 10 s, with a question.
        let stop_after = self.git.stop_after;
        let Some(run) = p.running.first() else {
            return;
        };
        let ran = run.handle.elapsed().unwrap_or_else(|| run.since.elapsed());
        if !run.started.load(Ordering::Relaxed) || run.network_now() {
            run.handle.stop();
            self.status = Status::info(format!("{}: stopping", run.title));
        } else if ran >= stop_after {
            let title = run.title.clone();
            p.view.ask_stop(title);
            self.status = Status::default();
        } else {
            self.status = Status::info(format!(
                "{} is writing: Esc can stop it once it has run {} s",
                run.title,
                stop_after.as_secs()
            ));
        }
    }

    /// The git panel's keys (03 §9), through the key table's git context.
    pub(super) fn git_keys(&mut self, ctx: &egui::Context) {
        if self.ops.holds_keyboard() {
            return;
        }
        let Some(i) = self.git_focused() else {
            return;
        };
        // A key of the owner's: what was said before is old news.
        let pressed = ctx.input(|inp| {
            inp.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key { pressed: true, .. } | egui::Event::Text(_)
                )
            })
        });
        if pressed {
            self.status = Status::default();
        }
        let actions = git_panel::keys(ctx, &mut self.git.panels[i].view);
        self.git_actions(ctx, i, actions);
    }

    /// Carries out what panel `i` asks (S2-D's `GitAction`s).
    pub(super) fn git_actions(&mut self, ctx: &egui::Context, i: usize, actions: Vec<GitAction>) {
        for action in actions {
            if i >= self.git.panels.len() {
                return;
            }
            match action {
                GitAction::Help => {
                    help::open(ctx, help::for_context(Context::Git, self.mode, &[]));
                }
                GitAction::Details(subject) => self.ask_details(ctx, i, subject),
                GitAction::MoreRows => self.more_rows(ctx, i),
                GitAction::Search(query) => self.start_search(ctx, i, query),
                GitAction::MoreResults => {
                    let p = &mut self.git.panels[i];
                    if p.search.is_none() {
                        // Ended while hidden: run again, as many pages as were shown and one more.
                        if let Some(query) = p.search_dropped.take() {
                            if p.view.search() == Some(&query) {
                                let shown = p.view.search_rows().div_ceil(history::PAGE);
                                self.start_search_pages(ctx, i, query, shown + 1);
                                continue;
                            }
                        }
                    }
                    if let Some(s) = self.git.panels[i].search.as_mut() {
                        let link = &mut s.link;
                        if !link.ended && link.asked == 0 && link.ask.send(1).is_ok() {
                            link.asked = 1;
                        }
                    }
                }
                GitAction::FiltersChanged(_) => {
                    let pages = self.git.panels[i].view.rows().len().div_ceil(history::PAGE);
                    self.start_stream(ctx, i, pages.max(1));
                }
                GitAction::Diff {
                    subject,
                    group,
                    file,
                } => self.ask_reveal(ctx, i, subject, group, file, true),
                GitAction::RevisionFile {
                    subject,
                    group,
                    file,
                } => self.ask_reveal(ctx, i, subject, group, file, false),
                GitAction::ListFolder { subject, dir } => self.list_folder(ctx, i, subject, dir),
                GitAction::Edit(path) => {
                    let top = self.git.panels[i].top.clone();
                    match path {
                        None => self.edit(ctx, top, true),
                        Some(path) => {
                            let abs = format!("{}/{path}", top.trim_end_matches('/'));
                            self.edit(ctx, abs, false)
                        }
                    }
                }
                GitAction::OtherSide => self.git_leave(),
                GitAction::SwitchMode => {
                    // 02 §2: an automatic panel gives the keyboard back first; a pinned one
                    // keeps it (the pin outranks the mode).
                    let pinned = self
                        .git
                        .pin
                        .as_ref()
                        .is_some_and(|p| p.repo == self.git.panels[i].top);
                    if !pinned {
                        self.git_leave();
                    }
                    self.git.wide = false;
                    self.mode = self.mode.other();
                    self.moved_at = None;
                }
                GitAction::Refresh => self.hard_refresh(ctx, i),
                GitAction::Pin => {
                    if self.git.pin.take().is_some() {
                        self.git_leave();
                    } else {
                        self.git.pin = Some(view::Pin {
                            side: 1 - self.active,
                            repo: self.git.panels[i].top.clone(),
                        });
                    }
                    self.moved_at = None;
                }
                GitAction::Unpin => {
                    if self.git.pin.take().is_some() {
                        self.git_leave();
                    } else if self.git.wide {
                        self.git.wide = false;
                    }
                    self.moved_at = None;
                }
                GitAction::Wide => {
                    self.git.wide = !self.git.wide;
                    self.moved_at = None;
                }
                // S3 (03 §7–§8).
                GitAction::Open { id, target } => self.ask_prefill(ctx, i, id, target),
                GitAction::Run(action) => self.start_action(ctx, i, action),
                GitAction::Restore { stamp, only } => self.ask_restore(ctx, i, stamp, only),
                GitAction::Backups => self.ask_backups(ctx, i),
                GitAction::Log => {
                    if let Some(runner) = self.git_runner() {
                        self.git.panels[i].view.open_log(runner.log().entries());
                    }
                }
                GitAction::ShowBackup(refs) => self.show_backups(ctx, i, refs),
                GitAction::TagDetails(refname) => self.ask_tag_details(ctx, i, refname),
                GitAction::StopWrite => {
                    let p = &mut self.git.panels[i];
                    for run in &p.running {
                        run.handle.stop();
                    }
                    p.view.status = Some(Status::info("stopping git…"));
                }
            }
        }
    }

    /// Cmd/Ctrl+R in the git panel (03 §5): detection, git version and every read again.
    pub(super) fn hard_refresh(&mut self, ctx: &egui::Context, i: usize) {
        let top = self.git.panels[i].top.clone();
        let host = self.git.host.clone();
        let folders: Vec<String> = self
            .git
            .detections
            .iter()
            .filter(|(_, d)| matches!(d, Detection::WorkTree { top: t, .. } if *t == top))
            .map(|(f, _)| f.clone())
            .collect();
        self.git.detector.forget_host(&host);
        for folder in folders {
            self.git.detector.forget(&host, &folder);
            self.git.detections.remove(&folder);
            self.git.said.remove(&folder);
        }
        let pages = self.git.panels[i].view.rows().len().div_ceil(history::PAGE);
        self.start_read(ctx, i, false, pages.max(1));
    }

    /// The details of `subject` on a worker (03 §6.1, §6.4); a newer ask stops an older one.
    pub(super) fn ask_details(&mut self, ctx: &egui::Context, i: usize, subject: Subject) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let inputs = DetailsInputs {
            stashes: p
                .snapshot
                .as_ref()
                .map(|s| s.stashes.clone())
                .unwrap_or_default(),
            status: p
                .snapshot
                .as_ref()
                .map(|s| s.status.clone())
                .unwrap_or_else(|| GitStatus {
                    branch: Default::default(),
                    entries: Vec::new(),
                }),
        };
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        std::thread::spawn(move || {
            let tell = |d: Details| {
                let _ = tx.send(d);
                waker.request_repaint();
            };
            read_details(&git, &top, &subject, &inputs, &tell);
        });
        p.details = Some(Pending { rx, handle });
    }

    /// → or Enter on an untracked folder's row (03 §6.1): its files, read now, after its row.
    pub(super) fn list_folder(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        subject: Subject,
        dir: String,
    ) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let Some(mut details) = p.last_details.clone().filter(|d| d.subject == subject) else {
            return;
        };
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        std::thread::spawn(move || {
            match history::others(&git, &top, &dir) {
                Ok(files) => {
                    for group in details.groups.iter_mut() {
                        let Some(at) = group.files.iter().position(|f| f.path == dir) else {
                            continue;
                        };
                        let already = group
                            .files
                            .get(at + 1)
                            .is_some_and(|f| f.path.starts_with(dir.as_str()) && f.path != dir);
                        if already {
                            continue;
                        }
                        let rows = files.iter().map(|path| FileRow {
                            letter: '?',
                            path: path.clone(),
                            old_path: None,
                            lines: None,
                            lossy: false,
                        });
                        group.files.splice(at + 1..at + 1, rows);
                    }
                }
                Err(e) => details.error = Some(format!("{dir}: {e}")),
            }
            let _ = tx.send(details);
            waker.request_repaint();
        });
        p.details = Some(Pending { rx, handle });
    }

    /// Enter or Shift+F3 on a file row (03 §6.2, §6.3), on a worker.
    pub(super) fn ask_reveal(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        subject: Subject,
        group: Group,
        file: FileRow,
        diff: bool,
    ) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let top = p.top.clone();
        let shown_top = show(self.label.as_deref(), &top);
        let (space, cache, waker) = (self.git.space.clone(), self.cache.clone(), ctx.clone());
        let said = format!(
            "{} {}…",
            if diff { "diffing" } else { "reading" },
            file.path
        );
        std::thread::spawn(move || {
            let revealed = reveal(
                &git, &top, &shown_top, &subject, group, &file, diff, &space, &cache,
            );
            let _ = tx.send(revealed);
            waker.request_repaint();
        });
        p.reveal = Some(Pending { rx, handle });
        self.status = Status::info(said);
    }

    /// A file row's diff or file is ready: its viewer, read-only, over both sides (03 §6.2); or
    /// the working file as the list opens it; or why there is nothing.
    pub(super) fn revealed(&mut self, ctx: &egui::Context, revealed: Revealed) {
        if self.viewer.is_some() || self.opening.is_some() {
            return;
        }
        self.status = Status::default();
        match revealed {
            Revealed::File { file, title, edit } => {
                let name = file_name(&file.to_string_lossy()).to_string();
                let local: Connect =
                    Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>));
                self.start_open_with(
                    ctx,
                    file.to_string_lossy().into_owned(),
                    name,
                    local,
                    None,
                    Some(ReadOnly { title, edit }),
                );
            }
            Revealed::Working(abs) => {
                let name = file_name(&abs).to_string();
                let (connect, label) = (self.connect.clone(), self.label.clone());
                self.start_open_with(ctx, abs, name, connect, label, None);
            }
            Revealed::Refused(why) => self.status = Status::error(why),
        }
    }

    /// History search (03 §4.7): `git log` with the query's options and the panel's revisions,
    /// paged like the graph.
    pub(super) fn start_search(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        query: git_panel::SearchQuery,
    ) {
        self.start_search_pages(ctx, i, query, 1);
    }

    /// [`Browser::start_search`], reading `pages` pages at once.
    pub(super) fn start_search_pages(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        query: git_panel::SearchQuery,
        pages: usize,
    ) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        self.git.panels[i].search_dropped = None;
        self.git.panels[i].search = None;
        self.make_room_for_a_stream(i);
        let p = &mut self.git.panels[i];
        let Some(snapshot) = p.snapshot.clone() else {
            return;
        };
        let f = p.view.filters().clone();
        let roots = roots_of(&f, &snapshot);
        let q = history::argv::Query {
            message: query.message.clone(),
            author: query.author.clone(),
            text: query.text.clone(),
            path: query.path.clone(),
            since: query.since.clone(),
            until: query.until.clone(),
            case: query.case,
        };
        if roots.is_empty() {
            p.view
                .set_search_results(&query, Vec::new(), Paging::NoRefsChosen);
            return;
        }
        let generation = Generation::new();
        let g = generation.bump();
        let handle = Handle::new();
        let link = spawn_stream(
            ctx,
            RunnerGit::new(runner, handle.clone()),
            p.top.clone(),
            history::argv::search(&log_options(&f), &q),
            roots,
            generation,
            g,
        );
        let mut link = StreamLink {
            generation: g,
            handle,
            ask: link.0,
            pages: link.1,
            asked: 0,
            rows: RowStream::new(None, false, Vec::new(), false),
            first: true,
            ended: false,
        };
        if link.ask.send(pages).is_ok() {
            link.asked = pages;
        }
        p.search = Some(SearchLink { query, link });
    }

    /// `SOPEN_STATE`'s git keys (02 §9), each once its panel is there: pin (the keyboard goes
    /// in), `select=REV` (resolved, then the bar on it once its page came — more pages asked
    /// for until it did), `label=N`, `details=1`.
    pub(super) fn git_script(&mut self, ctx: &egui::Context) {
        if self.git.script.done() {
            return;
        }
        if self.git.script.pin {
            let folder = self.panels[self.active].dir.clone();
            match self.git.detections.get(&folder) {
                Some(Detection::WorkTree { top, .. }) => {
                    self.git.pin = Some(view::Pin {
                        side: 1 - self.active,
                        repo: top.clone(),
                    });
                    self.git.focus = true;
                    self.git.script.pin = false;
                    self.moved_at = None;
                }
                Some(other) => {
                    let host = self.label.clone().unwrap_or_else(|| "this machine".into());
                    self.status = Status::error(format!(
                        "SOPEN_STATE git=pin: {}",
                        other
                            .reason(&host)
                            .unwrap_or_else(|| "not in a git work tree".into())
                    ));
                    self.git.script = GitScript::default();
                }
                None => {}
            }
            return;
        }
        let Some(i) = self.git.pin.as_ref().and_then(|p| self.git.panel(&p.repo)) else {
            return;
        };
        let loaded = {
            let p = &self.git.panels[i];
            p.read.is_none() && p.stream.as_ref().is_some_and(|s| s.asked == 0)
                || !matches!(p.view.paging(), Paging::Loading)
                    && p.read.is_none()
                    && p.stream.is_none()
        };
        if !loaded {
            return;
        }
        if let Some(rev) = self.git.script.select.take() {
            let Some(runner) = self.git_runner() else {
                return;
            };
            let p = &mut self.git.panels[i];
            let handle = Handle::new();
            let git = RunnerGit::new(runner, handle.clone());
            let (tx, rx) = mpsc::channel();
            let (top, waker) = (p.top.clone(), ctx.clone());
            std::thread::spawn(move || {
                let spec = format!("{rev}^{{commit}}");
                let args = vec!["rev-parse".into(), "-q".into(), "--verify".into(), spec];
                let found = match git.run(&top, &args, b"", history::Bound::Read) {
                    Ok(ran) if ran.code == Some(0) => {
                        Oid::parse(String::from_utf8_lossy(&ran.stdout).trim())
                            .ok_or_else(|| format!("SOPEN_STATE select={rev}: not a commit"))
                    }
                    Ok(_) => Err(format!(
                        "SOPEN_STATE select={rev}: git knows no such commit"
                    )),
                    Err(e) => Err(format!("SOPEN_STATE select={rev}: {e}")),
                };
                let _ = tx.send(found);
                waker.request_repaint();
            });
            p.resolving = Some(Pending { rx, handle });
            return;
        }
        let p = &mut self.git.panels[i];
        if p.resolving.is_some() {
            return;
        }
        if p.view.selecting().is_some() {
            // Not loaded yet: the next page, until it comes or the history ends.
            match p.view.paging() {
                Paging::More(_) => self.more_rows(ctx, i),
                _ => {
                    self.status =
                        Status::error("SOPEN_STATE select: that commit is not in the graph");
                    self.git.script = GitScript::default();
                }
            }
            return;
        }
        if let Some(n) = self.git.script.label.take() {
            p.view.set_label_cursor(n);
        }
        if std::mem::take(&mut self.git.script.details) {
            let actions = p.view.open_details();
            self.git_actions(ctx, i, actions);
        }
        // `dialog=<id>` (02 §9, S3): that action's dialog, a confirmation's receipt.
        if let Some(id) = self.git.script.dialog.take() {
            let asked = self.git.panels[i].view.open_by_id(id);
            self.git_actions(ctx, i, asked);
        }
    }

    /// The git panel shown on side `side` in `rect` (the keyboard's when `active`), its clicks
    /// carried out; a press on it gives it the keyboard.
    pub(super) fn git_ui(&mut self, ui: &mut egui::Ui, side: usize, rect: Rect, top: &str) {
        let Some(i) = self.git.panel(top) else {
            return;
        };
        let pressed = ui.input(|inp| {
            inp.pointer.any_pressed()
                && inp
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| rect.contains(pos))
        });
        if pressed && !self.ops.holds_keyboard() && self.tree.is_none() && self.listing.is_empty() {
            if side != 1 - self.active {
                // The other side's list had the keyboard: it is the origin list now.
                self.active = 1 - side;
            }
            self.git.focus = true;
        }
        let active = self.git.focus && side == 1 - self.active && self.viewer.is_none();
        let mut actions = git_panel::draw(ui, rect, &mut self.git.panels[i].view, active);
        if self.ops.holds_keyboard() || self.tree.is_some() {
            actions.clear();
        }
        let ctx = ui.ctx().clone();
        self.git_actions(&ctx, i, actions);
    }
}

// ---------------------------------------------------------------------------------------------
// S3 (S3-C): the actions — a dialog's prefill and its reads beyond the refresh, the run on a
// worker under the repository's write lock (02 §6), the refresh after it (03 §5),
// `GitEvent::WorkTreeChanged`, the window's undo (03 §8.3), the Backups list, the command log, a
// tag's details and Show.

/// What a git write tells the browser (02 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitEvent {
    /// A write that can change files ended (`Plan::changes_files`): the panels showing a folder
    /// inside `top` re-list, their bar kept on its name (the git panel refreshes after every
    /// action anyway).
    WorkTreeChanged { host: Option<String>, top: String },
}

/// How a run on the worker ended.
pub(super) enum RunEnd {
    /// The plan ran: each step's outcome, the status row's words, whether files may have changed,
    /// and whether the Commit dialog follows (a merge `--squash` that did not say "don't commit").
    Ran {
        outcome: Box<Outcome>,
        title: String,
        what: String,
        changes_files: bool,
        squash: bool,
        /// The action that ran (`delete-branch`'s second dialog is the same action, forced).
        action: Box<Action>,
    },
    /// Nothing ran: the plan refused at Run, or a read it needed failed.
    NotRun {
        title: String,
        why: String,
        line: Option<String>,
    },
    /// Nothing ran: no confirmation showed this run's plan (it was lost, or the action came with
    /// none) and the plan made now asks for one (03 §8.1) — its dialog opens again (review 1).
    Confirm { title: String, action: Box<Action> },
}

/// A backup stamp's order (03 §8.2: `YYYYMMDDTHHMMSSZ`, then `-2`, `-3`… when taken): its UTC
/// time, then its number (none is the first).
fn stamp_order(stamp: &str) -> (String, u64) {
    match stamp.split_once('-') {
        Some((base, n)) => (base.to_string(), n.parse().unwrap_or(u64::MAX)),
        None => (stamp.to_string(), 1),
    }
}

/// A group this window wrote, onto its undo (03 §8.3): kept in stamp order, so the newest is the
/// newest written whatever order the runs ended in (review 1).
pub(super) fn record_group(undo: &mut backup::Undo, stamp: String) {
    if !undo.stamps.contains(&stamp) {
        undo.stamps.push(stamp);
    }
    undo.stamps.sort_by_key(|s| stamp_order(s));
}

/// An action running on a worker, or waiting for the repository's write lock (02 §6). Never a
/// [`Pending`]: dropping it does not stop the write — a closing window waits for it and prints
/// its result ([`wait_run`]), and a panel with one is never dropped.
pub(super) struct Running {
    /// The action's words (its confirmation's title, else its menu label).
    pub(super) title: String,
    pub(super) handle: Handle,
    pub(super) rx: Receiver<RunEnd>,
    /// The worker holds the write lock: it runs (until then it waits in line, 03 §3.4).
    pub(super) started: Arc<AtomicBool>,
    pub(super) since: std::time::Instant,
    /// The plan's network command lines (03 §3.4: Esc stops them at once).
    pub(super) network: Vec<String>,
    pub(super) worker: Option<JoinHandle<()>>,
}

impl Running {
    /// The command running now is a network one (or a read): Esc stops it at once (03 §3.4).
    pub(super) fn network_now(&self) -> bool {
        let Some(line) = self.handle.command_line() else {
            return false;
        };
        let words = ["git fetch", "git push", "git ls-remote"];
        self.network.contains(&line) || words.iter().any(|w| line.starts_with(w))
    }
}

/// A dialog's prefill as read ([`GitAction::Open`]).
pub(super) struct Prefilled {
    pub(super) id: ActionId,
    pub(super) target: git_panel::Target,
    pub(super) read: std::result::Result<git_panel::Prefill, String>,
}

/// A dialog's reads as `actions::inspect` made them: those that came (a failed one is never
/// among them), the reads asked for, and the failure that ended the round.
pub(super) struct Inspected {
    pub(super) seen: Seen,
    pub(super) asked: Vec<actions::Need>,
    pub(super) failed: Option<backup::Failed>,
}

/// A restore's entries as read (03 §8.3): the group's stamp, the entries, the ref asked for.
pub(super) type RestoreRead =
    std::result::Result<(String, Vec<RestoreEntry>, Option<String>), String>;

/// A run that outlives its window (02 §6): waited for after the window, its result printed.
pub(super) fn wait_run(mut run: Running) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let said = match run.rx.recv() {
            Ok(RunEnd::Ran { outcome, title, .. }) => {
                let failed = outcome
                    .steps
                    .iter()
                    .find(|s| s.outcome == actions::StepOutcome::Failed);
                match (failed, &outcome.backup) {
                    (Some(step), _) => format!("{title}: failed at {}", step.line),
                    (None, Some(stamp)) => format!("{title}: done · backup {stamp}"),
                    (None, None) => format!("{title}: done"),
                }
            }
            Ok(RunEnd::NotRun { title, why, .. }) => format!("{title}: not run: {why}"),
            Ok(RunEnd::Confirm { title, .. }) => {
                format!("{title}: not run: its plan asks for a confirmation")
            }
            Err(_) => format!("{}: the run stopped without a result", run.title),
        };
        eprintln!(
            "sopen: git: {}",
            crate::git::runner::mask_credentials(&said)
        );
        if let Some(worker) = run.worker.take() {
            let _ = worker.join();
        }
    })
}

/// `sopen-read <path>`: the file's bytes, nothing when there is none (git's prepared squash
/// message). A fixed sh snippet; it runs no git.
static READ_FILE: crate::git::runner::Snippet = crate::git::runner::Snippet {
    name: "sopen-read",
    script: r#"case $1 in /*) p=$1;; *) p=./$1;; esac; if [ -f "$p" ]; then cat -- "$p"; fi; exit 0"#,
};

/// Plans `action` against `repo` when no confirmation showed its plan: `needs` and `inspect`
/// until it has its reads (S3-A's loop, a few rounds at most), then `plan`.
fn plan_now(
    runner: &GitRunner,
    handle: &Handle,
    mut repo: Repo,
    action: &Action,
) -> std::result::Result<(Plan, Repo), (String, Option<String>)> {
    for _ in 0..8 {
        let missing = actions::needs(action, &repo);
        if missing.is_empty() {
            return match actions::plan(action, &repo) {
                Ok(plan) => Ok((plan, repo)),
                Err(refused) => Err((refused.0, None)),
            };
        }
        if let Err(failed) = actions::inspect(runner, handle, &mut repo, &missing) {
            let line = failed.ran.last().map(|d| d.entry.line.clone());
            return Err((failed.reason, line));
        }
    }
    Err(("the reads its plan needs did not settle".into(), None))
}

/// The exit codes a read of the integration's own takes as an answer — every other ending is a
/// failure, never an empty answer: 0; and for `config` (`--get`, `--get-regexp`) also 1, git's
/// "not set", whose answer is its empty stdout. (The plans' reads go through S3-A's
/// `actions::inspect`, which names its own the same way and records only those.)
fn accepted_exits(args: &[&str]) -> &'static [i32] {
    match args.first() {
        Some(&"config") => &[0, 1],
        _ => &[0],
    }
}

/// One read for a prefill: git's stdout when it ended with an accepted code
/// ([`accepted_exits`]), else the failure.
fn read_value(git: &RunnerGit, top: &str, args: &[&str]) -> std::result::Result<String, String> {
    let accepted = accepted_exits(args);
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    match git.run(top, &args, b"", history::Bound::Read) {
        Ok(ran) if ran.code.is_some_and(|c| accepted.contains(&c)) => {
            Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
        }
        Ok(ran) => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&ran.stderr).trim()
        )),
        Err(e) => Err(format!("git {}: {e}", args.join(" "))),
    }
}

/// What `id`'s dialog starts from (03 §7; [`git_panel::prefilled`]), read on a worker.
fn read_prefill(
    git: &RunnerGit,
    top: &str,
    id: ActionId,
    target: &git_panel::Target,
    remotes: &[String],
) -> std::result::Result<git_panel::Prefill, String> {
    use ActionId as A;
    let mut prefill = git_panel::Prefill::default();
    let one_line = |s: String| s.lines().next().unwrap_or("").to_string();
    match id {
        A::Archive => prefill.save_dir = crate::gui::binary::download_dir(),
        // The last commit's message as git prints it (`%B`; the record's own newline taken off).
        A::Amend => {
            let mut message = read_value(git, top, &["log", "-1", "--format=%B", "HEAD"])?;
            if message.ends_with('\n') {
                message.pop();
            }
            prefill.message = Some(message);
        }
        // git's prepared squash message, after `merge --squash` (03 §7.1).
        A::Commit => {
            let path = one_line(read_value(
                git,
                top,
                &["rev-parse", "--git-path", "SQUASH_MSG"],
            )?);
            if !path.is_empty() {
                let cmd = GitCommand::snippet(Class::Read, top, &READ_FILE, [path.as_str()]);
                let done = git.runner.run(&cmd, &git.handle);
                if !done.ok() {
                    return Err(done.entry.status_line());
                }
                let message = String::from_utf8_lossy(&done.stdout).into_owned();
                prefill.message = (!message.is_empty()).then_some(message);
            }
        }
        // The annotated tags' messages (03 §7.4), by name.
        A::MoveTag => {
            let format = "--format=%(objecttype) %(refname)%00%(contents)%00";
            let out = read_value(git, top, &["for-each-ref", format, "refs/tags"])?;
            for record in out.split("\0\n").filter(|r| !r.is_empty()) {
                let Some((head, message)) = record.split_once('\0') else {
                    continue;
                };
                if let Some(refname) = head.strip_prefix("tag ") {
                    let name = refname.strip_prefix("refs/tags/").unwrap_or(refname);
                    prefill
                        .tag_messages
                        .insert(name.to_string(), message.to_string());
                }
            }
        }
        // The remotes' URLs, masked (03 §3.7: a dialog never holds a password).
        A::EditRemote => {
            let out = read_value(git, top, &["config", "-z", "--get-regexp", r"^remote\."])?;
            let pairs: Vec<(&str, &str)> =
                out.split('\0').filter_map(|r| r.split_once('\n')).collect();
            let value = |key: String| {
                pairs
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(&key))
                    .map(|(_, v)| crate::git::runner::mask_credentials(v))
            };
            prefill.remotes = remotes
                .iter()
                .map(|name| actions::Remote {
                    name: name.clone(),
                    url: value(format!("remote.{name}.url")).unwrap_or_default(),
                    push_url: value(format!("remote.{name}.pushurl")),
                })
                .collect();
        }
        // This repository's name and email, else the global ones (03 §7.7).
        A::User => {
            let get = |scope: &str, key: &str| {
                read_value(git, top, &["config", scope, "--get", key]).map(one_line)
            };
            let (name, email) = (get("--local", "user.name")?, get("--local", "user.email")?);
            prefill.user = if !name.is_empty() || !email.is_empty() {
                Some((name, email, actions::ConfigScope::Local))
            } else {
                let (name, email) = (
                    get("--global", "user.name")?,
                    get("--global", "user.email")?,
                );
                (!name.is_empty() || !email.is_empty()).then_some((
                    name,
                    email,
                    actions::ConfigScope::Global,
                ))
            };
        }
        // The branch's push remote, else `remote.pushDefault` (03 §7.2).
        A::Push => {
            let branch = match target {
                git_panel::Target::Label(r) => r.name().to_string(),
                _ => String::new(),
            };
            let key = format!("branch.{branch}.pushRemote");
            let mut remote = one_line(read_value(git, top, &["config", "--get", &key])?);
            if remote.is_empty() {
                remote = one_line(read_value(
                    git,
                    top,
                    &["config", "--get", "remote.pushDefault"],
                )?);
            }
            prefill.push_remote = (!remote.is_empty()).then_some(remote);
        }
        _ => {}
    }
    Ok(prefill)
}

impl Browser {
    /// The status row while panel `i` runs an action (03 §3.4, §3.6): what runs and for how long,
    /// a network command's newest progress line, "no output for N s — … may be waiting for you"
    /// after 10 s of silence; or that it waits for the repository's other write.
    pub(super) fn git_running_status(&self, i: usize) -> Option<Status> {
        let run = self.git.panels.get(i)?.running.first()?;
        if !run.started.load(Ordering::Relaxed) {
            return Some(Status::info(format!(
                "{}: waits for the other write to end (one at a time per repository)",
                run.title
            )));
        }
        let mut line = format!("{} · {} s", run.title, run.since.elapsed().as_secs());
        if let Some(progress) = run.handle.progress().filter(|p| !p.is_empty()) {
            line.push_str(&format!(" · {progress}"));
        }
        if let Some(quiet) = run.handle.silent_for() {
            if quiet >= self.git.quiet_after {
                line.push_str(&format!(
                    " · no output for {} s — a hook, a signer or a credential helper may be waiting for you; Esc stops",
                    quiet.as_secs()
                ));
            }
        }
        // 03 §3.4: a second action waits in line, and the status row says so.
        let waiting: Vec<&str> = self.git.panels[i].running[1..]
            .iter()
            .map(|r| r.title.as_str())
            .collect();
        if !waiting.is_empty() {
            line.push_str(&format!(
                " · {} waits for the other write to end (one at a time)",
                waiting.join(", ")
            ));
        }
        Some(Status::info(line))
    }

    /// What a git write tells the browser (02 §7).
    pub(super) fn git_event(&mut self, ctx: &egui::Context, event: GitEvent) {
        match event {
            GitEvent::WorkTreeChanged { host, top } => {
                if host != self.label {
                    return;
                }
                let under = format!("{}/", top.trim_end_matches('/'));
                for i in 0..2 {
                    let panel = &self.panels[i];
                    if panel.dir != top && !panel.dir.starts_with(&under) {
                        continue;
                    }
                    let name = panel.current().map(|e| e.name.clone());
                    let (dir, at) = (panel.dir.clone(), panel.highlight);
                    self.relist(Some(i), dir, name, Some(at), Some(ctx));
                }
            }
        }
    }

    /// Runs `action` of panel `i` on a worker (02 §6): it waits for the repository's write lock,
    /// plans when no confirmation showed its plan (else runs exactly that plan, S3-A: its backup
    /// verifies what the owner confirmed), executes, and tells the panel.
    pub(super) fn start_action(&mut self, ctx: &egui::Context, i: usize, action: Action) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let Some((repo, _)) = p.view.repo() else {
            return;
        };
        let repo = repo.clone();
        let plan = p.view.take_plan(&action);
        let title = plan
            .as_ref()
            .and_then(|plan| plan.confirm.as_ref())
            .map(|c| c.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                git_panel::action_label(action.id())
                    .trim_end_matches('…')
                    .to_string()
            });
        let network = plan.as_ref().map_or_else(Vec::new, |plan| {
            plan.steps
                .iter()
                .filter_map(|s| match s {
                    Step::Git(cmd) if cmd.class() == Class::Network => Some(cmd.command_line()),
                    _ => None,
                })
                .collect()
        });
        let lock = self
            .git
            .write_locks
            .entry(p.top.clone())
            .or_default()
            .clone();
        let handle = Handle::new();
        let started = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (h, s, waker, t) = (handle.clone(), started.clone(), ctx.clone(), title.clone());
        let worker = std::thread::spawn(move || {
            let _held = lock.lock().unwrap_or_else(|p| p.into_inner());
            s.store(true, Ordering::Relaxed);
            waker.request_repaint();
            let confirmed = plan.is_some();
            let planned = match plan {
                Some(plan) => Ok((plan, repo)),
                None => plan_now(&runner, &h, repo, &action),
            };
            let end = match planned {
                // 03 §8.1: only a plan with no confirmation runs at once; one that asks for a
                // confirmation no dialog showed does not run (review 1).
                Ok((plan, _)) if !confirmed && plan.confirm.is_some() => RunEnd::Confirm {
                    title: t,
                    action: Box::new(action),
                },
                Ok((plan, repo)) => {
                    let outcome = actions::execute(&plan, &repo.top, &runner, &h);
                    let what = plan
                        .message
                        .strip_prefix("sopen: ")
                        .filter(|m| !m.is_empty())
                        .map_or_else(|| t.clone(), str::to_string);
                    let squash = matches!(
                        &action,
                        Action::Merge { options, .. } if options.squash && !options.no_commit
                    );
                    RunEnd::Ran {
                        outcome: Box::new(outcome),
                        title: t,
                        what,
                        changes_files: plan.changes_files,
                        squash,
                        action: Box::new(action),
                    }
                }
                Err((why, line)) => RunEnd::NotRun {
                    title: t,
                    why,
                    line,
                },
            };
            let _ = tx.send(end);
            waker.request_repaint();
        });
        p.running.push(Running {
            title,
            handle,
            rx,
            started,
            since: std::time::Instant::now(),
            network,
            worker: Some(worker),
        });
    }

    /// A run ended (03 §3.8, §5, §8.3): its result — the error dialog, the status row with the
    /// backup and Cmd/Ctrl+Z, or its warning —, the group onto the window's undo, the panels'
    /// folders re-listed when files may have changed, the Commit dialog after a squash, and the
    /// refresh.
    pub(super) fn run_ended(&mut self, ctx: &egui::Context, i: usize, end: RunEnd) {
        let top = self.git.panels[i].top.clone();
        // What Esc said while it ran ("stopping") gives way to the run's own words.
        self.status = Status::default();
        if let RunEnd::Confirm { title, action } = end {
            // Nothing ran, nothing to refresh: the action's confirmation, again.
            let p = &mut self.git.panels[i];
            p.view.status = Some(Status::info(format!(
                "{title}: not run — its plan asks for a confirmation"
            )));
            let asked = p.view.reconfirm(*action);
            self.git_actions(ctx, i, asked);
            return;
        }
        match end {
            RunEnd::Confirm { .. } => {}
            RunEnd::Ran {
                outcome,
                title,
                what,
                changes_files,
                squash,
                action,
            } => {
                if let Some(stamp) = &outcome.backup {
                    let undo = self.git.undo.entry(top.clone()).or_default();
                    record_group(undo, stamp.clone());
                    let newest = undo.newest().map(String::from);
                    self.git.panels[i].view.set_undo(newest);
                }
                let p = &mut self.git.panels[i];
                let said = p.view.show_outcome(&title, &what, &outcome);
                p.view.status = Some(said);
                if changes_files {
                    let host = self.label.clone();
                    self.git_event(ctx, GitEvent::WorkTreeChanged { host, top });
                }
                let done = outcome
                    .steps
                    .iter()
                    .all(|s| s.outcome == actions::StepOutcome::Done);
                if squash && done {
                    self.ask_prefill(ctx, i, ActionId::Commit, git_panel::Target::Uncommitted);
                }
                // 03 §7.2: git refused to delete a branch with commits no other branch has —
                // the second dialog, "delete anyway?", in place of the error.
                if let Action::DeleteBranch {
                    branch,
                    force: false,
                    remotes,
                } = *action
                {
                    let unmerged = outcome.steps.iter().any(|s| {
                        s.outcome == actions::StepOutcome::Failed
                            && s.ran.iter().any(|d| d.stderr.contains("not fully merged"))
                    });
                    if unmerged {
                        let forced = Action::DeleteBranch {
                            branch,
                            force: true,
                            remotes,
                        };
                        let p = &mut self.git.panels[i];
                        let asked = p.view.confirm_again(forced);
                        self.git_actions(ctx, i, asked);
                    }
                }
            }
            RunEnd::NotRun { title, why, line } => {
                let p = &mut self.git.panels[i];
                p.view.show_text(&title, &why, line.as_deref());
                p.view.status = Some(Status::error(format!("{title}: not run")));
            }
        }
        // 03 §5: a command of the panel finished — refresh (the rows stay when the graph did).
        self.start_read(ctx, i, true, 0);
        if self.git.panels[i].view.backups().is_some() {
            self.ask_backups(ctx, i);
        }
    }

    /// The prefill of `id`'s dialog (03 §7), read on a worker; then the dialog.
    pub(super) fn ask_prefill(
        &mut self,
        ctx: &egui::Context,
        i: usize,
        id: ActionId,
        target: git_panel::Target,
    ) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let remotes = p
            .view
            .repo()
            .map(|(r, _)| r.remotes.clone())
            .unwrap_or_default();
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        std::thread::spawn(move || {
            let read = read_prefill(&git, &top, id, &target, &remotes);
            let _ = tx.send(Prefilled { id, target, read });
            waker.request_repaint();
        });
        p.prefill = Some(Pending { rx, handle });
    }

    /// The reads the open dialog's plan needs beyond the refresh (S3-A's `needs` / `inspect`),
    /// on a worker, once per draft and snapshot; the dialog plans again with them.
    fn dialog_reads(&mut self, ctx: &egui::Context, i: usize) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        if p.inspect.is_some() {
            return;
        }
        let (Some(draft), Some((repo, generation))) = (p.view.draft(), p.view.repo()) else {
            p.checked = None;
            return;
        };
        let key = (draft.clone(), generation, repo.seen.reads.len());
        if p.checked.as_ref() == Some(&key) {
            return;
        }
        // A read that failed for this snapshot is not asked again (no loop of failing reads).
        let missing: Vec<actions::Need> = actions::needs(draft, repo)
            .into_iter()
            .filter(|n| !p.read_failed(generation, n))
            .collect();
        p.checked = Some(key);
        if missing.is_empty() {
            return;
        }
        let mut repo = repo.clone();
        repo.seen = Seen::default();
        let handle = Handle::new();
        let (tx, rx) = mpsc::channel();
        let (h, waker) = (handle.clone(), ctx.clone());
        std::thread::spawn(move || {
            // `inspect` records a read only once its command answered: a failed one stays unread.
            let failed = actions::inspect(&runner, &h, &mut repo, &missing).err();
            let _ = tx.send(Inspected {
                seen: repo.seen,
                asked: missing,
                failed,
            });
            waker.request_repaint();
        });
        p.inspect = Some((Pending { rx, handle }, generation));
    }

    /// The Backups list (03 §8.2), read on a worker.
    pub(super) fn ask_backups(&mut self, ctx: &egui::Context, i: usize) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let handle = Handle::new();
        let (tx, rx) = mpsc::channel();
        let (top, h, waker) = (p.top.clone(), handle.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(backup::list(&runner, &h, &top));
            waker.request_repaint();
        });
        p.backups_read = Some(Pending { rx, handle });
    }

    /// Group `stamp`'s entries with the values now (03 §8.3), read on a worker; then its dialog.
    fn ask_restore(&mut self, ctx: &egui::Context, i: usize, stamp: String, only: Option<String>) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let Some((repo, _)) = p.view.repo() else {
            return;
        };
        let head = repo.head.clone();
        let handle = Handle::new();
        let (tx, rx) = mpsc::channel();
        let (top, h, waker) = (p.top.clone(), handle.clone(), ctx.clone());
        std::thread::spawn(move || {
            let read = match backup::group(&runner, &h, &top, &stamp) {
                Ok(Some(record)) => backup::entries(&runner, &h, &top, &record, &head)
                    .map(|entries| (stamp, entries, only))
                    .map_err(|f| f.reason),
                Ok(None) => Err(format!("no backup group {stamp}")),
                Err(f) => Err(f.reason),
            };
            let _ = tx.send(read);
            waker.request_repaint();
        });
        p.restore_read = Some(Pending { rx, handle });
    }

    /// A tag's details (03 §7.4: `cat-file -p <t>`), read on a worker; then in a dialog.
    fn ask_tag_details(&mut self, ctx: &egui::Context, i: usize, refname: String) {
        let Some(runner) = self.git_runner() else {
            return;
        };
        let p = &mut self.git.panels[i];
        let handle = Handle::new();
        let git = RunnerGit::new(runner, handle.clone());
        let (tx, rx) = mpsc::channel();
        let (top, waker) = (p.top.clone(), ctx.clone());
        std::thread::spawn(move || {
            let name = refname.strip_prefix("refs/tags/").unwrap_or(&refname);
            let title = format!("Tag {name}");
            let text = read_value(&git, &top, &["cat-file", "-p", &refname]);
            let _ = tx.send((title, text));
            waker.request_repaint();
        });
        p.text_read = Some(Pending { rx, handle });
    }

    /// What panel `i`'s S3 workers answered: runs that ended, a prefill (its dialog opens), a
    /// dialog's reads, the Backups list, a restore's entries, a tag's details; and the command
    /// log handed again when the runner logged more.
    pub(super) fn poll_actions(&mut self, ctx: &egui::Context, i: usize) {
        let mut ended = Vec::new();
        self.git.panels[i]
            .running
            .retain_mut(|run| match run.rx.try_recv() {
                Ok(end) => {
                    ended.push(end);
                    false
                }
                Err(TryRecvError::Empty) => true,
                Err(TryRecvError::Disconnected) => {
                    ended.push(RunEnd::NotRun {
                        title: run.title.clone(),
                        why: "the run stopped without a result".into(),
                        line: None,
                    });
                    false
                }
            });
        for end in ended {
            self.run_ended(ctx, i, end);
        }
        let p = &mut self.git.panels[i];
        if let Some(Ok(prefilled)) = p.prefill.as_ref().map(|r| r.rx.try_recv()) {
            p.prefill = None;
            match prefilled.read {
                Ok(read) => {
                    let asked = p.view.open_action(prefilled.id, prefilled.target, read);
                    self.git_actions(ctx, i, asked);
                }
                Err(why) => {
                    let what = git_panel::action_label(prefilled.id).trim_end_matches('…');
                    p.view.show_text(what, &why, None);
                }
            }
        }
        let p = &mut self.git.panels[i];
        let inspected = p
            .inspect
            .as_ref()
            .and_then(|(r, g)| r.rx.try_recv().ok().map(|read| (read, *g)));
        if let Some((read, generation)) = inspected {
            p.inspect = None;
            if let Some(failed) = read.failed {
                // The read that failed stays unread: the plan refuses "not read yet: …" and the
                // dialog says so; it is not asked again for this snapshot. The status row says
                // why.
                let came = |n: &actions::Need| read.seen.get(n).is_some();
                if let Some(need) = read.asked.iter().find(|n| !came(n)).cloned() {
                    match &mut p.failed_reads {
                        Some((g, needs)) if *g == generation => needs.push(need),
                        slot => *slot = Some((generation, vec![need])),
                    }
                }
                p.view.status = Some(Status::error(format!(
                    "a read for the dialog failed: {}",
                    failed.reason
                )));
                // The reads after it in that round were not made: asked again, without it.
                p.checked = None;
            }
            p.view.seen(generation, read.seen);
        }
        self.dialog_reads(ctx, i);
        let p = &mut self.git.panels[i];
        if let Some(Ok(read)) = p.backups_read.as_ref().map(|r| r.rx.try_recv()) {
            p.backups_read = None;
            match read {
                Ok(records) => p.view.open_backups(records),
                Err(failed) => {
                    let line = failed.ran.last().map(|d| d.entry.line.clone());
                    p.view.show_text("Backups", &failed.reason, line.as_deref());
                }
            }
        }
        if let Some(Ok(read)) = p.restore_read.as_ref().map(|r| r.rx.try_recv()) {
            p.restore_read = None;
            match read {
                Ok((stamp, entries, only)) => {
                    let asked = p.view.open_restore(stamp, entries, only.as_deref());
                    self.git_actions(ctx, i, asked);
                }
                Err(why) => p.view.show_text("Restore", &why, None),
            }
        }
        let p = &mut self.git.panels[i];
        if let Some(Ok((title, text))) = p.text_read.as_ref().map(|r| r.rx.try_recv()) {
            p.text_read = None;
            match text {
                Ok(text) => p.view.show_text(&title, &text, None),
                Err(why) => p.view.show_text(&title, &why, None),
            }
        }
        // The command log: handed again when the runner logged a newer entry.
        let runner = self.git_runner();
        let p = &mut self.git.panels[i];
        if let (Some(shown), Some(runner)) = (p.view.log_newest(), runner) {
            let last = runner.log().last();
            if last.as_ref() != shown {
                p.view.set_log(runner.log().entries());
            }
        }
    }

    /// The Backups list's Show (03 §8.2): the refs' commits in the graph's revisions until the
    /// next refresh, the bar on the first.
    fn show_backups(&mut self, ctx: &egui::Context, i: usize, refs: Vec<BackupRef>) {
        let p = &mut self.git.panels[i];
        p.shown = refs.iter().map(|r| r.oid.clone()).collect();
        if let Some(first) = refs.first() {
            p.view.select(RowKey::Commit(first.oid.clone()));
            p.view.status = Some(Status::info(format!(
                "showing {} until the next refresh",
                refs.iter()
                    .map(|r| format!("{} {}", backups_kind(&r.kind), r.oid.short()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let pages = p.view.rows().len().div_ceil(history::PAGE).max(1);
        self.start_stream(ctx, i, pages);
    }
}

/// A backup ref's kind as 03 §8.2 names it (`heads/main`, `files`).
fn backups_kind(kind: &crate::git::model::BackupKind) -> String {
    backup::kind_name(kind)
}

/// The handles of what panel `p` waits for on the owner's behalf: a page or a search's page asked
/// and not come, a load's reads, a file row's diff or file. (A details read following the bar is
/// not: the next bar move replaces it.)
pub(super) fn git_waiting(p: &RepoPanel) -> Vec<Handle> {
    let mut out = Vec::new();
    if let Some(s) = p.stream.as_ref().filter(|s| s.asked > 0) {
        out.push(s.handle.clone());
    }
    if let Some(s) = p.search.as_ref().filter(|s| s.link.asked > 0) {
        out.push(s.link.handle.clone());
    }
    if let Some(r) = &p.read {
        out.push(r.pending.handle.clone());
    }
    if let Some(r) = &p.reveal {
        out.push(r.handle.clone());
    }
    // S3's reads the owner waits for: a dialog's prefill, the Backups list, a restore's entries,
    // a tag's details.
    out.extend(p.prefill.as_ref().map(|r| r.handle.clone()));
    out.extend(p.backups_read.as_ref().map(|r| r.handle.clone()));
    out.extend(p.restore_read.as_ref().map(|r| r.handle.clone()));
    out.extend(p.text_read.as_ref().map(|r| r.handle.clone()));
    out
}

/// A read-only viewer's title and the file F4 opens (the path now, never the `.revisions` copy).
#[derive(Debug, Clone)]
pub(super) struct ReadOnly {
    pub(super) title: String,
    pub(super) edit: String,
}

/// A log stream on a worker of its own: started with `args` and `roots`, it reads as many pages
/// as each ask says, and ends when the asks' sender goes (git stopped with it).
pub(super) fn spawn_stream(
    ctx: &egui::Context,
    git: RunnerGit,
    top: String,
    args: Vec<String>,
    roots: Vec<Oid>,
    generation: Generation,
    g: u64,
) -> (mpsc::Sender<usize>, Receiver<Page>) {
    let (ask, asks) = mpsc::channel::<usize>();
    let (tx, pages) = mpsc::channel();
    let waker = ctx.clone();
    std::thread::spawn(move || {
        let mut stream = match LogStream::start_with(&git, &top, args, &roots, &generation, g) {
            Ok(stream) => stream,
            Err(e) => {
                let why = match e {
                    StartError::Run(e) => format!("git log: {e}"),
                    StartError::NoRoots => "no refs chosen".into(),
                    StartError::Superseded => return,
                };
                let _ = tx.send(Page {
                    generation: g,
                    rows: Vec::new(),
                    end: PageEnd::Error(why),
                });
                waker.request_repaint();
                return;
            }
        };
        while let Ok(n) = asks.recv() {
            for _ in 0..n {
                let page = stream.page(history::PAGE);
                let more = page.end == PageEnd::More;
                if tx.send(page).is_err() {
                    return;
                }
                waker.request_repaint();
                if !more {
                    break;
                }
            }
        }
    });
    (ask, pages)
}
