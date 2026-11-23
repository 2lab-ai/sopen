//! The git panel's shared read model (03): the plain data the parsers make from git's output and
//! the lane layout, the panel and the actions read. Each type names the read it comes from, and
//! each field is a format atom of that read or a word of 03's tables, so the parser and its
//! readers agree on one spelling.
//!
//! S2-0 wrote these types and S2-B owns them: S2-B adds fields, variants and types; renaming or
//! removing one goes through the dispatcher (02 §5), since S2-C, S2-D and S3 build on them.

/// A full object id (03 §1's `<c>`): 40 lowercase hex digits, or 64 in a SHA-256 repository
/// (03 §6.2). Only [`Oid::parse`] makes one, so every `Oid` is well formed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(String);

impl Oid {
    /// `text` as git prints an object id (`%H`, `%(objectname)`), or `None` for anything else:
    /// a record whose hash does not parse ends the page with an error (03 §4.1).
    pub fn parse(text: &str) -> Option<Oid> {
        let well_formed = matches!(text.len(), 40 | 64)
            && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        well_formed.then(|| Oid(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 8 hex digits: the graph's Commit column (03 §4.2).
    pub fn short(&self) -> &str {
        &self.0[..8]
    }
}

/// What one folder is to git: a variant per row of 03 §2.1's table, from `rev-parse
/// --is-bare-repository --is-inside-git-dir --is-shallow-repository --absolute-git-dir
/// --git-common-dir --show-toplevel --show-superproject-working-tree` run in the folder. Cached
/// per `(host, folder)` (03 §2.2); only [`Detection::WorkTree`] shows the panel (03 §2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// A work tree, or a folder below its top level (exit 0).
    WorkTree {
        /// `--show-toplevel`: the panel's repository and its frame title.
        top: String,
        /// `--absolute-git-dir` (a linked worktree's own).
        git_dir: String,
        /// `--git-common-dir`, resolved against the folder when git prints it relative.
        common_dir: String,
        /// A linked worktree: `git_dir` is not `common_dir` (the title's ` (worktree of …)`).
        linked: bool,
        /// A submodule: the 7th line, the superproject's top level (` (submodule of …)`).
        submodule_of: Option<String>,
        /// A shallow clone: the third line is `true` (` (shallow)`).
        shallow: bool,
    },
    /// A repository with no work tree here (exit 128, `must be run in a work tree`).
    NoWorkTree(NoWorkTree),
    /// Not in a repository (exit 128, `not a git repository`). `Some` holds stderr's line when
    /// git stopped at a filesystem boundary: a mounted folder is not part of the repo above it.
    NotARepo(Option<String>),
    /// git refuses the repository (another user's: `safe.directory`): stderr's first line.
    Refused(String),
    /// No answer to use: git missing, too old (03 §2.4), the 10 s bound passed, a top level
    /// that is not UTF-8, or an answer detection cannot read.
    Unavailable(Unavailable),
}

/// Why a repository has no work tree at the folder (03 §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoWorkTree {
    /// A bare repository (`true`, `true`, …).
    Bare,
    /// Inside a `.git` directory (`false`, `true`, …).
    GitDir,
}

/// Why detection gave no answer to use (03 §2.1's last row, and S2-B's `NotUtf8` and `Failed`).
/// [`Unavailable::NoGit`] is cached per host (03 §2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    /// No git there (a remote's `sopen: no git`, exit 127; 03 §2.4).
    NoGit,
    /// The host's `git version`, below the 2.20 floor (03 §2.4).
    TooOld(String),
    /// The 10 s bound passed.
    TimedOut,
    /// A work tree whose top level is not UTF-8 (shown here with U+FFFD): sopen names a
    /// repository by its top level (`-C <top level>`, 03 §3.1), and a lossy name would run git
    /// in another folder, so such a repository keeps the directory list, as the browser never
    /// opens an entry whose name is not UTF-8 (`remote::Entry::lossy`).
    NotUtf8(String),
    /// git answered with something detection cannot read (an exit or a message 03 §2.1's
    /// table does not have), or could not be run: stderr's first line or the runner's error.
    /// Not cached, so the next visit asks again.
    Failed(String),
}

/// A host's `git version` (03 §2.4), run once per host: the 2.20 floor and the features above
/// it are decided from `major.minor.patch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// What follows `git version ` as git printed it (`2.39.5 (Apple Git-154)`).
    pub text: String,
}

impl GitVersion {
    /// At least `major.minor` (the floor is `at_least(2, 20)`; `--update-refs` is 2.38).
    pub fn at_least(&self, major: u32, minor: u32) -> bool {
        (self.major, self.minor) >= (major, minor)
    }
}

/// 03 §4.1's HEAD read: `symbolic-ref -q HEAD`, then `rev-parse -q --verify HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// The checked-out branch (`refs/heads/main`) and its commit.
    Branch {
        refname: String,
        /// `refname` is not valid UTF-8: it shows with U+FFFD and names no ref.
        lossy: bool,
        oid: Oid,
    },
    /// HEAD detached at a commit.
    Detached(Oid),
    /// A branch with no commit yet: an empty repository (03 §4.1, `no commits yet`) or an
    /// orphan branch.
    Unborn { refname: String, lossy: bool },
}

impl Head {
    /// HEAD's commit, `None` while unborn.
    pub fn oid(&self) -> Option<&Oid> {
        match self {
            Head::Branch { oid, .. } | Head::Detached(oid) => Some(oid),
            Head::Unborn { .. } => None,
        }
    }
}

/// One commit of the log stream: 03 §4.1's record,
/// `%H%x00%P%x00%aN%x00%aE%x00%at%x00%ct%x00%s`, holding what 03 §4.5 says a row stores. Its
/// lanes and node segments are not here: they are the lane layout's (`graph`, S2-C).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    /// `%H`.
    pub oid: Oid,
    /// `%P`, in git's order (the first parent first); empty for a root commit.
    pub parents: Vec<Oid>,
    /// `%aN` (after `.mailmap`), cut at 256 bytes.
    pub author_name: String,
    /// `%aE` (after `.mailmap`), cut at 256 bytes.
    pub author_email: String,
    /// `%at`: the author date in Unix seconds (the Date column, 03 §4.2).
    pub author_time: i64,
    /// `%ct`: the committer date in Unix seconds.
    pub commit_time: i64,
    /// `%s`, cut at 1 KiB; text that is not UTF-8 has U+FFFD.
    pub subject: String,
}

/// One ref of 03 §4.1's refs read, `for-each-ref
/// --format=%(objectname)%00%(objecttype)%00%(*objectname)%00%(refname)%00%(upstream)%00%(upstream:track)
/// refs/heads refs/remotes refs/tags`: a label of the graph (03 §4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLabel {
    /// `%(objectname)`: the object id read — an annotated tag's own tag object.
    pub object: Oid,
    /// `%(objecttype)`.
    pub object_type: ObjectType,
    /// `%(*objectname)`: what an annotated tag points at, where its label is drawn; `None` for
    /// any other ref.
    pub peeled: Option<Oid>,
    /// `%(refname)`, whole (`refs/heads/main`, `refs/remotes/origin/main`, `refs/tags/v1`).
    pub refname: String,
    /// What `refname`'s prefix says it is (derived, not a format atom).
    pub kind: RefKind,
    /// `%(upstream)`; `None` when git prints nothing.
    pub upstream: Option<String>,
    /// `%(upstream:track)` as git prints it (`[ahead 2, behind 1]`, `[gone]`); empty for none.
    pub track: String,
    /// `refname` or `upstream` is not valid UTF-8 (as `remote::Entry::lossy`): it shows with
    /// U+FFFD, and no action may name the ref by it — two names can show alike.
    pub lossy: bool,
}

impl RefLabel {
    /// Where the label is drawn: an annotated tag's `*objectname`, else `objectname`.
    pub fn target(&self) -> &Oid {
        self.peeled.as_ref().unwrap_or(&self.object)
    }

    /// `refname` without its kind's prefix: `main`, `origin/main`, `v1`.
    pub fn name(&self) -> &str {
        ["refs/heads/", "refs/remotes/", "refs/tags/"]
            .iter()
            .find_map(|p| self.refname.strip_prefix(p))
            .unwrap_or(&self.refname)
    }
}

/// git's four object types, as `%(objecttype)` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Commit,
    Tag,
    Tree,
    Blob,
}

/// The kinds of label 03 §4.4 draws, by the ref's prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// `refs/heads/`: a local branch.
    LocalBranch,
    /// `refs/remotes/`: a remote branch (`origin/HEAD` is not drawn).
    RemoteBranch,
    /// `refs/tags/`.
    Tag,
}

/// One stash of 03 §4.1's stashes read, `stash list -z --format=%H%x1f%P%x1f%gd%x1f%ct%x1f%s`,
/// with all its parents. It is drawn as a row of its own above its base (03 §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashRow {
    /// `%H`: the stash commit.
    pub oid: Oid,
    /// `%P`'s first: the base, the stash row's one parent in the graph.
    pub base: Oid,
    /// `%P`'s second: the index.
    pub index: Option<Oid>,
    /// `%P`'s third: the untracked files, shown as `?` in its details (03 §6.1).
    pub untracked: Option<Oid>,
    /// `%gd`: `stash@{n}`, the row's label.
    pub selector: String,
    /// `%ct`, in Unix seconds.
    pub commit_time: i64,
    /// `%s`.
    pub subject: String,
}

/// One path of 03 §4.1's status read, `status --porcelain=v2 -z --branch
/// --untracked-files=normal`: a file of the uncommitted row (03 §6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// The path from the top level; an untracked folder is one entry ending in `/` (`dir/`).
    pub path: String,
    /// A rename's old path (`R`, shown `old → new`).
    pub old_path: Option<String>,
    pub state: StatusState,
    /// `path` or `old_path` is not valid UTF-8: it shows with U+FFFD and names no file.
    pub lossy: bool,
}

/// 03 §4.1's status read whole: its `# branch.*` headers and its entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub branch: BranchStatus,
    pub entries: Vec<StatusEntry>,
}

/// The status read's `--branch` headers: the foot's `main ↑2 ↓1 origin/main` (03 §4.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BranchStatus {
    /// `# branch.oid`; `None` for `(initial)` (no commit yet).
    pub oid: Option<Oid>,
    /// `# branch.head`, the short name; `None` for `(detached)`.
    pub head: Option<String>,
    /// `# branch.upstream` (`origin/main`); `None` without one.
    pub upstream: Option<String>,
    /// `# branch.ab`; `None` when git prints none (no upstream, or it is gone).
    pub ahead_behind: Option<AheadBehind>,
    /// `head` or `upstream` is not valid UTF-8.
    pub lossy: bool,
}

/// `# branch.ab +<ahead> -<behind>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AheadBehind {
    pub ahead: u64,
    pub behind: u64,
}

/// A commit's summary in the details pane (03 §6.1), from `log -1 -z
/// --format=%H%x00%P%x00%aN%x00%aE%x00%at%x00%cN%x00%cE%x00%ct%x00%B <c> --`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitDetails {
    /// `%H`.
    pub oid: Oid,
    /// `%P`, the first parent first.
    pub parents: Vec<Oid>,
    /// `%aN`, `%aE` (after `.mailmap`); text that is not UTF-8 has U+FFFD.
    pub author_name: String,
    pub author_email: String,
    /// `%at`, Unix seconds.
    pub author_time: i64,
    /// `%cN`, `%cE`.
    pub committer_name: String,
    pub committer_email: String,
    /// `%ct`, Unix seconds.
    pub commit_time: i64,
    /// `%B`, whole, as git printed it; text that is not UTF-8 has U+FFFD.
    pub message: String,
    /// `%B` is not valid UTF-8: never prefilled into an editor, so reword, squash and amend
    /// refuse the commit with the reason (03 §3.3).
    pub message_lossy: bool,
}

/// One row of the details pane's Files (03 §6.1): `diff -z --find-renames --name-status <old>
/// <new> --` and the same with `--numstat`, joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub letter: FileLetter,
    /// The path (a rename's or copy's new one).
    pub path: String,
    /// A rename's or copy's old path (`old → new`).
    pub old_path: Option<String>,
    /// `+adds -dels` for text; `None` for a binary file (numstat's `-`).
    pub lines: Option<(u64, u64)>,
    /// `path` or `old_path` is not valid UTF-8: it shows with U+FFFD and names no file.
    pub lossy: bool,
}

/// The change letter of a details file row (03 §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLetter {
    /// `A`, `M`, `D`, `R`, `T` or `C`.
    Change(Change),
    /// `U`: unmerged.
    Unmerged,
    /// `?`: a stash's untracked file (its third parent, 03 §6.1).
    Untracked,
}

/// Which of 03 §6.1's groups an entry is in, with its change letters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusState {
    /// Tracked and changed: `Staged` when the index differs from HEAD, `Not staged` when the
    /// work tree differs from the index (porcelain v2's `XY`; `.` is `None`).
    Changed {
        staged: Option<Change>,
        unstaged: Option<Change>,
    },
    /// Unmerged (`U`): the `Conflicts` group, git's `XY` as it prints it (`UU`, `AA`, `DU`, …).
    Conflict(String),
    /// Untracked (`?`).
    Untracked,
}

/// A change letter of 03 §6.1 on a tracked path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// `A`.
    Added,
    /// `M`.
    Modified,
    /// `D`.
    Deleted,
    /// `R`.
    Renamed,
    /// `T`.
    TypeChanged,
    /// `C`: a copy (status with `status.renames=copies`; a diff with copy detection).
    Copied,
}

/// What the in-progress probe found (03 §8.5): the operation git has in progress in this work
/// tree, whoever started it, shown in the banner. The default is nothing in progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InProgress {
    /// The first row of 03 §8.5's table whose marker exists; `None` when none does.
    pub operation: Option<Operation>,
    /// `BISECT_LOG` exists: bisect, on its own line as well as any operation.
    pub bisect: bool,
    /// `rebase-merge/`'s files, for the banner (`REBASE 3/7 · stopped at abc12345`).
    pub rebase: Option<RebaseProgress>,
    /// The first line of `sequencer/todo`.
    pub todo_first: Option<String>,
    /// What a merge, a cherry-pick or a revert applies, for the banner's `MERGE of feature`,
    /// `CHERRY-PICK abc12345`, `REVERT abc12345` (03 §8.5; S3-C): `None` when none of their
    /// head files exists.
    pub head: Option<OperationHead>,
}

/// The head file of the operation in progress (03 §8.5's banner detail), as the probe read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperationHead {
    /// The first line of `MERGE_HEAD`, `CHERRY_PICK_HEAD` or `REVERT_HEAD` — the first of them
    /// that exists: the commit merged, picked or reverted.
    pub commit: String,
    /// A merge's `MERGE_MSG`, its first line (`Merge branch 'feature'`), when it exists.
    pub merge_msg: Option<String>,
}

/// The states of 03 §8.5's table, by row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// 1, `rebase-merge/`: a rebase (interactive or merge backend; planner runs).
    Rebase,
    /// 2, `rebase-apply/` with `applying`: am (a patch series).
    Am,
    /// 3, `rebase-apply/` with `rebasing`: a rebase (apply backend).
    RebaseApply,
    /// 4, `MERGE_HEAD`: a merge.
    Merge,
    /// 5, `CHERRY_PICK_HEAD`: a cherry-pick.
    CherryPick,
    /// 6, `REVERT_HEAD`: a revert.
    Revert,
    /// 7, `sequencer/` alone: a cherry-pick or revert sequence between steps; the first word of
    /// `sequencer/todo` says which.
    Sequence(Sequence),
}

/// The first word of `sequencer/todo` (03 §8.5 row 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequence {
    /// `pick`: as a cherry-pick.
    Pick,
    /// `revert`: as a revert.
    Revert,
}

/// The `rebase-merge/` files the probe prints (03 §8.5), each as git wrote it, `None` when absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebaseProgress {
    /// `msgnum`: the step.
    pub msgnum: Option<String>,
    /// `end`: the steps in all.
    pub end: Option<String>,
    /// `head-name`: the branch being rebased.
    pub head_name: Option<String>,
    /// `onto`.
    pub onto: Option<String>,
    /// `stopped-sha`: where it stopped.
    pub stopped_sha: Option<String>,
    /// The last line of `done` (an edit stop: `edit <c> …`).
    pub done_last: Option<String>,
}

/// A backup group (03 §8.2): the refs under `refs/sopen-backup/<stamp>/` written in one
/// transaction before a history rewrite or destructive action, its manifest among them; a group
/// of the Backups list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// `<stamp>`: the UTC time of Run as `YYYYMMDDTHHMMSSZ`, with `-2`, `-3`… when taken.
    pub stamp: String,
    /// The reflog message (`sopen: drop abc12345 from main`).
    pub message: String,
    /// Each ref of the group.
    pub refs: Vec<BackupRef>,
}

/// One ref of a [`Backup`]: its kind, short hash and subject are a row of the Backups list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRef {
    pub kind: BackupKind,
    /// The whole ref name (`refs/sopen-backup/<stamp>/heads/main`).
    pub refname: String,
    /// What it holds.
    pub oid: Oid,
    /// The subject of what it holds.
    pub subject: String,
}

/// The kinds of 03 §8.2's table, by the name under the stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupKind {
    /// `heads/<b>`: a branch's commit.
    Head(String),
    /// `detached`: HEAD's commit when detached.
    Detached,
    /// `tags/<t>`: a tag (its tag object when annotated).
    Tag(String),
    /// `stash`: a stash commit.
    Stash,
    /// `worktree`: `stash create`'s commit of the index and the tracked changes.
    Worktree,
    /// `files`: a commit, parent HEAD, of the current bytes of a list of paths.
    Files,
    /// `pushed/<r>/heads/<b>`, `pushed/<r>/tags/<t>`: a remote's value; the name after `pushed/`.
    Pushed(String),
    /// `remotes/<r>/<b>`: a remote-tracking ref; the name after `remotes/`.
    Remote(String),
    /// `manifest`: every value before the action, `absent` included.
    Manifest,
}
