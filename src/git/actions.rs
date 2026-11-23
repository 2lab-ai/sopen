//! The git panel's actions (03 §7.1–§7.7, §8): every action as a pure plan — its steps, each a
//! git command of 03 §3.4's class, the confirmation its dialog shows (§8.1), the backups written
//! before its first step (§8.2) and its overwrites or deletes on a remote (§8.4) — and the banner
//! of an operation in progress (§8.5). The history edits (§7.8) are S4's (`rebase.rs`). S3-A
//! writes it; the menu, the dialogs, the backups list, the command log and the banner that show
//! it are S3-B's (`gui::browser::git_panel`), the wiring S3-C's.
//!
//! # The API (S3-0's contract between S3-A and S3-B)
//!
//! - **Ids.** [`ActionId`] is every id of 03 §7.1–§7.7's tables, spelled as the tables spell them
//!   (`SOPEN_STATE dialog=<id>`, 02 §9), plus the few sopen names where 03 gives none (the
//!   conflict's Redo, the backups list's Restore, Show and Delete, the banner's keys). The menu
//!   (S3-B) offers ids for the object under the bar; the views among them (copy, compare, the
//!   filters, find, the log, …) the panel carries out itself, with no plan.
//! - **Actions.** A dialog collects its options into an [`Action`], one variant per id that runs
//!   git, with the data it needs, and asks [`plan`] for the [`Plan`] against the [`Repo`]
//!   snapshot — again whenever an option changes (§8.1: the confirmation updates as options
//!   change), so `plan` reads nothing and runs nothing. An action it refuses before anything runs
//!   (§8.6's files in the way, a mirror's prune, a reset hard during a conflict) is [`Refused`].
//! - **Steps.** A [`Step::Git`] is a [`GitCommand`]: its words, its [`Class`](super::runner::Class),
//!   stdin and environment. Its [`GitCommand::command_line`] is the confirmation's line and the
//!   log's. The runner adds `--progress --no-prune --no-prune-tags` to every `fetch` and
//!   `--progress` to every `push` ([`GitCommand::args`]), so a plan does not add them again; an
//!   action that prunes passes `--prune` / `--prune-tags` itself, which win.
//! - **What is decided at Run.** The backup stamp is the UTC time of Run (§8.2) and §8.4's lease
//!   value is fetched at Run, so those parts are steps of their own ([`Step::Backup`],
//!   [`Step::Remote`]) rather than final words.
//! - **Outcome.** Running a plan gives an [`Outcome`]: each step `done`, `failed` or `not run`
//!   (§3.8), with the commands it ran as the log keeps them. [`execute`] runs a plan on a worker.
//!
//! # What S3-A added (for S3-B and S3-C)
//!
//! - **Reads beyond the refresh.** Some confirmations and checks need reads the refresh does not
//!   make: the paths an operation writes and the untracked or ignored entries in the way (§8.6),
//!   the files a backup takes (§8.2), the commits only a branch has (§8.1's `loses`), a
//!   remote's fetch refspecs (pruning), the user config (`user`). [`needs`] lists those `plan`
//!   would read and [`Repo::seen`] does not hold yet, keyed by git's own words (so typing a
//!   message never asks for a read again); [`inspect`] reads them on a worker into
//!   [`Repo::seen`]. Call `needs` until it is empty (each round can follow from the last: a
//!   folder in the way is listed only once it is known to be in the way), then `plan`; a `plan`
//!   with a read missing refuses (`not read yet: …`) rather than guess.
//! - **Running.** [`execute`]`(&plan, top, &runner, &handle)` runs the steps in order on a
//!   worker; the first that fails stops the rest. A backup step writes the group
//!   ([`backup::write`](super::backup::write)); a remote step pushes with the lease its backup
//!   fetched; a step naming `stash@{n}` runs only after `rev-parse stash@{n}` matches the stash
//!   the plan backed up (§7.5); a pruning command's group is trimmed after it (§8.2). A refusal
//!   at Run (endpoints that disagree, a stash list that changed, a taken archive name) is a step
//!   `failed` whose `Done` was never sent, its reason as its text.
//! - **The probe and the banner** (§8.5): [`probe_paths_command`] once per repository,
//!   [`probe_command`] on every refresh, [`parse_probe`] into `model::InProgress`, then
//!   [`Banner::of`].

use super::backup::{self, RestoreEntry, RestoreHow, Subject, Take, Value};
use super::model::{
    BackupKind, GitVersion, Head, InProgress, Oid, Operation, OperationHead, RebaseProgress,
    RefKind, RefLabel, Sequence, StashRow, Status, StatusState,
};
use super::runner::{Class, Done, GitCommand, GitRunner, Handle, Snippet};
use std::cell::RefCell;
use std::path::PathBuf;

/// Every action id of 03 §7.1–§7.7 (the history edits of §7.8 are S4's), and sopen's names for
/// the few actions 03 names without an id. An id appearing in several tables (`merge`, `rebase`,
/// `archive`, `copy-name`) is one id; its target is the [`Action`]'s data.
///
/// The ids with no [`Action`] variant are views or the clipboard, carried out by the panel with no
/// plan (03 §8.1's "No confirmation"): `compare`, `copy-hash`, `copy-subject`, `copy-name`,
/// `only-branch`, `all-branches`, `tag-details` (a read into the details pane), `editor` (F4),
/// `remote-visibility`, `refresh`, `find`, `search`, `filters`, `log`, `backups`, `show-backup`.
/// `undo` is the [`Action::Restore`] of the newest group written in this window (§8.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionId {
    // 03 §7.1, a commit row.
    Tag,
    Branch,
    MoveBranch,
    Checkout,
    Pick,
    Revert,
    Merge,
    Rebase,
    Reset,
    Archive,
    CopyHash,
    CopySubject,
    Compare,
    // §7.2, a local branch's label.
    CheckoutBranch,
    Rename,
    DeleteBranch,
    Push,
    OnlyBranch,
    AllBranches,
    CopyName,
    // §7.3, a remote branch's label.
    CheckoutRemote,
    DeleteRemoteBranch,
    FetchInto,
    Pull,
    // §7.4, a tag's label.
    TagDetails,
    DeleteTag,
    PushTag,
    MoveTag,
    // §7.5, a stash row.
    StashApply,
    StashPop,
    StashBranch,
    StashDrop,
    // §7.6, the uncommitted row and its files.
    Stage,
    Unstage,
    Commit,
    Amend,
    Stash,
    Discard,
    ResetChanges,
    Clean,
    Ours,
    Theirs,
    Resolved,
    /// "Redo the merge of the file" (§7.6): 03 gives it no id; `redo-merge` is sopen's, so it is
    /// never read as §8.3's redo.
    RedoMerge,
    Editor,
    // §7.7, the repository.
    Fetch,
    FetchRemote,
    PruneRemote,
    AddRemote,
    EditRemote,
    DeleteRemote,
    RemoteVisibility,
    User,
    UserRemove,
    Refresh,
    Find,
    Search,
    Filters,
    Log,
    Backups,
    Undo,
    // sopen's ids where 03 gives none: the Backups list's Restore, Show and Delete (§8.2), the
    // banner's keys (§8.5).
    Restore,
    ShowBackup,
    DeleteBackup,
    Continue,
    Skip,
    Abort,
    BisectReset,
}

impl ActionId {
    /// Every id, in 03 §7's table order.
    pub const ALL: [ActionId; 68] = [
        ActionId::Tag,
        ActionId::Branch,
        ActionId::MoveBranch,
        ActionId::Checkout,
        ActionId::Pick,
        ActionId::Revert,
        ActionId::Merge,
        ActionId::Rebase,
        ActionId::Reset,
        ActionId::Archive,
        ActionId::CopyHash,
        ActionId::CopySubject,
        ActionId::Compare,
        ActionId::CheckoutBranch,
        ActionId::Rename,
        ActionId::DeleteBranch,
        ActionId::Push,
        ActionId::OnlyBranch,
        ActionId::AllBranches,
        ActionId::CopyName,
        ActionId::CheckoutRemote,
        ActionId::DeleteRemoteBranch,
        ActionId::FetchInto,
        ActionId::Pull,
        ActionId::TagDetails,
        ActionId::DeleteTag,
        ActionId::PushTag,
        ActionId::MoveTag,
        ActionId::StashApply,
        ActionId::StashPop,
        ActionId::StashBranch,
        ActionId::StashDrop,
        ActionId::Stage,
        ActionId::Unstage,
        ActionId::Commit,
        ActionId::Amend,
        ActionId::Stash,
        ActionId::Discard,
        ActionId::ResetChanges,
        ActionId::Clean,
        ActionId::Ours,
        ActionId::Theirs,
        ActionId::Resolved,
        ActionId::RedoMerge,
        ActionId::Editor,
        ActionId::Fetch,
        ActionId::FetchRemote,
        ActionId::PruneRemote,
        ActionId::AddRemote,
        ActionId::EditRemote,
        ActionId::DeleteRemote,
        ActionId::RemoteVisibility,
        ActionId::User,
        ActionId::UserRemove,
        ActionId::Refresh,
        ActionId::Find,
        ActionId::Search,
        ActionId::Filters,
        ActionId::Log,
        ActionId::Backups,
        ActionId::Undo,
        ActionId::Restore,
        ActionId::ShowBackup,
        ActionId::DeleteBackup,
        ActionId::Continue,
        ActionId::Skip,
        ActionId::Abort,
        ActionId::BisectReset,
    ];

    /// The id as 03 §7 spells it (`delete-branch`): `SOPEN_STATE dialog=`'s value (02 §9).
    pub fn as_str(self) -> &'static str {
        match self {
            ActionId::Tag => "tag",
            ActionId::Branch => "branch",
            ActionId::MoveBranch => "move-branch",
            ActionId::Checkout => "checkout",
            ActionId::Pick => "pick",
            ActionId::Revert => "revert",
            ActionId::Merge => "merge",
            ActionId::Rebase => "rebase",
            ActionId::Reset => "reset",
            ActionId::Archive => "archive",
            ActionId::CopyHash => "copy-hash",
            ActionId::CopySubject => "copy-subject",
            ActionId::Compare => "compare",
            ActionId::CheckoutBranch => "checkout-branch",
            ActionId::Rename => "rename",
            ActionId::DeleteBranch => "delete-branch",
            ActionId::Push => "push",
            ActionId::OnlyBranch => "only-branch",
            ActionId::AllBranches => "all-branches",
            ActionId::CopyName => "copy-name",
            ActionId::CheckoutRemote => "checkout-remote",
            ActionId::DeleteRemoteBranch => "delete-remote-branch",
            ActionId::FetchInto => "fetch-into",
            ActionId::Pull => "pull",
            ActionId::TagDetails => "tag-details",
            ActionId::DeleteTag => "delete-tag",
            ActionId::PushTag => "push-tag",
            ActionId::MoveTag => "move-tag",
            ActionId::StashApply => "stash-apply",
            ActionId::StashPop => "stash-pop",
            ActionId::StashBranch => "stash-branch",
            ActionId::StashDrop => "stash-drop",
            ActionId::Stage => "stage",
            ActionId::Unstage => "unstage",
            ActionId::Commit => "commit",
            ActionId::Amend => "amend",
            ActionId::Stash => "stash",
            ActionId::Discard => "discard",
            ActionId::ResetChanges => "reset-changes",
            ActionId::Clean => "clean",
            ActionId::Ours => "ours",
            ActionId::Theirs => "theirs",
            ActionId::Resolved => "resolved",
            ActionId::RedoMerge => "redo-merge",
            ActionId::Editor => "editor",
            ActionId::Fetch => "fetch",
            ActionId::FetchRemote => "fetch-remote",
            ActionId::PruneRemote => "prune-remote",
            ActionId::AddRemote => "add-remote",
            ActionId::EditRemote => "edit-remote",
            ActionId::DeleteRemote => "delete-remote",
            ActionId::RemoteVisibility => "remote-visibility",
            ActionId::User => "user",
            ActionId::UserRemove => "user-remove",
            ActionId::Refresh => "refresh",
            ActionId::Find => "find",
            ActionId::Search => "search",
            ActionId::Filters => "filters",
            ActionId::Log => "log",
            ActionId::Backups => "backups",
            ActionId::Undo => "undo",
            ActionId::Restore => "restore",
            ActionId::ShowBackup => "show-backup",
            ActionId::DeleteBackup => "delete-backup",
            ActionId::Continue => "continue",
            ActionId::Skip => "skip",
            ActionId::Abort => "abort",
            ActionId::BisectReset => "bisect-reset",
        }
    }

    /// The id spelled `text` as 03 §7 spells it, or `None` (`SOPEN_STATE dialog=` refuses an
    /// unknown one, 02 §9).
    pub fn parse(text: &str) -> Option<ActionId> {
        ActionId::ALL.into_iter().find(|id| id.as_str() == text)
    }
}

/// A revision an action names, as 03 §7 writes it in the commands: `<c>`, `<b>`, `<r>/<b>`,
/// `<t>`. A ref whose name is not UTF-8 (`RefLabel::lossy`) is never one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rev {
    /// `<c>`: a commit, by its full hash.
    Commit(Oid),
    /// `<b>`: a local branch, the name after `refs/heads/`.
    Branch(String),
    /// `<r>/<b>`: a remote branch, its remote and the name after `refs/remotes/<r>/`.
    RemoteBranch { remote: String, branch: String },
    /// `<t>`: a tag, the name after `refs/tags/`.
    Tag(String),
}

/// `merge`'s fast-forward choice (03 §7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FastForward {
    /// When possible (the default): no flag.
    #[default]
    WhenPossible,
    /// Never: `--no-ff`.
    Never,
    /// Only: `--ff-only`.
    Only,
}

/// `merge`'s "on conflicts prefer" (03 §7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Prefer {
    /// Nothing (the default).
    #[default]
    Nothing,
    /// `-X ours`.
    Ours,
    /// `-X theirs`.
    Theirs,
}

/// `merge`'s options (03 §7.1), each off by default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOptions {
    pub fast_forward: FastForward,
    /// `--squash`; without `no_commit`, the Commit dialog (§7.6) then opens with git's prepared
    /// squash message.
    pub squash: bool,
    /// `--no-commit`.
    pub no_commit: bool,
    pub prefer: Prefer,
    /// `--allow-unrelated-histories`.
    pub allow_unrelated: bool,
    /// `--no-verify` (03 §3.3: skip hooks).
    pub no_verify: bool,
}

/// `rebase`'s options (03 §7.1). "Plan the commits first" opens the planner (§7.8, S4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebaseOptions {
    /// `--ignore-date`.
    pub ignore_date: bool,
    /// `--autostash`: the dialog turns it on when there are uncommitted changes.
    pub autostash: bool,
    /// `--update-refs` (git ≥ 2.38): every branch it moves is backed up too (§8.2).
    pub update_refs: bool,
}

/// `reset`'s mode (03 §7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    /// The default.
    #[default]
    Mixed,
    /// Refuses to lose changes.
    Keep,
    /// Also backs up `worktree`, and `files` for untracked or ignored files in the way (§8.6).
    Hard,
}

/// `archive`'s format (03 §7.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ArchiveFormat {
    #[default]
    Zip,
    Tar,
}

/// How `pull` brings the fetched branch in (03 §7.3): its step 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullHow {
    /// `merge --no-edit --no-overwrite-ignore [--no-ff] [--squash] FETCH_HEAD` (the default).
    Merge { no_ff: bool, squash: bool },
    /// `rebase FETCH_HEAD`: the confirmation lists the merges it would flatten; backs up
    /// `heads/<current>`.
    Rebase,
    /// `merge … --ff-only FETCH_HEAD`.
    FastForwardOnly,
}

impl Default for PullHow {
    fn default() -> Self {
        PullHow::Merge {
            no_ff: false,
            squash: false,
        }
    }
}

/// A push's own options (03 §7.2's `push` row): the same whether the push is a plain step or the
/// lease push of an overwrite (§8.4), which carries them in its [`RemoteChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushOptions {
    /// `--set-upstream` (the dialog's default: on); never with a delete.
    pub set_upstream: bool,
    /// `--no-verify` (03 §3.3: skip hooks; the dialog's default: off).
    pub no_verify: bool,
}

/// Where `user` and `user-remove` write (03 §7.7): `config --local` or `--global`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// This repository (the default when its name and email are set here).
    Local,
    Global,
}

/// A remote's name and URLs, as `edit-remote`'s dialog prefills and edits them (03 §7.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    /// The fetch URL.
    pub url: String,
    /// The push URL; `None`: the fetch URL.
    pub push_url: Option<String>,
}

/// The banner's keys (03 §8.5's table): what each runs depends on the operation in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proceed {
    /// `rebase --continue`, `am --continue`, `merge --continue`, `cherry-pick --continue`,
    /// `revert --continue`; each but `am`'s with `GIT_EDITOR=true`.
    Continue,
    /// `rebase --skip`, `am --skip`, `cherry-pick --skip`, `revert --skip` (a merge has none).
    Skip,
    /// `rebase --abort`, `am --abort`, `merge --abort`, `cherry-pick --abort`, `revert --abort`.
    Abort,
    /// `bisect reset`, the one bisect step the panel offers.
    BisectReset,
}

/// An action that runs git, with what it needs (03 §7.1–§7.7, §8.3, §8.5): one variant per
/// [`ActionId`] that is not a view. Branch, tag and remote names are the short names the commands
/// take (`<b>`, `<t>`, `<r>`); paths are from the top level, as the status read gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // 03 §7.1, a commit row.
    /// `tag`: `message` `Some` is annotated (`tag -a -F -`, the message on stdin, may be empty),
    /// `None` lightweight; then pushed to `push` when one is chosen. A name taken: the move form
    /// (`-f`, backup `tags/<t>`).
    Tag {
        at: Oid,
        name: String,
        message: Option<String>,
        push: Option<String>,
    },
    /// `branch`: `branch <b> <c>`, or `checkout --no-overwrite-ignore -b <b> <c>`. A name taken:
    /// the move form (`branch -f`, backup `heads/<b>`; refused for the checked-out branch).
    Branch {
        at: Oid,
        name: String,
        checkout: bool,
    },
    /// `move-branch`: `update-ref -m 'sopen: move <b>' refs/heads/<b> <c> <v>`, backup
    /// `heads/<b>`.
    MoveBranch { branch: String, to: Oid },
    /// `checkout`: `checkout --no-overwrite-ignore --detach <c>`.
    Checkout { at: Oid },
    /// `pick`: the bar's commit or the marked ones, oldest first; `mainline` is a merge's parent
    /// (`-m <n>`, 1…n).
    Pick {
        commits: Vec<Oid>,
        mainline: Option<u32>,
        record_origin: bool,
        no_commit: bool,
    },
    /// `revert`: the bar's commit or the marked ones, newest first.
    Revert {
        commits: Vec<Oid>,
        mainline: Option<u32>,
    },
    /// `merge` (§7.1, §7.2, §7.3): into the current branch.
    Merge { from: Rev, options: MergeOptions },
    /// `rebase` (§7.1, §7.2): the current branch onto `onto`; backup `heads/<current>`.
    Rebase { onto: Rev, options: RebaseOptions },
    /// `reset`: the current branch to `to`; backup `heads/<current>`, and with hard `worktree`
    /// and `files` (§8.6).
    Reset { to: Oid, mode: ResetMode },
    /// `archive` (§7.1–§7.4): `archive --format=zip|tar <rev>`, stdout into 01 §6a's staged file
    /// at `to` (the "Save to…" path field), never over anything.
    Archive {
        rev: Rev,
        format: ArchiveFormat,
        to: PathBuf,
    },
    // §7.2, a local branch's label.
    /// `checkout-branch`: `checkout --no-overwrite-ignore <b>` (no confirmation, §8.1).
    CheckoutBranch { branch: String },
    /// `rename`: `branch -m <b> <new>`.
    Rename { branch: String, to: String },
    /// `delete-branch`: on each of `remotes` first §8.4's lease delete, then `branch -d <b>`
    /// (`-D` with `force`, which git's `not fully merged` refusal asks for in a second dialog).
    DeleteBranch {
        branch: String,
        force: bool,
        remotes: Vec<String>,
    },
    /// `push`: `push [--set-upstream] [--no-verify] <r> refs/heads/<b>:refs/heads/<b>` to each
    /// remote in turn, stopping at the first failure; with `overwrite`, §8.4's lease push, which
    /// keeps `options` ([`RemoteChange::options`]).
    Push {
        branch: String,
        remotes: Vec<String>,
        options: PushOptions,
        overwrite: bool,
    },
    // §7.3, a remote branch's label.
    /// `checkout-remote`: `checkout --no-overwrite-ignore -b <name> --track <r>/<b>`; `pull`
    /// `Some` is the form for a `name` that exists: check it out, then pull `<r>/<b>` into it.
    CheckoutRemote {
        remote: String,
        branch: String,
        name: String,
        pull: Option<PullHow>,
    },
    /// `delete-remote-branch`: §8.4's lease delete; then `branch -d -r <r>/<b>` if the
    /// remote-tracking ref is still there.
    DeleteRemoteBranch { remote: String, branch: String },
    /// `fetch-into`: `fetch … <r> refs/heads/<b>:refs/heads/<b>` (`+…` with `force`, backup
    /// `heads/<b>`).
    FetchInto {
        remote: String,
        branch: String,
        force: bool,
    },
    /// `pull`: step 1 the fetch (network, stoppable), step 2 the merge or rebase of `FETCH_HEAD`.
    Pull {
        remote: String,
        branch: String,
        how: PullHow,
    },
    // §7.4, a tag's label.
    /// `delete-tag`: on `remote` first (§8.4's lease delete of `refs/tags/<t>`), then `tag -d`;
    /// backup `tags/<t>` and `pushed/<r>/tags/<t>`.
    DeleteTag { tag: String, remote: Option<String> },
    /// `push-tag`: to each remote in turn; `replace` a tag that differs there through §8.4's
    /// lease.
    PushTag {
        tag: String,
        remotes: Vec<String>,
        replace: bool,
    },
    /// `move-tag`: `tag -f <t> <c>`, or with `message` `Some` kept annotated (`tag -f -a -F -`);
    /// backup `tags/<t>`.
    MoveTag {
        tag: String,
        to: Oid,
        message: Option<String>,
    },
    // §7.5, a stash row: by its hash where git takes one, by `stash@{n}` where it needs the list
    // entry, after checking `rev-parse stash@{n}` against the hash (a mismatch refreshes instead).
    /// `stash-apply`: `stash apply [--index] <c>`.
    StashApply { stash: StashRow, index: bool },
    /// `stash-pop`: `stash pop [--index] stash@{n}`; backup `stash`.
    StashPop { stash: StashRow, index: bool },
    /// `stash-branch`: `stash branch <b> stash@{n}`; backup `stash`.
    StashBranch { stash: StashRow, name: String },
    /// `stash-drop`: `stash drop stash@{n}`; backup `stash`.
    StashDrop { stash: StashRow },
    // §7.6, the uncommitted row and its files.
    /// `stage`: `add -- <p>…` (no confirmation, §8.1).
    Stage { paths: Vec<String> },
    /// `unstage`: `restore --staged -- <p>…` (git < 2.23: `reset -q -- <p>…`).
    Unstage { paths: Vec<String> },
    /// `commit`: `commit --cleanup=verbatim [--no-verify] -F -`, the message on stdin; `amend`
    /// instead backs up `heads/<current>`.
    Commit {
        message: String,
        amend: bool,
        no_verify: bool,
    },
    /// `amend`: the staged changes into the last commit, keeping its message (`None`) or with
    /// this one; backup `heads/<current>`.
    Amend { message: Option<String> },
    /// `stash`: `stash push [--include-untracked] [-m <message>]`, the message one line.
    Stash {
        message: String,
        include_untracked: bool,
    },
    /// `discard`: the file under the bar or the marked files — tracked ones `checkout HEAD --`,
    /// untracked ones (a file or a `dir/` row) `clean -f -d --`; backup `worktree`, `files`.
    Discard { paths: Vec<String> },
    /// `reset-changes`: `reset --mixed HEAD`, or with `hard` `reset --hard HEAD` (backup
    /// `worktree`).
    ResetChanges { hard: bool },
    /// `clean`: `clean -f [-d]`, ignored files staying; backup `files`.
    Clean { folders: bool },
    /// `ours`: `checkout --ours -- <p>`; backup `files` of the file as it is (§8.5).
    Ours { paths: Vec<String> },
    /// `theirs`: `checkout --theirs -- <p>`; backup `files`.
    Theirs { paths: Vec<String> },
    /// `resolved`: `add -- <p>`.
    Resolved { paths: Vec<String> },
    /// `redo-merge`: `checkout --merge -- <p>`, the conflict and its markers back; backup `files`.
    RedoMerge { paths: Vec<String> },
    // §7.7, the repository.
    /// `fetch`: `fetch --all` with `--prune|--no-prune --prune-tags|--no-prune-tags` (Fetch all,
    /// Cmd/Ctrl+Shift+R, runs at once with both off); pruning backs up what it may delete (§8.2).
    Fetch { prune: bool, prune_tags: bool },
    /// `fetch-remote`: `fetch <r>`, pruning as `fetch`.
    FetchRemote {
        remote: String,
        prune: bool,
        prune_tags: bool,
    },
    /// `prune-remote`: `remote prune <r>`; backup `remotes/<r>/*`, trimmed after.
    PruneRemote { remote: String },
    /// `add-remote`: `remote add <r> <url>`, `remote set-url --push` when `push_url` is given,
    /// then `fetch <r>` when `fetch_now`.
    AddRemote {
        name: String,
        url: String,
        push_url: Option<String>,
        fetch_now: bool,
    },
    /// `edit-remote`: only what changed between `was` and `now` (`remote rename`, `remote
    /// set-url`, `remote set-url --push`).
    EditRemote { was: Remote, now: Remote },
    /// `delete-remote`: `remote remove <r>`; backup `remotes/<r>/*`.
    DeleteRemote { remote: String },
    /// `user`: `config --local|--global user.name <n>`, `user.email <e>`.
    User {
        name: String,
        email: String,
        scope: ConfigScope,
    },
    /// `user-remove`: `config --local|--global --unset-all user.name`, `user.email`.
    UserRemove { scope: ConfigScope },
    // §8.2–§8.3, the Backups list and undo.
    /// `restore` (and `undo`: the newest group written in this window): the chosen entries of
    /// the group `stamp` back to their manifest values, after a group of its own (§8.3).
    Restore {
        stamp: String,
        entries: Vec<RestoreEntry>,
    },
    /// `delete-backup`: the group's refs deleted, the one removal sopen does without a backup
    /// (§8.2).
    DeleteBackup { stamp: String },
    // §8.5, the banner.
    /// `continue`, `skip`, `abort`, `bisect-reset`: the form of §8.5's table for the operation
    /// [`Repo::in_progress`] names.
    Proceed(Proceed),
}

impl Action {
    /// Its id (03 §7).
    pub fn id(&self) -> ActionId {
        match self {
            Action::Tag { .. } => ActionId::Tag,
            Action::Branch { .. } => ActionId::Branch,
            Action::MoveBranch { .. } => ActionId::MoveBranch,
            Action::Checkout { .. } => ActionId::Checkout,
            Action::Pick { .. } => ActionId::Pick,
            Action::Revert { .. } => ActionId::Revert,
            Action::Merge { .. } => ActionId::Merge,
            Action::Rebase { .. } => ActionId::Rebase,
            Action::Reset { .. } => ActionId::Reset,
            Action::Archive { .. } => ActionId::Archive,
            Action::CheckoutBranch { .. } => ActionId::CheckoutBranch,
            Action::Rename { .. } => ActionId::Rename,
            Action::DeleteBranch { .. } => ActionId::DeleteBranch,
            Action::Push { .. } => ActionId::Push,
            Action::CheckoutRemote { .. } => ActionId::CheckoutRemote,
            Action::DeleteRemoteBranch { .. } => ActionId::DeleteRemoteBranch,
            Action::FetchInto { .. } => ActionId::FetchInto,
            Action::Pull { .. } => ActionId::Pull,
            Action::DeleteTag { .. } => ActionId::DeleteTag,
            Action::PushTag { .. } => ActionId::PushTag,
            Action::MoveTag { .. } => ActionId::MoveTag,
            Action::StashApply { .. } => ActionId::StashApply,
            Action::StashPop { .. } => ActionId::StashPop,
            Action::StashBranch { .. } => ActionId::StashBranch,
            Action::StashDrop { .. } => ActionId::StashDrop,
            Action::Stage { .. } => ActionId::Stage,
            Action::Unstage { .. } => ActionId::Unstage,
            Action::Commit { .. } => ActionId::Commit,
            Action::Amend { .. } => ActionId::Amend,
            Action::Stash { .. } => ActionId::Stash,
            Action::Discard { .. } => ActionId::Discard,
            Action::ResetChanges { .. } => ActionId::ResetChanges,
            Action::Clean { .. } => ActionId::Clean,
            Action::Ours { .. } => ActionId::Ours,
            Action::Theirs { .. } => ActionId::Theirs,
            Action::Resolved { .. } => ActionId::Resolved,
            Action::RedoMerge { .. } => ActionId::RedoMerge,
            Action::Fetch { .. } => ActionId::Fetch,
            Action::FetchRemote { .. } => ActionId::FetchRemote,
            Action::PruneRemote { .. } => ActionId::PruneRemote,
            Action::AddRemote { .. } => ActionId::AddRemote,
            Action::EditRemote { .. } => ActionId::EditRemote,
            Action::DeleteRemote { .. } => ActionId::DeleteRemote,
            Action::User { .. } => ActionId::User,
            Action::UserRemove { .. } => ActionId::UserRemove,
            Action::Restore { .. } => ActionId::Restore,
            Action::DeleteBackup { .. } => ActionId::DeleteBackup,
            Action::Proceed(Proceed::Continue) => ActionId::Continue,
            Action::Proceed(Proceed::Skip) => ActionId::Skip,
            Action::Proceed(Proceed::Abort) => ActionId::Abort,
            Action::Proceed(Proceed::BisectReset) => ActionId::BisectReset,
        }
    }
}

/// What [`plan`] knows of the repository: its detection and the panel's last refresh (03 §4.1's
/// reads and §8.5's probe). The integration (S3-C) fills it; S3-A adds the fields its
/// confirmations need beyond these (adds only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The top level: every command's `-C` (03 §3.1).
    pub top: String,
    /// The git directory (`Detection::WorkTree::git_dir`), where a stop looks for `*.lock`.
    pub git_dir: String,
    /// The host's git (03 §2.4): `--update-refs` from 2.38, `restore --staged` from 2.23.
    pub version: GitVersion,
    pub head: Head,
    /// Local branches, remote branches and tags (03 §4.1's refs read).
    pub refs: Vec<RefLabel>,
    pub stashes: Vec<StashRow>,
    pub status: Status,
    pub in_progress: InProgress,
    /// The repository's remotes, by name (`git remote`).
    pub remotes: Vec<String>,
    /// The reads beyond the refresh that [`needs`] asked for and [`inspect`] made (S3-A; the
    /// refresh leaves it empty).
    pub seen: Seen,
}

/// A read [`plan`] needs beyond the refresh: git's words from the subcommand on, run in the top
/// level as a read. Equal words are the same read, whatever action asked.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Need(pub Vec<String>);

impl Need {
    fn of(words: &[&str]) -> Need {
        Need(words.iter().map(|w| w.to_string()).collect())
    }

    /// `git <words>`, as the log would show it.
    pub fn line(&self) -> String {
        let cmd = GitCommand::new(Class::Read, ".", self.0.iter());
        cmd.command_line()
    }
}

/// The reads made for [`plan`] ([`Repo::seen`]): each [`Need`] with what git printed on stdout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    pub reads: Vec<(Need, Vec<u8>)>,
}

impl Seen {
    pub fn get(&self, need: &Need) -> Option<&[u8]> {
        self.reads
            .iter()
            .find(|(n, _)| n == need)
            .map(|(_, out)| out.as_slice())
    }

    /// Records a read, replacing an older one of the same words.
    pub fn put(&mut self, need: Need, stdout: Vec<u8>) {
        self.reads.retain(|(n, _)| n != &need);
        self.reads.push((need, stdout));
    }
}

/// The dialog's confirmation (03 §8.1): exactly what changes, the backups first, the command
/// lines. It changes with the options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confirmation {
    /// The dialog's title: the action (`Reset the current branch here`).
    pub title: String,
    /// A destructive or rewriting action's first line, what is lost without the backup
    /// (`feature has 3 commits on no other branch`, `12 files lose their changes`).
    pub loses: Option<String>,
    /// What changes: refs as `name: old → new` with short hashes, commits by hash and subject,
    /// files by count and the first few paths.
    pub changes: Vec<String>,
    /// The backup refs written first, by their names under the stamp (`heads/main`, §8.2).
    pub backups: Vec<String>,
    /// The exact command lines, as the log will show them.
    pub commands: Vec<String>,
    /// What else the dialog must say: commits already on a remote-tracking branch (a rewrite),
    /// the merges a rebase flattens, permission bits or empty folders a backup does not keep, a
    /// remote overwrite or delete (§8.4).
    pub notes: Vec<String>,
}

/// One overwrite or delete on a remote (03 §8.4), never a plain `--force`: the push endpoints,
/// their values fetched into staging refs at Run (refused when they disagree), the backup
/// `pushed/<r>/…`, then the push with that value as the lease. Only the lease value is resolved
/// at Run; everything else the push sends is here, decided by the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteChange {
    pub remote: String,
    /// The remote's ref (`refs/heads/<b>`, `refs/tags/<t>`).
    pub refname: String,
    pub change: Overwrite,
    /// The push's own options, kept as the action chose them through the lease resolved at Run
    /// (03 §7.2's `push` with overwrite: `--set-upstream`, `--no-verify`); a delete, a replaced
    /// tag and a restore's push have both off.
    pub options: PushOptions,
}

/// What [`RemoteChange`] does to the remote's ref (03 §8.4, step 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overwrite {
    /// `push --force-with-lease=<ref>:<value> [--set-upstream] [--no-verify] <r> <src>:<ref>`: an
    /// overwriting push or a replaced tag; `src` the local side (`refs/heads/<b>`,
    /// `refs/tags/<t>`).
    Replace { src: String },
    /// `push --force-with-lease=<ref>:<value> [--no-verify] <r> --delete <ref>`.
    Delete,
}

/// One step of a [`Plan`], in order; the first failing step stops the rest (03 §3.8).
#[derive(Debug, Clone)]
pub enum Step {
    /// Writes the plan's backup group ([`Plan::backups`], 03 §8.2): the files snapshot and the
    /// staging fetches it needs, then one `update-ref --stdin` transaction under the stamp of
    /// Run. When it fails, nothing after it runs.
    Backup,
    /// One git command, its words final.
    Git(GitCommand),
    /// §8.4's push for one change: its lease the value the backup step fetched at Run, its words
    /// otherwise the change's own, [`RemoteChange::options`] included.
    Remote(RemoteChange),
    /// `archive`'s command, its stdout streamed into 01 §6a's staged file and published at `to`
    /// as it is (a remote archive lands on this machine).
    Archive { command: GitCommand, to: PathBuf },
}

/// What an action will do (03 §7, §8): made by [`plan`], shown by its dialog, run on Enter.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The dialog; `None` runs at once (03 §8.1's "No confirmation": Fetch all, Stage, Unstage,
    /// Check out of a branch).
    pub confirm: Option<Confirmation>,
    /// The backup group's items (03 §8.2), written by [`Step::Backup`] before anything changes;
    /// empty: no group.
    pub backups: Vec<Take>,
    /// The group's reflog message, one line (`sopen: drop abc12345 from main`).
    pub message: String,
    pub steps: Vec<Step>,
    /// It can change files: the integration sends `GitEvent::WorkTreeChanged` after it (02 §7).
    pub changes_files: bool,
}

impl Plan {
    /// Its overwrites and deletes on a remote (03 §8.4), in step order; none for most actions.
    pub fn remote_changes(&self) -> impl Iterator<Item = &RemoteChange> {
        self.steps.iter().filter_map(|step| match step {
            Step::Remote(change) => Some(change),
            _ => None,
        })
    }
}

/// Why [`plan`] refuses an action before anything runs, as the dialog says it: an ignored file
/// in the way (03 §8.6, listing them), a branch move onto the checked-out branch, a mirror's
/// prune, a reset hard or discard during a conflict (naming Abort, §8.5), …
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

/// The plan of `action` on `repo` (03 §7, §8): pure — it reads and runs nothing, so a dialog
/// asks again as its options change. A read it needs that [`Repo::seen`] lacks refuses it with
/// `not read yet: …` ([`needs`] lists them).
pub fn plan(action: &Action, repo: &Repo) -> Result<Plan, Refused> {
    let reads = Reads::new(&repo.seen);
    let drafted = draft(action, repo, &reads);
    let missing = reads.missing.into_inner();
    if let Some(first) = missing.first() {
        return Err(Refused(format!("not read yet: {}", first.line())));
    }
    drafted
}

/// The reads `plan(action, repo)` would make that `repo.seen` does not hold yet (see the module's
/// notes): empty when `plan` has all it needs.
pub fn needs(action: &Action, repo: &Repo) -> Vec<Need> {
    let reads = Reads::new(&repo.seen);
    let _ = draft(action, repo, &reads);
    let mut missing = reads.missing.into_inner();
    missing.dedup();
    missing
}

/// Makes `needs`' reads, on a worker, into `repo.seen`. A read that fails is the error (a
/// `config` or `rev-parse -q` read that finds nothing is an empty answer).
pub fn inspect(
    runner: &GitRunner,
    handle: &Handle,
    repo: &mut Repo,
    needs: &[Need],
) -> Result<(), backup::Failed> {
    let top = repo.top.clone();
    let mut ran = Vec::new();
    for need in needs {
        let (cmd, codes): (GitCommand, &[i32]) = match need.0.first().map(String::as_str) {
            Some(name) if name == backup::FILES_RESTORE.name => (
                GitCommand::snippet(
                    Class::Read,
                    top.as_str(),
                    &backup::FILES_RESTORE,
                    &need.0[1..],
                ),
                &[1],
            ),
            Some("config" | "rev-parse") => (
                GitCommand::new(Class::Read, top.as_str(), need.0.iter()),
                &[1],
            ),
            _ => (
                GitCommand::new(Class::Read, top.as_str(), need.0.iter()),
                &[],
            ),
        };
        let done = runner.run(&cmd, handle);
        let ok =
            matches!(done.ending, super::runner::Ending::Exit(c) if c == 0 || codes.contains(&c));
        if !ok {
            let reason = backup::first_error(&done);
            ran.push(done);
            return Err(backup::Failed { reason, ran });
        }
        repo.seen.put(need.clone(), done.stdout.clone());
        ran.push(done);
    }
    Ok(())
}

/// `name` as a new branch or tag name: refused as git's `check-ref-format --branch` refuses it
/// (G-R1: `x\y`, `a b`, `a..b`, `-x`, `x.lock`), with the reason.
pub fn valid_name(name: &str) -> Result<(), String> {
    let refused = |why: &str| Err(format!("`{name}` is not a valid name: {why}"));
    if name.is_empty() {
        return refused("it is empty");
    }
    if name.starts_with('-') {
        return refused("it starts with '-'");
    }
    if name == "HEAD" {
        return refused("HEAD names the checked-out commit");
    }
    if let Some(c) = name
        .chars()
        .find(|&c| c.is_ascii_control() || " ~^:?*[\\".contains(c))
    {
        return refused(&format!("it holds {c:?}"));
    }
    if name.contains("..") {
        return refused("it holds '..'");
    }
    if name.contains("@{") {
        return refused("it holds '@{'");
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return refused("a '/' starts or ends it, or two follow each other");
    }
    if name.ends_with('.') {
        return refused("it ends with '.'");
    }
    for part in name.split('/') {
        if part.starts_with('.') {
            return refused("a part of it starts with '.'");
        }
        if part.ends_with(".lock") {
            return refused("a part of it ends with '.lock'");
        }
    }
    Ok(())
}

/// The `--git-path`s the probe looks at (03 §8.5), in its argument order.
pub const PROBE_PATHS: [&str; 7] = [
    "rebase-merge",
    "rebase-apply",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "sequencer",
    "BISECT_LOG",
];

/// Once per repository (03 §8.5): `rev-parse --git-path rebase-merge … --git-path BISECT_LOG`,
/// per worktree.
pub fn probe_paths_command(top: &str) -> GitCommand {
    let mut args = vec!["rev-parse".to_string()];
    for p in PROBE_PATHS {
        args.extend(["--git-path".to_string(), p.to_string()]);
    }
    GitCommand::new(Class::Read, top, args)
}

/// [`probe_paths_command`]'s seven lines; `None` when it printed something else.
pub fn probe_paths(stdout: &[u8]) -> Option<Vec<String>> {
    let text = std::str::from_utf8(stdout).ok()?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    (lines.len() == PROBE_PATHS.len() && lines.iter().all(|l| !l.is_empty())).then_some(lines)
}

/// The in-progress probe (03 §8.5): its arguments are [`probe_paths`]' seven paths; it prints
/// `<key> <value>` records, each ending in NUL — `exists <marker>` for each marker there
/// (`rebase-merge`, `rebase-apply`, `applying`, `rebasing`, `MERGE_HEAD`, `CHERRY_PICK_HEAD`,
/// `REVERT_HEAD`, `sequencer`, `BISECT_LOG`), `rebase-merge/`'s `msgnum`, `end`, `head-name`,
/// `onto`, `stopped-sha` and `done-last` (the last line of `done`), `todo-first` (the first
/// line of `sequencer/todo`), `head` (the first line of the first of `MERGE_HEAD`,
/// `CHERRY_PICK_HEAD` and `REVERT_HEAD` that exists) and, with `MERGE_HEAD`, `merge-msg` (the
/// first line of `MERGE_MSG` beside it) — the banner's detail (S3-C). It runs no git.
pub static PROBE: Snippet = Snippet {
    name: "sopen-probe",
    script: concat!(
        r#"rec() { printf '%s %s\0' "$1" "$2"; }; "#,
        r#"at() { case $1 in /*) printf '%s' "$1";; *) printf './%s' "$1";; esac; }; "#,
        r#"rm_=$(at "$1"); ra=$(at "$2"); sq=$(at "$6"); "#,
        r#"n=0; for m in rebase-merge rebase-apply MERGE_HEAD CHERRY_PICK_HEAD REVERT_HEAD sequencer BISECT_LOG; do "#,
        r#"n=$((n+1)); eval "p=\${$n}"; p=$(at "$p"); "#,
        r#"if [ -e "$p" ] || [ -L "$p" ]; then rec exists "$m"; fi; done; "#,
        r#"if [ -d "$ra" ]; then [ -e "$ra/applying" ] && rec exists applying; "#,
        r#"[ -e "$ra/rebasing" ] && rec exists rebasing; fi; "#,
        r#"if [ -d "$rm_" ]; then for f in msgnum end head-name onto stopped-sha; do "#,
        r#"if [ -f "$rm_/$f" ]; then v=; IFS= read -r v < "$rm_/$f"; rec "$f" "$v"; fi; done; "#,
        r#"if [ -f "$rm_/done" ]; then v=$(tail -n 1 "$rm_/done"); rec done-last "$v"; fi; fi; "#,
        r#"if [ -f "$sq/todo" ]; then v=; IFS= read -r v < "$sq/todo"; rec todo-first "$v"; fi; "#,
        r#"for h in "$3" "$4" "$5"; do h=$(at "$h"); "#,
        r#"if [ -f "$h" ]; then v=; IFS= read -r v < "$h"; rec head "$v"; break; fi; done; "#,
        r#"mh=$(at "$3"); if [ -f "$mh" ] && [ -f "${mh%/*}/MERGE_MSG" ]; then "#,
        r#"v=; IFS= read -r v < "${mh%/*}/MERGE_MSG"; rec merge-msg "$v"; fi; "#,
        r#"exit 0"#,
    ),
};

/// The probe's command on every refresh, a read.
pub fn probe_command(top: &str, paths: &[String]) -> GitCommand {
    GitCommand::snippet(Class::Read, top, &PROBE, paths.iter())
}

/// The probe's output as `model::InProgress` (03 §8.5's table: the first marker that exists).
pub fn parse_probe(stdout: &[u8]) -> InProgress {
    let mut exists: Vec<String> = Vec::new();
    let mut rebase = RebaseProgress::default();
    let mut any_rebase = false;
    let mut todo_first = None;
    let mut head: Option<OperationHead> = None;
    let mut merge_msg = None;
    for record in stdout.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let (key, value) = record.split_once(' ').unwrap_or((&record, ""));
        let value = Some(value.to_string());
        match key {
            "exists" => exists.extend(value),
            "msgnum" => rebase.msgnum = value,
            "end" => rebase.end = value,
            "head-name" => rebase.head_name = value,
            "onto" => rebase.onto = value,
            "stopped-sha" => rebase.stopped_sha = value,
            "done-last" => rebase.done_last = value,
            "todo-first" => todo_first = value,
            "head" => {
                head = value.map(|commit| OperationHead {
                    commit,
                    merge_msg: None,
                })
            }
            "merge-msg" => merge_msg = value,
            _ => continue,
        }
        any_rebase |= !matches!(key, "exists" | "todo-first" | "head" | "merge-msg");
    }
    let has = |m: &str| exists.iter().any(|e| e == m);
    let sequence = || match todo_first
        .as_deref()
        .and_then(|l| l.split_whitespace().next())
    {
        Some("revert" | "r") => Sequence::Revert,
        _ => Sequence::Pick,
    };
    let operation = if has("rebase-merge") {
        Some(Operation::Rebase)
    } else if has("rebase-apply") && has("applying") {
        Some(Operation::Am)
    } else if has("rebase-apply") {
        Some(Operation::RebaseApply)
    } else if has("MERGE_HEAD") {
        Some(Operation::Merge)
    } else if has("CHERRY_PICK_HEAD") {
        Some(Operation::CherryPick)
    } else if has("REVERT_HEAD") {
        Some(Operation::Revert)
    } else if has("sequencer") {
        Some(Operation::Sequence(sequence()))
    } else {
        None
    };
    InProgress {
        operation,
        bisect: has("BISECT_LOG"),
        rebase: any_rebase.then_some(rebase),
        todo_first,
        head: head.map(|h| OperationHead { merge_msg, ..h }),
    }
}

/// What a merge merges, from its `MERGE_MSG`'s first line: the first name git quotes in it
/// (`Merge branch 'feature' into main`, `Merge remote-tracking branch 'origin/x'`, `Merge tag
/// 'v1'`); `None` for a message of the owner's own.
fn merged_name(msg: &str) -> Option<&str> {
    let (_, rest) = msg.split_once('\'')?;
    let (name, _) = rest.split_once('\'')?;
    (!name.is_empty()).then_some(name)
}

/// How one step of a run ended (03 §3.8's list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// `done`.
    Done,
    /// `failed`: it stopped the action (an exit, a stop, `outcome unknown`, a refusal at Run).
    Failed,
    /// `not run`: a step after the failed one.
    NotRun,
}

/// One step of a run, as the result dialog lists it (03 §3.8).
#[derive(Debug, Clone)]
pub struct StepResult {
    /// The step's line, as the confirmation listed it.
    pub line: String,
    pub outcome: StepOutcome,
    /// The commands it ran, each as the log keeps it (a backup or a remote change runs several):
    /// the error dialog's text and command line.
    pub ran: Vec<Done>,
}

/// A plan's run: each step's outcome; the panel refreshes after it whatever it says (03 §5).
#[derive(Debug, Clone)]
pub struct Outcome {
    pub steps: Vec<StepResult>,
    /// The stamp of the group written, for the status row's `<what was done> · backup <stamp> ·
    /// Cmd/Ctrl+Z undoes` and the window's undo (§8.3).
    pub backup: Option<String>,
}

impl Outcome {
    /// The run's warning, for the result dialog and the status row in place of the plain undo
    /// text: `backup <stamp> incomplete: …` when the steps were done but the group's `after`
    /// record or its pruning trim could not be written (S3-A review round 1); `<the step> was
    /// refused; your uncommitted changes were set aside in backup <stamp> — restore it to bring
    /// them back` when a restore stopped after its work tree's reset and before the apply (03
    /// §8.3).
    /// The first of them, in step order.
    pub fn warning(&self) -> Option<String> {
        self.steps
            .iter()
            .flat_map(|s| &s.ran)
            .find_map(|d| match &d.ending {
                super::runner::Ending::NotSent(why)
                    if (why.starts_with("backup ") && why.contains(" incomplete: "))
                        || why.contains(" were set aside in backup ") =>
                {
                    Some(why.clone())
                }
                _ => None,
            })
    }
}

/// The in-progress banner (03 §8.5): the git panel's top row, in the warning colour, while git
/// has an operation in progress, whoever started it — the probe's [`InProgress`] and the status
/// read's conflicts, and the keys it offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub state: InProgress,
    /// Unmerged entries (the uncommitted row's `Conflicts` group, §6.1).
    pub conflicts: usize,
}

impl Banner {
    /// The banner for this refresh (03 §5, §8.5); `None` when nothing is in progress.
    pub fn of(in_progress: &InProgress, status: &Status) -> Option<Banner> {
        if in_progress.operation.is_none() && !in_progress.bisect {
            return None;
        }
        let conflicts = status
            .entries
            .iter()
            .filter(|e| matches!(e.state, StatusState::Conflict(_)))
            .count();
        Some(Banner {
            state: in_progress.clone(),
            conflicts,
        })
    }

    /// Its text (03 §8.5): `REBASE 3/7 · stopped at abc12345 <subject> · 2 conflicts`, `AM · 1 conflict`,
    /// `MERGE of feature · 1 conflict`, `CHERRY-PICK abc12345`, `REVERT abc12345`, `BISECT ·
    /// Reset` on a line of its own; an edit stop `stopped to edit abc12345`. The detail is the
    /// probe's `head` record ([`InProgress::head`], S3-C): a merge's name from its `MERGE_MSG`,
    /// else its commit; without the record, the state alone.
    pub fn text(&self) -> String {
        let mut lines = Vec::new();
        if let Some(op) = self.state.operation {
            let mut line = match (op, self.edit_stop()) {
                (_, Some(stop)) => format!("stopped to edit {stop}"),
                (Operation::Rebase, None) => {
                    let mut line = "REBASE".to_string();
                    if let Some(r) = &self.state.rebase {
                        if let (Some(n), Some(end)) = (&r.msgnum, &r.end) {
                            line.push_str(&format!(" {}/{}", n.trim(), end.trim()));
                        }
                        if let Some(sha) = r.stopped_sha.as_deref().map(str::trim) {
                            let short = shown_id(sha);
                            line.push_str(&format!(" · stopped at {short}"));
                            if let Some(subject) = r.done_last.as_deref().and_then(done_subject) {
                                line.push(' ');
                                line.push_str(&subject);
                            }
                        }
                    }
                    line
                }
                (Operation::Am, None) => "AM".into(),
                (Operation::RebaseApply, None) => "REBASE".into(),
                (Operation::Merge, None) => match &self.state.head {
                    Some(h) => {
                        let msg = h.merge_msg.as_deref().and_then(merged_name);
                        format!("MERGE of {}", msg.unwrap_or(shown_id(h.commit.trim())))
                    }
                    None => "MERGE".into(),
                },
                (Operation::CherryPick, None) => self.with_head("CHERRY-PICK"),
                (Operation::Revert, None) => self.with_head("REVERT"),
                (Operation::Sequence(Sequence::Pick), None) => "CHERRY-PICK".into(),
                (Operation::Sequence(Sequence::Revert), None) => "REVERT".into(),
            };
            match self.conflicts {
                0 => {}
                1 => line.push_str(" · 1 conflict"),
                n => line.push_str(&format!(" · {n} conflicts")),
            }
            lines.push(line);
        }
        if self.state.bisect {
            lines.push("BISECT · Reset".into());
        }
        lines.join("\n")
    }

    /// `word` and the operation's commit, 8 digits, when the probe read it.
    fn with_head(&self, word: &str) -> String {
        match &self.state.head {
            Some(h) => format!("{word} {}", shown_id(h.commit.trim())),
            None => word.to_string(),
        }
    }

    /// The keys §8.5's table offers for the state, in order. An edit stop adds §7.6's `amend`
    /// (and S4 its Split), which [`Banner::edit_stop`] says.
    pub fn offers(&self) -> Vec<Proceed> {
        let mut keys = match self.state.operation {
            None => Vec::new(),
            Some(_) if self.edit_stop().is_some() => vec![Proceed::Continue, Proceed::Abort],
            Some(Operation::Merge) => vec![Proceed::Continue, Proceed::Abort],
            Some(_) => vec![Proceed::Continue, Proceed::Skip, Proceed::Abort],
        };
        if self.state.bisect {
            keys.push(Proceed::BisectReset);
        }
        keys
    }

    /// An edit stop (03 §8.5: `stopped to edit abc12345`): a rebase whose last done line is an
    /// `edit`, with no conflict — the commit it stopped at, 8 digits. The banner then also offers
    /// §7.6's `amend` (S4: its Split), which [`Proceed`] does not name.
    pub fn edit_stop(&self) -> Option<&str> {
        if self.state.operation != Some(Operation::Rebase) || self.conflicts > 0 {
            return None;
        }
        let r = self.state.rebase.as_ref()?;
        let mut words = r.done_last.as_deref()?.split_whitespace();
        if !matches!(words.next(), Some("edit" | "e")) {
            return None;
        }
        let sha = r.stopped_sha.as_deref().map(str::trim).or(words.next())?;
        Some(shown_id(sha))
    }
}

/// A commit id from the probe as the banner shows it: 8 digits when it is hex, as git writes
/// it; anything else (the files are the repository's, not git's promise) its first 8
/// characters, never cut inside one.
fn shown_id(id: &str) -> &str {
    let id = id.split_whitespace().next().unwrap_or("");
    if id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return &id[..id.len().min(8)];
    }
    let end = id.char_indices().nth(8).map_or(id.len(), |(i, _)| i);
    &id[..end]
}

/// The subject of a `done` line: `pick <c> # <subject>` (or without `# `, older git).
fn done_subject(line: &str) -> Option<String> {
    let mut words = line.splitn(3, ' ');
    let (_, _, rest) = (words.next()?, words.next()?, words.next()?);
    let rest = rest.strip_prefix("# ").unwrap_or(rest).trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

// =============================================================================================
// Planning (S3-A)

use super::backup::Source;
use std::collections::BTreeSet;
use std::ffi::OsString;

/// What [`draft`] reads from [`Repo::seen`], recording each read it lacks (an empty answer
/// stands in for it, so one pass finds every read that does not depend on another).
pub(crate) struct Reads<'a> {
    seen: &'a Seen,
    pub(crate) missing: RefCell<Vec<Need>>,
}

impl<'a> Reads<'a> {
    pub(crate) fn new(seen: &'a Seen) -> Self {
        Self {
            seen,
            missing: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn get(&self, need: Need) -> Vec<u8> {
        match self.seen.get(&need) {
            Some(out) => out.to_vec(),
            None => {
                let mut missing = self.missing.borrow_mut();
                if !missing.contains(&need) {
                    missing.push(need);
                }
                Vec::new()
            }
        }
    }

    /// NUL-terminated records.
    pub(crate) fn records(&self, need: Need) -> Vec<String> {
        backup::records(&self.get(need))
    }

    /// Lines.
    fn lines(&self, need: Need) -> Vec<String> {
        String::from_utf8_lossy(&self.get(need))
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }
}

fn refused<T>(why: impl Into<String>) -> Result<T, Refused> {
    Err(Refused(why.into()))
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn short(oid: &Oid) -> &str {
    oid.short()
}

/// `<c>`, `refs/heads/<b>`, `refs/remotes/<r>/<b>`, `refs/tags/<t>`: a revision as a command
/// takes it, whole, so no name is read as another.
fn rev_word(rev: &Rev) -> String {
    match rev {
        Rev::Commit(c) => c.as_str().to_string(),
        Rev::Branch(b) => format!("refs/heads/{b}"),
        Rev::RemoteBranch { remote, branch } => format!("refs/remotes/{remote}/{branch}"),
        Rev::Tag(t) => format!("refs/tags/{t}"),
    }
}

/// How a revision is named in a confirmation.
fn rev_shown(rev: &Rev) -> String {
    match rev {
        Rev::Commit(c) => short(c).to_string(),
        Rev::Branch(b) => b.clone(),
        Rev::RemoteBranch { remote, branch } => format!("{remote}/{branch}"),
        Rev::Tag(t) => t.clone(),
    }
}

/// A plan being made.
pub(crate) struct Draft<'a> {
    repo: &'a Repo,
    confirm: bool,
    title: String,
    pub(crate) loses: Vec<String>,
    pub(crate) changes: Vec<String>,
    pub(crate) notes: Vec<String>,
    takes: Vec<Take>,
    steps: Vec<Step>,
    message: String,
    pub(crate) changes_files: bool,
}

impl<'a> Draft<'a> {
    pub(crate) fn new(
        repo: &'a Repo,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            repo,
            confirm: true,
            title: title.into(),
            loses: Vec::new(),
            changes: Vec::new(),
            notes: Vec::new(),
            takes: Vec::new(),
            steps: Vec::new(),
            message: backup::one_line(&message.into()),
            changes_files: false,
        }
    }

    pub(crate) fn command<I, S>(&self, class: Class, args: I) -> GitCommand
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        GitCommand::new(class, self.repo.top.as_str(), args).git_dir(&self.repo.git_dir)
    }

    pub(crate) fn step(&mut self, cmd: GitCommand) {
        self.steps.push(Step::Git(cmd));
    }

    pub(crate) fn write<I, S>(&mut self, args: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let cmd = self.command(Class::Write, args);
        self.step(cmd);
    }

    fn network<I, S>(&mut self, args: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let cmd = self.command(Class::Network, args);
        self.step(cmd);
    }

    /// `prefix` then `paths`, as one command per batch of at most 64 KiB of arguments.
    fn write_paths(&mut self, prefix: &[&str], paths: &[String]) {
        for batch in backup::batches(paths, 1) {
            let mut args: Vec<String> = prefix.iter().map(|w| w.to_string()).collect();
            args.extend(batch);
            self.write(args);
        }
    }

    pub(crate) fn take(&mut self, kind: BackupKind, from: Source) {
        if self.takes.iter().any(|t| t.kind == kind) {
            return;
        }
        self.takes.push(Take {
            kind,
            from,
            pruned_only: false,
        });
    }

    /// A local ref at its value now (03 §8.2).
    pub(crate) fn take_ref(&mut self, refname: &str) {
        if let Some(kind) = backup::kind_for(refname) {
            let value = ref_value(self.repo, refname);
            self.take(
                kind,
                Source::Ref {
                    refname: refname.to_string(),
                    value,
                },
            );
        }
    }

    /// `heads/<current>`, or `detached` (03 §8.2).
    pub(crate) fn take_head(&mut self) -> Result<(), Refused> {
        match &self.repo.head {
            Head::Branch {
                refname,
                lossy: false,
                oid,
            } => {
                let kind = backup::kind_for(refname).expect("a branch");
                self.take(
                    kind,
                    Source::Ref {
                        refname: refname.clone(),
                        value: Some(oid.clone()),
                    },
                );
                Ok(())
            }
            Head::Branch { lossy: true, .. } | Head::Unborn { lossy: true, .. } => {
                refused("the checked-out branch's name is not UTF-8, so sopen cannot name it")
            }
            Head::Detached(oid) => {
                self.take(
                    BackupKind::Detached,
                    Source::Ref {
                        refname: "HEAD".into(),
                        value: Some(oid.clone()),
                    },
                );
                Ok(())
            }
            Head::Unborn { .. } => refused("the branch has no commit yet"),
        }
    }

    /// The `files` backup of these paths (one `files` per group: added to it).
    fn take_files(&mut self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        if let Some(Take {
            from: Source::Files(have),
            ..
        }) = self.takes.iter_mut().find(|t| t.kind == BackupKind::Files)
        {
            for p in paths {
                if !have.contains(&p) {
                    have.push(p);
                }
            }
            return;
        }
        let mut paths = paths;
        paths.dedup();
        self.takes.push(Take {
            kind: BackupKind::Files,
            from: Source::Files(paths),
            pruned_only: false,
        });
    }

    pub(crate) fn finish(self) -> Plan {
        let mut steps = self.steps;
        if !self.takes.is_empty() {
            steps.insert(0, Step::Backup);
        }
        let mut notes = self.notes;
        if self.takes.iter().any(|t| t.kind == BackupKind::Files) {
            notes.push(
                "the files backup keeps each file's bytes, its executable bit and a link's \
                 target; no other permission bits and no empty folders"
                    .into(),
            );
        }
        let mut plan = Plan {
            confirm: None,
            backups: self.takes,
            message: self.message,
            steps,
            changes_files: self.changes_files,
        };
        if self.confirm {
            let commands = plan.steps.iter().map(|s| step_line(s, &plan)).collect();
            plan.confirm = Some(Confirmation {
                title: self.title,
                loses: (!self.loses.is_empty()).then(|| self.loses.join("; ")),
                changes: self.changes,
                backups: plan
                    .backups
                    .iter()
                    .map(|t| backup::kind_name(&t.kind))
                    .collect(),
                commands,
                notes,
            });
        }
        plan
    }
}

/// A local ref's value in the refs read (its own object: an annotated tag's tag object); a
/// branch checked out with no other ref read is HEAD's.
fn ref_value(repo: &Repo, refname: &str) -> Option<Oid> {
    if let Some(r) = repo.refs.iter().find(|r| r.refname == refname && !r.lossy) {
        return Some(r.object.clone());
    }
    match &repo.head {
        Head::Branch {
            refname: name, oid, ..
        } if name == refname => Some(oid.clone()),
        _ => None,
    }
}

fn has_ref(repo: &Repo, refname: &str) -> bool {
    ref_value(repo, refname).is_some()
}

/// The checked-out branch's short name.
fn current(repo: &Repo) -> Option<&str> {
    match &repo.head {
        Head::Branch {
            refname,
            lossy: false,
            ..
        } => refname.strip_prefix("refs/heads/"),
        _ => None,
    }
}

/// The checked-out branch's name, or `HEAD`.
fn head_shown(repo: &Repo) -> String {
    current(repo).map_or_else(|| "HEAD".to_string(), str::to_string)
}

fn conflicts(repo: &Repo) -> Vec<&str> {
    repo.status
        .entries
        .iter()
        .filter(|e| matches!(e.state, StatusState::Conflict(_)))
        .map(|e| e.path.as_str())
        .collect()
}

/// Refused during a conflict: the `worktree` backup cannot be written then (03 §8.5).
fn no_conflicts(repo: &Repo, what: &str) -> Result<(), Refused> {
    let c = conflicts(repo);
    if c.is_empty() {
        return Ok(());
    }
    refused(format!(
        "{what} is refused while {} in conflict: its backup cannot be written then \
         (git's stash create refuses unmerged files). Use Abort first (the banner).",
        plural(c.len(), "file is").replace("1 file is", "a file is")
    ))
}

/// The tracked files with changes now (03 §8.1: `12 files lose their changes`).
fn changed_files(repo: &Repo) -> Vec<&str> {
    repo.status
        .entries
        .iter()
        .filter(|e| !matches!(e.state, StatusState::Untracked))
        .map(|e| e.path.as_str())
        .collect()
}

/// The first few of `paths`, for a confirmation: `a, b, c and 9 more`.
fn few(paths: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut text = paths
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        text.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    text
}

/// The commits only `branch` reaches (03 §8.1's `loses`): from `tip`, not from `not`, nor any
/// other branch, tag or remote branch.
fn only_on(reads: &Reads, tip: &str, not: &[&str], branch: Option<&str>) -> Vec<String> {
    let mut words = vec!["rev-list".to_string(), tip.to_string(), "--not".into()];
    words.extend(not.iter().map(|w| w.to_string()));
    if let Some(b) = branch {
        words.push(format!("--exclude={b}"));
    }
    words.extend(["--branches", "--tags", "--remotes"].map(String::from));
    reads.lines(Need(words))
}

fn loses_commits(d: &mut Draft, reads: &Reads, tip: &str, not: &[&str], branch: &str, how: &str) {
    let only = only_on(reads, tip, not, Some(branch));
    if !only.is_empty() {
        d.loses.push(format!(
            "`{branch}` has {} on no other branch{how}",
            plural(only.len(), "commit")
        ));
    }
}

// --- files in the way (03 §8.6) ------------------------------------------------------------

/// The untracked and ignored entries, a folder with nothing tracked in it as one `dir/`.
fn others(reads: &Reads) -> Vec<String> {
    reads.records(Need::of(&["ls-files", "-z", "--others", "--directory"]))
}

/// The ignored entries alone.
fn ignored(reads: &Reads) -> Vec<String> {
    reads.records(Need::of(&[
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ]))
}

/// An entry collides with a footprint path when it equals one, lies inside one, or contains one.
fn collides(entry: &str, footprint: &BTreeSet<String>) -> bool {
    let e = entry.trim_end_matches('/');
    if footprint.contains(e) {
        return true;
    }
    let mut at = e;
    while let Some((parent, _)) = at.rsplit_once('/') {
        if footprint.contains(parent) {
            return true;
        }
        at = parent;
    }
    let prefix = format!("{e}/");
    footprint
        .range(prefix.clone()..)
        .next()
        .is_some_and(|p| p.starts_with(&prefix))
}

fn diff_paths(reads: &Reads, from: &str, to: &str) -> Vec<String> {
    reads.records(Need(
        [
            "diff-tree",
            "-r",
            "-z",
            "--name-only",
            "--no-renames",
            from,
            to,
        ]
        .map(String::from)
        .to_vec(),
    ))
}

/// What applying (or reverting) commit `c` writes: its diff against `mainline`'s parent, or
/// against its own (a root: the empty tree).
fn commit_paths(reads: &Reads, c: &Oid, mainline: Option<u32>) -> Vec<String> {
    match mainline {
        Some(m) => diff_paths(reads, &format!("{}^{m}", c.as_str()), c.as_str()),
        None => reads.records(Need(
            [
                "diff-tree",
                "-r",
                "-z",
                "--name-only",
                "--no-renames",
                "--no-commit-id",
                "--root",
                c.as_str(),
            ]
            .map(String::from)
            .to_vec(),
        )),
    }
}

/// What a rebase of HEAD onto `onto` writes: the diff from HEAD to `onto`, and every path the
/// commits it replays change.
pub(crate) fn rebase_paths(reads: &Reads, onto: &str) -> Vec<String> {
    let mut paths = diff_paths(reads, "HEAD", onto);
    paths.extend(
        reads.records(Need(
            [
                "log",
                "-z",
                "--name-only",
                "--format=",
                "--no-renames",
                &format!("{onto}..HEAD"),
            ]
            .map(String::from)
            .to_vec(),
        )),
    );
    paths
}

/// What applying a stash writes: its tracked changes, its index, its untracked files.
fn stash_paths(reads: &Reads, stash: &StashRow) -> Vec<String> {
    let s = stash.oid.as_str();
    let mut paths = diff_paths(reads, &format!("{s}^1"), s);
    if stash.index.is_some() {
        paths.extend(diff_paths(reads, &format!("{s}^1"), &format!("{s}^2")));
    }
    if let Some(u) = &stash.untracked {
        paths.extend(
            reads.records(Need(
                [
                    "ls-tree",
                    "-r",
                    "-z",
                    "--name-only",
                    "--full-tree",
                    u.as_str(),
                ]
                .map(String::from)
                .to_vec(),
            )),
        );
    }
    paths
}

/// 03 §8.6's refusal: an ignored file sits at a path `what` writes (git overwrites it, with no
/// flag to stop it).
pub(crate) fn refuse_ignored(reads: &Reads, paths: Vec<String>, what: &str) -> Result<(), Refused> {
    let footprint: BTreeSet<String> = paths.into_iter().collect();
    let hits: Vec<String> = ignored(reads)
        .into_iter()
        .filter(|e| collides(e, &footprint))
        .collect();
    if hits.is_empty() {
        return Ok(());
    }
    refused(format!(
        "{what} would write over ignored files in the way: {}. Move them away, or use Reset \
         hard, which backs them up.",
        few(&hits)
    ))
}

/// The paths a reset `--hard` to `target` writes or removes (its tree and the index now), and
/// what is in their way: every untracked or ignored file there, a folder's files whole — and
/// what `ls-files` never lists (a fifo, a socket, a device) at a path the target has and the
/// index has not, so the backup refuses it by name (03 §8.2, §8.6).
fn in_the_way_of_reset(repo: &Repo, reads: &Reads, target: &str) -> Vec<String> {
    let tree = reads.records(Need(
        ["ls-tree", "-r", "-z", "--name-only", "--full-tree", target]
            .map(String::from)
            .to_vec(),
    ));
    let index: BTreeSet<String> = reads
        .records(Need::of(&["ls-files", "-z"]))
        .into_iter()
        .collect();
    let mut footprint: BTreeSet<String> = tree.iter().cloned().collect();
    footprint.extend(index.iter().cloned());
    let mut files: Vec<String> = Vec::new();
    for entry in others(reads) {
        if !collides(&entry, &footprint) {
            continue;
        }
        if entry.ends_with('/') {
            files.extend(
                reads.records(Need(
                    ["ls-files", "-z", "--others", "--", entry.as_str()]
                        .map(String::from)
                        .to_vec(),
                )),
            );
        } else {
            files.push(entry);
        }
    }
    // What `ls-files --others` does not list, looked at one by one: the target's paths the
    // index lacks, the index's the target lacks and tracked files deleted in the work tree —
    // a folder there (where a tracked file was: `--directory` lists nothing for it) gives its
    // files, a fifo, a socket or a device itself, so the backup refuses it by name.
    let tree_set: BTreeSet<String> = tree.iter().cloned().collect();
    let mut candidates: BTreeSet<String> = tree
        .iter()
        .filter(|p| !index.contains(*p))
        .cloned()
        .collect();
    candidates.extend(index.iter().filter(|p| !tree_set.contains(*p)).cloned());
    candidates.extend(
        repo.status
            .entries
            .iter()
            .filter(|e| {
                matches!(
                    e.state,
                    StatusState::Changed {
                        unstaged: Some(super::model::Change::Deleted),
                        ..
                    }
                )
            })
            .map(|e| e.path.clone()),
    );
    let candidates: Vec<String> = candidates
        .into_iter()
        .filter(|p| !files.contains(p))
        .collect();
    for (path, word, _) in look(reads, &candidates) {
        match word.as_str() {
            "other" => files.push(path),
            "folder" => files.extend(reads.records(Need(vec![
                "ls-files".into(),
                "-z".into(),
                "--others".into(),
                "--".into(),
                format!("{path}/"),
            ]))),
            _ => {}
        }
    }
    files.sort();
    files.dedup();
    files
}

/// The files restore snippet's `c` look at `paths`: each path, its word and detail.
fn look(reads: &Reads, paths: &[String]) -> Vec<(String, String, String)> {
    let words: Vec<String> = paths
        .iter()
        .flat_map(|p| ["c".to_string(), "-".into(), "-".into(), p.clone()])
        .collect();
    let mut found = Vec::new();
    for batch in backup::batches(&words, 4) {
        let mut need = vec![backup::FILES_RESTORE.name.to_string()];
        need.extend(batch);
        for r in backup::restored(&reads.get(Need(need))) {
            found.push((r.path, r.word, r.detail));
        }
    }
    found
}

/// The backups and lines of a reset `--hard` to `target`: `worktree`, and `files` for what is in
/// the way.
fn hard(d: &mut Draft, reads: &Reads, target: &str, what: &str) -> Result<(), Refused> {
    no_conflicts(d.repo, what)?;
    d.take(BackupKind::Worktree, Source::WorkTree);
    let changed = changed_files(d.repo);
    if !changed.is_empty() {
        d.loses.push(format!(
            "{} lose their changes",
            plural(changed.len(), "file")
        ));
        let changed: Vec<String> = changed.iter().map(|p| p.to_string()).collect();
        d.changes
            .push(format!("tracked changes: {}", few(&changed)));
    }
    let files = in_the_way_of_reset(d.repo, reads, target);
    if !files.is_empty() {
        d.changes
            .push(format!("in the way, backed up first: {}", few(&files)));
        d.take_files(files);
    }
    Ok(())
}

// --- remotes, pruning ----------------------------------------------------------------------

/// Each remote's fetch refspecs (`remote.<r>.fetch`).
fn refspecs(reads: &Reads) -> Vec<(String, String)> {
    reads
        .records(Need::of(&[
            "config",
            "-z",
            "--get-regexp",
            r"^remote\..*\.fetch$",
        ]))
        .into_iter()
        .filter_map(|r| {
            let (key, value) = r.split_once('\n')?;
            let name = key.strip_prefix("remote.")?.strip_suffix(".fetch")?;
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

/// A refspec destination with at most one `*` matched against `refname`.
fn matches_glob(pattern: &str, refname: &str) -> bool {
    match pattern.split_once('*') {
        Some((head, tail)) => {
            refname.len() >= head.len() + tail.len()
                && refname.starts_with(head)
                && refname.ends_with(tail)
        }
        None => pattern == refname,
    }
}

/// 03 §8.2 "Pruning": every ref a prune of `remotes` could delete, from the effective
/// configuration — each fetch refspec's destination, and with `tags` every tag — backed up
/// `pruned_only`; a refspec writing into `refs/heads/` (a mirror) refuses it.
fn prune_takes(
    d: &mut Draft,
    reads: &Reads,
    remotes: &[String],
    tags: bool,
) -> Result<(), Refused> {
    let mut patterns = Vec::new();
    for (remote, spec) in refspecs(reads) {
        if !remotes.contains(&remote) || spec.starts_with('^') {
            continue;
        }
        let Some((_, dst)) = spec.trim_start_matches('+').split_once(':') else {
            continue;
        };
        if !(dst.starts_with("refs/remotes/") || dst.starts_with("refs/tags/")) {
            return refused(format!(
                "{remote} fetches into {dst} (`{spec}`), outside refs/remotes/ and refs/tags/, so \
                 pruning it could delete local branches or sopen's backups; it is refused"
            ));
        }
        patterns.push(dst.to_string());
    }
    let mut pruned = Vec::new();
    for r in &d.repo.refs {
        if r.lossy {
            continue;
        }
        let remote_head = r.kind == RefKind::RemoteBranch && r.refname.ends_with("/HEAD");
        let by_spec = patterns.iter().any(|p| matches_glob(p, &r.refname));
        let by_tags = tags && r.kind == RefKind::Tag;
        if (by_spec || by_tags) && !remote_head {
            pruned.push((r.refname.clone(), r.object.clone()));
        }
    }
    for (refname, value) in pruned {
        if let Some(kind) = backup::kind_for(&refname) {
            if d.takes.iter().any(|t| t.kind == kind) {
                continue;
            }
            d.takes.push(Take {
                kind,
                from: Source::Ref {
                    refname,
                    value: Some(value),
                },
                pruned_only: true,
            });
        }
    }
    if !d.takes.is_empty() {
        d.notes.push(format!(
            "every ref the prune could delete is backed up first ({}); those it does not \
             delete are taken out of the group after it",
            plural(d.takes.len(), "ref")
        ));
    }
    Ok(())
}

fn prune_words(prune: bool, prune_tags: bool) -> [&'static str; 2] {
    [
        if prune { "--prune" } else { "--no-prune" },
        if prune_tags {
            "--prune-tags"
        } else {
            "--no-prune-tags"
        },
    ]
}

/// §8.4: an overwrite or delete on `remote`, with its `pushed/…` backup.
fn remote_change(
    d: &mut Draft,
    remote: &str,
    refname: &str,
    change: Overwrite,
    options: PushOptions,
) {
    d.take(
        backup::pushed_kind(remote, refname),
        Source::Pushed {
            remote: remote.to_string(),
            refname: refname.to_string(),
        },
    );
    let what = match &change {
        Overwrite::Replace { .. } => "overwritten",
        Overwrite::Delete => "deleted",
    };
    d.notes.push(format!(
        "{refname} on {remote} is {what} with a lease: its value is fetched from each push URL \
         at Run, backed up, and the push refused if it moved meanwhile"
    ));
    d.steps.push(Step::Remote(RemoteChange {
        remote: remote.to_string(),
        refname: refname.to_string(),
        change,
        options,
    }));
}

/// The note every rewrite gives (03 §7.8.2): commits it rewrites already on a remote branch.
pub(crate) fn on_remotes(d: &mut Draft, reads: &Reads, range: &str) {
    let all = reads.lines(Need(vec!["rev-list".into(), range.into()]));
    let local = reads.lines(Need(vec![
        "rev-list".into(),
        range.into(),
        "--not".into(),
        "--remotes".into(),
    ]));
    let shared = all.len().saturating_sub(local.len());
    if shared > 0 {
        d.notes.push(format!(
            "{} it rewrites {} already on a remote branch: you will need to push with \
             overwrite",
            plural(shared, "commit"),
            if shared == 1 { "is" } else { "are" }
        ));
    }
}

/// 03 §7.1 `rebase`: the merges of the range, which a plain rebase flattens.
fn flattened(d: &mut Draft, reads: &Reads, onto: &str) {
    let merges = reads.lines(Need(vec![
        "rev-list".into(),
        "--merges".into(),
        format!("{onto}..HEAD"),
    ]));
    if !merges.is_empty() {
        let shown: Vec<String> = merges
            .iter()
            .map(|m| m[..m.len().min(8)].to_string())
            .collect();
        d.notes.push(format!(
            "a plain rebase flattens {}: {} (the planner refuses them, 03 §7.8.2)",
            plural(merges.len(), "merge"),
            few(&shown)
        ));
    }
}

fn need_name(name: &str) -> Result<(), Refused> {
    valid_name(name).map_err(Refused)
}

/// The words of a stash's list entry, `stash@{n}`.
fn selector(stash: &StashRow) -> Result<String, Refused> {
    if stash.selector.starts_with("stash@{") {
        Ok(stash.selector.clone())
    } else {
        refused(format!("{} is not a stash list entry", stash.selector))
    }
}

/// `config --local|--global`.
fn scope_word(scope: ConfigScope) -> &'static str {
    match scope {
        ConfigScope::Local => "--local",
        ConfigScope::Global => "--global",
    }
}

/// The `user.name` / `user.email` keys set in `scope`.
fn user_keys(reads: &Reads, scope: ConfigScope) -> Vec<String> {
    reads
        .records(Need(
            [
                "config",
                "-z",
                scope_word(scope),
                "--get-regexp",
                r"^user\.(name|email)$",
            ]
            .map(String::from)
            .to_vec(),
        ))
        .into_iter()
        .filter_map(|r| r.split_once('\n').map(|(k, _)| k.to_string()))
        .collect()
}

// --- the actions ---------------------------------------------------------------------------

/// Refuses a remote name or URL that starts with `-`: git would read it as an option
/// (`--upload-pack=<command>` runs the command; review round 1).
pub fn plain_word(word: &str) -> Result<(), String> {
    if word.starts_with('-') {
        Err(format!(
            "`{}` starts with '-': not a URL sopen will pass to git",
            super::runner::mask_credentials(word)
        ))
    } else {
        Ok(())
    }
}

/// The remote names and URLs `action` passes to git, each checked by [`plain_word`].
fn plain_words(action: &Action) -> Result<(), Refused> {
    let mut words: Vec<&str> = Vec::new();
    match action {
        Action::Tag { push: Some(r), .. }
        | Action::DeleteTag {
            remote: Some(r), ..
        } => words.push(r),
        Action::DeleteBranch { remotes, .. }
        | Action::Push { remotes, .. }
        | Action::PushTag { remotes, .. } => words.extend(remotes.iter().map(String::as_str)),
        Action::CheckoutRemote { remote, .. }
        | Action::DeleteRemoteBranch { remote, .. }
        | Action::FetchInto { remote, .. }
        | Action::Pull { remote, .. }
        | Action::FetchRemote { remote, .. }
        | Action::PruneRemote { remote }
        | Action::DeleteRemote { remote } => words.push(remote),
        Action::AddRemote {
            name,
            url,
            push_url,
            ..
        } => {
            words.extend([name.as_str(), url.as_str()]);
            words.extend(push_url.as_deref());
        }
        Action::EditRemote { was, now } => {
            words.extend([was.name.as_str(), now.name.as_str(), now.url.as_str()]);
            words.extend(now.push_url.as_deref());
        }
        _ => {}
    }
    for w in words {
        plain_word(w).map_err(Refused)?;
    }
    Ok(())
}

fn draft(action: &Action, repo: &Repo, reads: &Reads) -> Result<Plan, Refused> {
    plain_words(action)?;
    let head = head_shown(repo);
    let d = match action {
        Action::Tag {
            at,
            name,
            message,
            push,
        } => {
            need_name(name)?;
            let refname = format!("refs/tags/{name}");
            let taken = ref_value(repo, &refname);
            let mut d = match &taken {
                Some(v) => Draft::new(
                    repo,
                    format!("Move tag `{name}` from {} to {}?", short(v), short(at)),
                    format!("sopen: move tag {name} to {}", short(at)),
                ),
                None => Draft::new(
                    repo,
                    format!("Tag {}", short(at)),
                    format!("sopen: tag {name}"),
                ),
            };
            if taken.is_some() {
                d.take_ref(&refname);
            }
            let mut args = vec!["tag"];
            if taken.is_some() {
                args.push("-f");
            }
            let cmd = match message {
                Some(m) => {
                    args.extend(["-a", "--cleanup=verbatim", "-F", "-", name, at.as_str()]);
                    d.command(Class::Write, args).feed("message", m.as_bytes())
                }
                None => {
                    args.extend([name.as_str(), at.as_str()]);
                    d.command(Class::Write, args)
                }
            };
            d.step(cmd);
            d.changes.push(match &taken {
                Some(v) => format!("{name}: {} → {}", short(v), short(at)),
                None => format!("{name}: new at {}", short(at)),
            });
            if let Some(r) = push {
                d.network(["push", r.as_str(), refname.as_str()]);
            }
            d
        }
        Action::Branch { at, name, checkout } => {
            need_name(name)?;
            let refname = format!("refs/heads/{name}");
            let taken = ref_value(repo, &refname);
            let mut d;
            match &taken {
                Some(v) => {
                    if current(repo) == Some(name.as_str()) {
                        return refused(format!(
                            "`{name}` is checked out, so it is not moved here: use Reset the \
                             current branch here instead"
                        ));
                    }
                    d = Draft::new(
                        repo,
                        format!("Move branch `{name}` from {} to {}?", short(v), short(at)),
                        format!("sopen: move {name} to {}", short(at)),
                    );
                    d.take_ref(&refname);
                    loses_commits(
                        &mut d,
                        reads,
                        &refname,
                        &[at.as_str()],
                        name,
                        ", which the move leaves",
                    );
                    d.write(["branch", "-f", name, at.as_str()]);
                    if *checkout {
                        d.write(["checkout", "--no-overwrite-ignore", name, "--"]);
                    }
                    d.changes
                        .push(format!("{name}: {} → {}", short(v), short(at)));
                }
                None => {
                    d = Draft::new(repo, format!("New branch at {}", short(at)), "");
                    if *checkout {
                        d.write(["checkout", "--no-overwrite-ignore", "-b", name, at.as_str()]);
                    } else {
                        d.write(["branch", name, at.as_str()]);
                    }
                    d.changes.push(format!("{name}: new at {}", short(at)));
                }
            }
            d.changes_files = *checkout;
            d
        }
        Action::MoveBranch { branch, to } => {
            let refname = format!("refs/heads/{branch}");
            if current(repo) == Some(branch.as_str()) {
                return refused(format!(
                    "`{branch}` is checked out: use Reset the current branch here instead"
                ));
            }
            let Some(v) = ref_value(repo, &refname) else {
                return refused(format!("there is no branch `{branch}`"));
            };
            let mut d = Draft::new(
                repo,
                format!("Move branch `{branch}` here"),
                format!("sopen: move {branch} to {}", short(to)),
            );
            d.take_ref(&refname);
            loses_commits(
                &mut d,
                reads,
                &refname,
                &[to.as_str()],
                branch,
                ", which the move leaves",
            );
            let reflog = format!("sopen: move {branch}");
            d.write([
                "update-ref",
                "-m",
                reflog.as_str(),
                refname.as_str(),
                to.as_str(),
                v.as_str(),
            ]);
            d.changes
                .push(format!("{branch}: {} → {}", short(&v), short(to)));
            d
        }
        Action::Checkout { at } => {
            let mut d = Draft::new(repo, format!("Check out {} (detached HEAD)", short(at)), "");
            d.write(["checkout", "--no-overwrite-ignore", "--detach", at.as_str()]);
            d.changes
                .push(format!("HEAD: {head} → {} (detached)", short(at)));
            d.changes_files = true;
            d
        }
        Action::Pick {
            commits,
            mainline,
            record_origin,
            no_commit,
        } => {
            if commits.is_empty() {
                return refused("no commit to cherry-pick");
            }
            let paths = commits
                .iter()
                .flat_map(|c| commit_paths(reads, c, *mainline))
                .collect();
            refuse_ignored(reads, paths, "Cherry-pick")?;
            let mut d = Draft::new(
                repo,
                format!(
                    "Cherry-pick {} onto {head}",
                    plural(commits.len(), "commit")
                ),
                "",
            );
            let mut args = vec!["cherry-pick".to_string()];
            if *no_commit {
                args.push("--no-commit".into());
            }
            if *record_origin {
                args.push("-x".into());
            }
            if let Some(m) = mainline {
                args.extend(["-m".to_string(), m.to_string()]);
            }
            args.extend(commits.iter().map(|c| c.as_str().to_string()));
            d.write(args);
            d.changes.push(format!(
                "{head}: + {}",
                commits.iter().map(short).collect::<Vec<_>>().join(", ")
            ));
            d.changes_files = true;
            d
        }
        Action::Revert { commits, mainline } => {
            if commits.is_empty() {
                return refused("no commit to revert");
            }
            let paths = commits
                .iter()
                .flat_map(|c| commit_paths(reads, c, *mainline))
                .collect();
            refuse_ignored(reads, paths, "Revert")?;
            let mut d = Draft::new(
                repo,
                format!("Revert {} on {head}", plural(commits.len(), "commit")),
                "",
            );
            let mut args = vec!["revert".to_string(), "--no-edit".into()];
            if let Some(m) = mainline {
                args.extend(["-m".to_string(), m.to_string()]);
            }
            args.extend(commits.iter().map(|c| c.as_str().to_string()));
            d.write(args);
            d.changes.push(format!(
                "{head}: + the reverts of {}",
                commits.iter().map(short).collect::<Vec<_>>().join(", ")
            ));
            d.changes_files = true;
            d
        }
        Action::Merge { from, options } => {
            let mut d = Draft::new(repo, format!("Merge {} into {head}", rev_shown(from)), "");
            let mut args = vec!["merge", "--no-edit", "--no-overwrite-ignore"];
            match options.fast_forward {
                FastForward::WhenPossible => {}
                FastForward::Never => args.push("--no-ff"),
                FastForward::Only => args.push("--ff-only"),
            }
            if options.squash {
                args.push("--squash");
            }
            if options.no_commit {
                args.push("--no-commit");
            }
            match options.prefer {
                Prefer::Nothing => {}
                Prefer::Ours => args.extend(["-X", "ours"]),
                Prefer::Theirs => args.extend(["-X", "theirs"]),
            }
            if options.allow_unrelated {
                args.push("--allow-unrelated-histories");
            }
            if options.no_verify {
                args.push("--no-verify");
            }
            // `merge … <b>` as 03 writes it: git's message then names the branch
            // (`Merge branch 'f1'`), as a terminal's merge would.
            let word = match from {
                Rev::Commit(c) => c.as_str().to_string(),
                other => rev_shown(other),
            };
            args.push(&word);
            d.write(args);
            if options.squash && !options.no_commit {
                d.notes.push(
                    "after the squash the Commit dialog opens with git's prepared message".into(),
                );
            }
            d.changes_files = true;
            d
        }
        Action::Rebase { onto, options } => {
            let word = rev_word(onto);
            refuse_ignored(reads, rebase_paths(reads, &word), "Rebase")?;
            let mut d = Draft::new(
                repo,
                format!("Rebase {head} onto {}", rev_shown(onto)),
                format!("sopen: rebase {head} onto {}", rev_shown(onto)),
            );
            d.take_head()?;
            let mut args = vec!["rebase"];
            if options.ignore_date {
                args.push("--ignore-date");
            }
            if options.autostash {
                args.push("--autostash");
            }
            if !options.update_refs && repo.version.at_least(2, 38) {
                // `rebase.updateRefs=true` in the config would move branches the plan has no
                // backup of (review round 1).
                args.push("--no-update-refs");
            }
            if options.update_refs {
                if !repo.version.at_least(2, 38) {
                    return refused(format!(
                        "moving other branches along needs git 2.38; this host has {}",
                        repo.version.text
                    ));
                }
                args.push("--update-refs");
                let moved = reads.lines(Need(vec![
                    "for-each-ref".into(),
                    "--format=%(refname)".into(),
                    "--merged".into(),
                    "HEAD".into(),
                    "--no-merged".into(),
                    word.clone(),
                    "refs/heads/".into(),
                ]));
                for b in moved {
                    if Some(b.as_str())
                        != current(repo).map(|c| format!("refs/heads/{c}")).as_deref()
                    {
                        d.take_ref(&b);
                    }
                }
            }
            args.push(&word);
            d.write(args);
            flattened(&mut d, reads, &word);
            on_remotes(&mut d, reads, &format!("{word}..HEAD"));
            d.changes_files = true;
            d
        }
        Action::Reset { to, mode } => {
            let (flag, name) = match mode {
                ResetMode::Soft => ("--soft", "soft"),
                ResetMode::Mixed => ("--mixed", "mixed"),
                ResetMode::Keep => ("--keep", "keep"),
                ResetMode::Hard => ("--hard", "hard"),
            };
            let mut d = Draft::new(
                repo,
                format!("Reset {head} to {} ({name})", short(to)),
                format!("sopen: reset {head} to {} ({name})", short(to)),
            );
            d.take_head()?;
            if let Some(b) = current(repo) {
                let b = b.to_string();
                loses_commits(
                    &mut d,
                    reads,
                    "HEAD",
                    &[to.as_str()],
                    &b,
                    ", which the reset leaves",
                );
            }
            if *mode == ResetMode::Hard {
                hard(&mut d, reads, to.as_str(), "Reset hard")?;
            }
            d.write(["reset", flag, to.as_str()]);
            if let Some(oid) = repo.head.oid() {
                d.changes
                    .push(format!("{head}: {} → {}", short(oid), short(to)));
            }
            d.changes_files = *mode != ResetMode::Soft;
            d
        }
        Action::Archive { rev, format, to } => {
            let fmt = match format {
                ArchiveFormat::Zip => "--format=zip",
                ArchiveFormat::Tar => "--format=tar",
            };
            let mut d = Draft::new(repo, format!("Save an archive of {}", rev_shown(rev)), "");
            let command = d.command(
                Class::Read,
                ["archive".to_string(), fmt.into(), rev_word(rev)],
            );
            d.steps.push(Step::Archive {
                command,
                to: to.clone(),
            });
            d.changes
                .push(format!("a new file {}, never over anything", to.display()));
            d
        }
        Action::CheckoutBranch { branch } => {
            if current(repo) == Some(branch.as_str()) {
                return refused(format!("`{branch}` is checked out already"));
            }
            let mut d = Draft::new(repo, format!("Check out {branch}"), "");
            d.confirm = false;
            d.write(["checkout", "--no-overwrite-ignore", branch, "--"]);
            d.changes_files = true;
            d
        }
        Action::Rename { branch, to } => {
            need_name(to)?;
            if has_ref(repo, &format!("refs/heads/{to}")) {
                return refused(format!("a branch `{to}` exists"));
            }
            let mut d = Draft::new(repo, format!("Rename {branch}"), "");
            d.write(["branch", "-m", branch, to]);
            d.changes.push(format!("{branch} → {to}"));
            d
        }
        Action::DeleteBranch {
            branch,
            force,
            remotes,
        } => {
            let refname = format!("refs/heads/{branch}");
            if current(repo) == Some(branch.as_str()) {
                return refused(format!("`{branch}` is checked out"));
            }
            let Some(v) = ref_value(repo, &refname) else {
                return refused(format!("there is no branch `{branch}`"));
            };
            let mut d = Draft::new(
                repo,
                format!("Delete branch {branch}"),
                format!("sopen: delete {branch}"),
            );
            d.take_ref(&refname);
            loses_commits(&mut d, reads, &refname, &[], branch, "");
            for r in remotes {
                remote_change(
                    &mut d,
                    r,
                    &refname,
                    Overwrite::Delete,
                    PushOptions {
                        set_upstream: false,
                        no_verify: false,
                    },
                );
            }
            d.write(["branch", if *force { "-D" } else { "-d" }, branch]);
            d.changes.push(format!("{branch}: {} → deleted", short(&v)));
            d
        }
        Action::Push {
            branch,
            remotes,
            options,
            overwrite,
        } => {
            if remotes.is_empty() {
                return refused("no remote chosen");
            }
            let refname = format!("refs/heads/{branch}");
            let mut d = Draft::new(
                repo,
                format!("Push {branch}"),
                format!("sopen: push {branch} with overwrite"),
            );
            for r in remotes {
                if *overwrite {
                    remote_change(
                        &mut d,
                        r,
                        &refname,
                        Overwrite::Replace {
                            src: refname.clone(),
                        },
                        *options,
                    );
                } else {
                    let mut args = vec!["push".to_string()];
                    if options.set_upstream {
                        args.push("--set-upstream".into());
                    }
                    if options.no_verify {
                        args.push("--no-verify".into());
                    }
                    args.extend([r.clone(), format!("{refname}:{refname}")]);
                    d.network(args);
                }
                d.changes.push(format!("{r}/{branch}"));
            }
            d
        }
        Action::CheckoutRemote {
            remote,
            branch,
            name,
            pull,
        } => {
            need_name(name)?;
            match pull {
                None => {
                    if has_ref(repo, &format!("refs/heads/{name}")) {
                        return refused(format!(
                            "a branch `{name}` exists: check it out and pull into it instead"
                        ));
                    }
                    let mut d =
                        Draft::new(repo, format!("Check out {remote}/{branch} as {name}"), "");
                    let track = format!("{remote}/{branch}");
                    d.write([
                        "checkout",
                        "--no-overwrite-ignore",
                        "-b",
                        name,
                        "--track",
                        track.as_str(),
                    ]);
                    d.changes.push(format!("{name}: new, tracking {track}"));
                    d.changes_files = true;
                    d
                }
                Some(how) => {
                    let mut d = Draft::new(
                        repo,
                        format!("Check out {name} and pull {remote}/{branch} into it"),
                        format!("sopen: pull {remote}/{branch} into {name}"),
                    );
                    d.write(["checkout", "--no-overwrite-ignore", name, "--"]);
                    pull_steps(&mut d, reads, remote, branch, *how, Some(name))?;
                    d.changes_files = true;
                    d
                }
            }
        }
        Action::DeleteRemoteBranch { remote, branch } => {
            let refname = format!("refs/heads/{branch}");
            let mut d = Draft::new(
                repo,
                format!("Delete {branch} on {remote}"),
                format!("sopen: delete {branch} on {remote}"),
            );
            remote_change(
                &mut d,
                remote,
                &refname,
                Overwrite::Delete,
                PushOptions {
                    set_upstream: false,
                    no_verify: false,
                },
            );
            d.write([
                "update-ref",
                "-d",
                &format!("refs/remotes/{remote}/{branch}"),
            ]);
            d.changes.push(format!("{remote}/{branch}: deleted"));
            d
        }
        Action::FetchInto {
            remote,
            branch,
            force,
        } => {
            let refname = format!("refs/heads/{branch}");
            if current(repo) == Some(branch.as_str()) {
                return refused(format!("`{branch}` is checked out: pull into it instead"));
            }
            if !has_ref(repo, &refname) {
                return refused(format!("there is no local branch `{branch}`"));
            }
            let mut d = Draft::new(
                repo,
                format!("Fetch {remote}/{branch} into {branch}"),
                format!("sopen: fetch {remote}/{branch} into {branch}"),
            );
            if *force {
                d.take_ref(&refname);
                loses_commits(&mut d, reads, &refname, &[], branch, "");
            }
            let spec = if *force {
                format!("+{refname}:{refname}")
            } else {
                format!("{refname}:{refname}")
            };
            d.network(["fetch", remote.as_str(), spec.as_str()]);
            d
        }
        Action::Pull {
            remote,
            branch,
            how,
        } => {
            let mut d = Draft::new(
                repo,
                format!("Pull {remote}/{branch} into {head}"),
                format!("sopen: pull {remote}/{branch} into {head}"),
            );
            pull_steps(&mut d, reads, remote, branch, *how, None)?;
            d.changes_files = true;
            d
        }
        Action::DeleteTag { tag, remote } => {
            let refname = format!("refs/tags/{tag}");
            let Some(v) = ref_value(repo, &refname) else {
                return refused(format!("there is no tag `{tag}`"));
            };
            let mut d = Draft::new(
                repo,
                format!("Delete tag {tag}"),
                format!("sopen: delete tag {tag}"),
            );
            d.take_ref(&refname);
            if let Some(r) = remote {
                remote_change(
                    &mut d,
                    r,
                    &refname,
                    Overwrite::Delete,
                    PushOptions {
                        set_upstream: false,
                        no_verify: false,
                    },
                );
            }
            d.write(["tag", "-d", tag]);
            d.changes.push(format!("{tag}: {} → deleted", short(&v)));
            d
        }
        Action::PushTag {
            tag,
            remotes,
            replace,
        } => {
            if remotes.is_empty() {
                return refused("no remote chosen");
            }
            let refname = format!("refs/tags/{tag}");
            let mut d = Draft::new(
                repo,
                format!("Push tag {tag}"),
                format!("sopen: replace tag {tag}"),
            );
            for r in remotes {
                if *replace {
                    remote_change(
                        &mut d,
                        r,
                        &refname,
                        Overwrite::Replace {
                            src: refname.clone(),
                        },
                        PushOptions {
                            set_upstream: false,
                            no_verify: false,
                        },
                    );
                } else {
                    d.network(["push", r.as_str(), refname.as_str()]);
                }
            }
            d
        }
        Action::MoveTag { tag, to, message } => {
            let refname = format!("refs/tags/{tag}");
            let Some(v) = ref_value(repo, &refname) else {
                return refused(format!("there is no tag `{tag}`"));
            };
            let mut d = Draft::new(
                repo,
                format!("Move tag {tag} here"),
                format!("sopen: move tag {tag} to {}", short(to)),
            );
            d.take_ref(&refname);
            let cmd = match message {
                Some(m) => d
                    .command(
                        Class::Write,
                        [
                            "tag",
                            "-f",
                            "-a",
                            "--cleanup=verbatim",
                            "-F",
                            "-",
                            tag,
                            to.as_str(),
                        ],
                    )
                    .feed("message", m.as_bytes()),
                None => d.command(Class::Write, ["tag", "-f", tag, to.as_str()]),
            };
            d.step(cmd);
            d.changes
                .push(format!("{tag}: {} → {}", short(&v), short(to)));
            d
        }
        Action::StashApply { stash, index } => {
            refuse_ignored(reads, stash_paths(reads, stash), "Apply")?;
            let mut d = Draft::new(repo, format!("Apply {}", stash.selector), "");
            let mut args = vec!["stash", "apply"];
            if *index {
                args.push("--index");
            }
            args.push(stash.oid.as_str());
            d.write(args);
            d.changes_files = true;
            d
        }
        Action::StashPop { stash, index } => {
            refuse_ignored(reads, stash_paths(reads, stash), "Pop")?;
            let sel = selector(stash)?;
            let mut d = Draft::new(
                repo,
                format!("Pop {sel}"),
                format!("sopen: pop {sel} {}", short(&stash.oid)),
            );
            d.take(
                BackupKind::Stash,
                Source::Stash {
                    commit: stash.oid.clone(),
                    listed: true,
                },
            );
            let mut args = vec!["stash", "pop"];
            if *index {
                args.push("--index");
            }
            args.push(&sel);
            d.write(args);
            d.notes.push(format!(
                "{sel} is checked against {} before it runs",
                short(&stash.oid)
            ));
            d.changes_files = true;
            d
        }
        Action::StashBranch { stash, name } => {
            need_name(name)?;
            let mut paths = stash_paths(reads, stash);
            paths.extend(diff_paths(reads, "HEAD", stash.base.as_str()));
            refuse_ignored(reads, paths, "New branch from the stash")?;
            let sel = selector(stash)?;
            let mut d = Draft::new(
                repo,
                format!("New branch {name} from {sel}"),
                format!("sopen: branch {name} from {sel} {}", short(&stash.oid)),
            );
            d.take(
                BackupKind::Stash,
                Source::Stash {
                    commit: stash.oid.clone(),
                    listed: true,
                },
            );
            d.write(["stash", "branch", name, sel.as_str()]);
            d.changes_files = true;
            d
        }
        Action::StashDrop { stash } => {
            let sel = selector(stash)?;
            let mut d = Draft::new(
                repo,
                format!("Drop {sel}"),
                format!("sopen: drop {sel} {}", short(&stash.oid)),
            );
            d.take(
                BackupKind::Stash,
                Source::Stash {
                    commit: stash.oid.clone(),
                    listed: true,
                },
            );
            d.write(["stash", "drop", sel.as_str()]);
            d.changes.push(format!(
                "{sel}: {} {} → dropped",
                short(&stash.oid),
                stash.subject
            ));
            d
        }
        Action::Stage { paths } => {
            let mut d = Draft::new(repo, "Stage", "");
            d.confirm = false;
            d.write_paths(&["add", "--"], paths);
            d
        }
        Action::Unstage { paths } => {
            let mut d = Draft::new(repo, "Unstage", "");
            d.confirm = false;
            if repo.version.at_least(2, 23) {
                d.write_paths(&["restore", "--staged", "--"], paths);
            } else {
                d.write_paths(&["reset", "-q", "--"], paths);
            }
            d
        }
        Action::Commit {
            message,
            amend,
            no_verify,
        } => {
            let mut d = Draft::new(
                repo,
                if *amend {
                    format!("Amend the last commit of {head}")
                } else {
                    format!("Commit to {head}")
                },
                format!("sopen: amend {head}"),
            );
            let mut args = vec!["commit"];
            if *amend {
                d.take_head()?;
                args.push("--amend");
                on_remotes(&mut d, reads, "HEAD^!");
            }
            args.push("--cleanup=verbatim");
            if *no_verify {
                args.push("--no-verify");
            }
            args.extend(["-F", "-"]);
            let cmd = d
                .command(Class::Write, args)
                .feed("message", message.as_bytes());
            d.step(cmd);
            d
        }
        Action::Amend { message } => {
            let mut d = Draft::new(
                repo,
                format!("Add the staged changes to the last commit of {head}"),
                format!("sopen: amend {head}"),
            );
            d.take_head()?;
            on_remotes(&mut d, reads, "HEAD^!");
            let cmd = match message {
                // The kept message keeps its bytes (03 §3.3): without `--cleanup=verbatim` git
                // strips its blank lines and trailing spaces again.
                None => d.command(
                    Class::Write,
                    ["commit", "--amend", "--no-edit", "--cleanup=verbatim"],
                ),
                Some(m) => d
                    .command(
                        Class::Write,
                        ["commit", "--amend", "--cleanup=verbatim", "-F", "-"],
                    )
                    .feed("message", m.as_bytes()),
            };
            d.step(cmd);
            d
        }
        Action::Stash {
            message,
            include_untracked,
        } => {
            let mut d = Draft::new(repo, "Stash the changes", "");
            let mut args = vec!["stash".to_string(), "push".into()];
            if *include_untracked {
                args.push("--include-untracked".into());
            }
            let line = backup::one_line(message);
            if !line.trim().is_empty() {
                args.extend(["-m".to_string(), line]);
            }
            // No path is named here; under `GIT_LITERAL_PATHSPECS=1` git's own clean of the
            // untracked files it stashed matches nothing and leaves them (git 2.56.1), so this
            // one command runs without it.
            let cmd = d.command(Class::Write, args).patterns();
            d.step(cmd);
            d.changes_files = true;
            d
        }
        Action::Discard { paths } => {
            let mut tracked = Vec::new();
            let mut untracked = Vec::new();
            let mut files = Vec::new();
            for p in paths {
                let state = repo.status.entries.iter().find(|e| &e.path == p);
                if state.is_some_and(|e| e.state == StatusState::Untracked) {
                    if p.ends_with('/') {
                        files.extend(
                            reads.records(Need(
                                [
                                    "ls-files",
                                    "-z",
                                    "--others",
                                    "--exclude-standard",
                                    "--",
                                    p.as_str(),
                                ]
                                .map(String::from)
                                .to_vec(),
                            )),
                        );
                    } else {
                        files.push(p.clone());
                    }
                    untracked.push(p.clone());
                } else {
                    tracked.push(p.clone());
                }
            }
            let mut d = Draft::new(
                repo,
                format!("Discard the changes of {}", plural(paths.len(), "file")),
                format!("sopen: discard {}", plural(paths.len(), "file")),
            );
            if !tracked.is_empty() {
                no_conflicts(repo, "Discard of tracked files")?;
                d.take(BackupKind::Worktree, Source::WorkTree);
                d.write_paths(&["checkout", "HEAD", "--"], &tracked);
                d.changes.push(format!("tracked: {}", few(&tracked)));
            }
            if !untracked.is_empty() {
                d.changes
                    .push(format!("untracked, removed: {}", few(&files)));
                d.take_files(files);
                d.write_paths(&["clean", "-f", "-d", "--"], &untracked);
            }
            d.loses.push(format!(
                "{} lose their changes",
                plural(paths.len(), "file")
            ));
            d.changes_files = true;
            d
        }
        Action::ResetChanges { hard: is_hard } => {
            let mut d = Draft::new(
                repo,
                if *is_hard {
                    "Reset the changes (discard all tracked changes)"
                } else {
                    "Reset the changes (unstage all)"
                },
                "sopen: reset the changes",
            );
            if *is_hard {
                hard(&mut d, reads, "HEAD", "Reset the changes (hard)")?;
                d.write(["reset", "--hard", "HEAD"]);
            } else {
                d.write(["reset", "--mixed", "HEAD"]);
            }
            d.changes_files = true;
            d
        }
        Action::Clean { folders } => {
            let mut d = Draft::new(
                repo,
                "Remove untracked files",
                "sopen: remove untracked files",
            );
            let listed = reads.records(Need::of(&[
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
                "--directory",
            ]));
            let files: Vec<String> = if *folders {
                let all = reads.records(Need::of(&[
                    "ls-files",
                    "-z",
                    "--others",
                    "--exclude-standard",
                ]));
                let empty: Vec<String> = listed
                    .iter()
                    .filter(|e| e.ends_with('/') && !all.iter().any(|f| f.starts_with(e.as_str())))
                    .cloned()
                    .collect();
                if !empty.is_empty() {
                    d.notes.push(format!(
                        "empty folders are removed and not kept: {}",
                        few(&empty)
                    ));
                }
                all
            } else {
                listed.into_iter().filter(|e| !e.ends_with('/')).collect()
            };
            if !files.is_empty() {
                d.loses.push(format!(
                    "{} are removed",
                    plural(files.len(), "untracked file")
                ));
                d.changes.push(few(&files));
            }
            d.take_files(files);
            d.write(if *folders {
                vec!["clean", "-f", "-d"]
            } else {
                vec!["clean", "-f"]
            });
            d.changes_files = true;
            d
        }
        Action::Ours { paths } | Action::Theirs { paths } | Action::RedoMerge { paths } => {
            let (flag, what) = match action {
                Action::Ours { .. } => ("--ours", "Take ours"),
                Action::Theirs { .. } => ("--theirs", "Take theirs"),
                _ => ("--merge", "Redo the merge"),
            };
            let mut d = Draft::new(
                repo,
                format!("{what}: {}", few(paths)),
                format!("sopen: {} {}", what.to_lowercase(), few(paths)),
            );
            d.take_files(paths.clone());
            let mut prefix = vec!["checkout", flag, "--"];
            prefix.truncate(3);
            d.write_paths(&prefix, paths);
            d.changes_files = true;
            d
        }
        Action::Resolved { paths } => {
            let mut d = Draft::new(repo, format!("Mark resolved: {}", few(paths)), "");
            d.write_paths(&["add", "--"], paths);
            d
        }
        Action::Fetch { prune, prune_tags } => {
            let mut d = Draft::new(repo, "Fetch all remotes", "sopen: fetch all, pruning");
            if *prune || *prune_tags {
                prune_takes(&mut d, reads, &repo.remotes, *prune_tags)?;
            } else {
                d.confirm = false;
            }
            let mut args = vec!["fetch", "--all"];
            args.extend(prune_words(*prune, *prune_tags));
            d.network(args);
            d
        }
        Action::FetchRemote {
            remote,
            prune,
            prune_tags,
        } => {
            let mut d = Draft::new(
                repo,
                format!("Fetch {remote}"),
                format!("sopen: fetch {remote}, pruning"),
            );
            if *prune || *prune_tags {
                prune_takes(&mut d, reads, std::slice::from_ref(remote), *prune_tags)?;
            }
            let mut args = vec!["fetch", remote.as_str()];
            args.extend(prune_words(*prune, *prune_tags));
            d.network(args);
            d
        }
        Action::PruneRemote { remote } => {
            let mut d = Draft::new(
                repo,
                format!("Prune {remote}"),
                format!("sopen: prune {remote}"),
            );
            prune_takes(&mut d, reads, std::slice::from_ref(remote), false)?;
            d.network(["remote", "prune", remote]);
            d
        }
        Action::AddRemote {
            name,
            url,
            push_url,
            fetch_now,
        } => {
            let mut d = Draft::new(repo, format!("Add remote {name}"), "");
            d.write(["remote", "add", name, url]);
            if let Some(p) = push_url.as_ref().filter(|p| !p.is_empty()) {
                d.write(["remote", "set-url", "--push", name, p]);
            }
            if *fetch_now {
                d.network(["fetch", name.as_str()]);
            }
            d
        }
        Action::EditRemote { was, now } => {
            let mut d = Draft::new(repo, format!("Edit remote {}", was.name), "");
            if was.name != now.name {
                d.write(["remote", "rename", was.name.as_str(), now.name.as_str()]);
            }
            let r = now.name.as_str();
            if was.url != now.url {
                d.write(["remote", "set-url", r, now.url.as_str()]);
            }
            if was.push_url != now.push_url {
                match now.push_url.as_ref().filter(|p| !p.is_empty()) {
                    Some(p) => d.write(["remote", "set-url", "--push", r, p]),
                    None => {
                        let key = format!("remote.{r}.pushurl");
                        d.write(["config", "--unset-all", key.as_str()]);
                    }
                }
            }
            if d.steps.is_empty() {
                return refused("nothing changed");
            }
            d
        }
        Action::DeleteRemote { remote } => {
            let mut d = Draft::new(
                repo,
                format!("Delete remote {remote}"),
                format!("sopen: delete remote {remote}"),
            );
            let prefix = format!("refs/remotes/{remote}/");
            let tracking: Vec<String> = repo
                .refs
                .iter()
                .filter(|r| {
                    r.refname.starts_with(&prefix) && !r.lossy && !r.refname.ends_with("/HEAD")
                })
                .map(|r| r.refname.clone())
                .collect();
            for r in &tracking {
                d.take_ref(r);
            }
            d.write(["remote", "remove", remote]);
            d
        }
        Action::User { name, email, scope } => {
            if name.trim().is_empty() || email.trim().is_empty() {
                return refused("a name and an email are both needed");
            }
            let mut d = Draft::new(repo, "Name and email", "");
            let s = scope_word(*scope);
            d.write(["config", s, "user.name", name]);
            d.write(["config", s, "user.email", email]);
            if *scope == ConfigScope::Global {
                for key in user_keys(reads, ConfigScope::Local) {
                    d.write(["config", "--local", "--unset-all", key.as_str()]);
                }
            }
            d
        }
        Action::UserRemove { scope } => {
            let keys = user_keys(reads, *scope);
            let mut d = Draft::new(repo, "Remove name and email", "");
            for key in &keys {
                d.write(["config", scope_word(*scope), "--unset-all", key.as_str()]);
            }
            if keys.is_empty() && reads.missing.borrow().is_empty() {
                return refused("no name or email is set there");
            }
            d
        }
        Action::Restore { stamp, entries } => restore_draft(repo, reads, stamp, entries)?,
        Action::DeleteBackup { stamp } => {
            let refs = reads.lines(Need(vec![
                "for-each-ref".into(),
                "--format=delete %(refname) %(objectname)".into(),
                format!("{}{stamp}/", backup::PREFIX),
            ]));
            if refs.is_empty() && reads.missing.borrow().is_empty() {
                return refused(format!("there is no backup {stamp}"));
            }
            let mut d = Draft::new(repo, format!("Delete the backup {stamp}"), "");
            let mut input = refs.join("\n");
            input.push('\n');
            let cmd = d
                .command(Class::Write, ["update-ref", "--stdin"])
                .feed("ref transaction", input.into_bytes());
            d.step(cmd);
            d.notes.push(
                "the one removal sopen does without a backup: the group's refs are deleted".into(),
            );
            d
        }
        Action::Proceed(p) => proceed_draft(repo, reads, *p)?,
    };
    Ok(d.finish())
}

/// A pull's two steps (03 §7.3): the fetch (network, stoppable), then the merge or rebase of
/// `FETCH_HEAD`; with rebase the branch is backed up and the confirmation lists the merges it
/// flattens. `into`: the branch checked out first (Check out's pull), else the current one.
fn pull_steps(
    d: &mut Draft,
    reads: &Reads,
    remote: &str,
    branch: &str,
    how: PullHow,
    into: Option<&str>,
) -> Result<(), Refused> {
    let source = format!("refs/remotes/{remote}/{branch}");
    if how == PullHow::Rebase {
        if has_ref(d.repo, &source) {
            refuse_ignored(reads, rebase_paths(reads, &source), "The pull's rebase")?;
            flattened(d, reads, &source);
            d.notes.push(format!(
                "the paths the rebase writes are checked against {remote}/{branch} as last \
                 fetched; the fetch may bring more"
            ));
        }
        match into {
            Some(b) => d.take_ref(&format!("refs/heads/{b}")),
            None => d.take_head()?,
        }
    }
    d.network(["fetch", remote, &format!("refs/heads/{branch}")]);
    match how {
        PullHow::Merge { no_ff, squash } => {
            let mut args = vec!["merge", "--no-edit", "--no-overwrite-ignore"];
            if no_ff {
                args.push("--no-ff");
            }
            if squash {
                args.push("--squash");
            }
            args.push("FETCH_HEAD");
            d.write(args);
        }
        PullHow::FastForwardOnly => {
            d.write([
                "merge",
                "--no-edit",
                "--no-overwrite-ignore",
                "--ff-only",
                "FETCH_HEAD",
            ]);
        }
        PullHow::Rebase => {
            if d.repo.version.at_least(2, 38) {
                d.write(["rebase", "--no-update-refs", "FETCH_HEAD"]);
            } else {
                d.write(["rebase", "FETCH_HEAD"]);
            }
        }
    }
    Ok(())
}

/// 03 §8.5's table: Continue, Skip, Abort for the operation in progress, Bisect Reset.
fn proceed_draft<'a>(repo: &'a Repo, reads: &Reads, p: Proceed) -> Result<Draft<'a>, Refused> {
    if p == Proceed::BisectReset {
        if !repo.in_progress.bisect {
            return refused("no bisect is in progress");
        }
        let mut d = Draft::new(repo, "Reset the bisect", "");
        d.write(["bisect", "reset"]);
        d.changes_files = true;
        return Ok(d);
    }
    let Some(op) = repo.in_progress.operation else {
        return refused("no operation is in progress");
    };
    let verb = match op {
        Operation::Rebase | Operation::RebaseApply => "rebase",
        Operation::Am => "am",
        Operation::Merge => "merge",
        Operation::CherryPick | Operation::Sequence(Sequence::Pick) => "cherry-pick",
        Operation::Revert | Operation::Sequence(Sequence::Revert) => "revert",
    };
    let (flag, word) = match p {
        Proceed::Continue => ("--continue", "Continue"),
        Proceed::Skip => ("--skip", "Skip"),
        _ => ("--abort", "Abort"),
    };
    if p == Proceed::Skip {
        if op == Operation::Merge {
            return refused("a merge has no Skip");
        }
        if matches!(verb, "cherry-pick" | "revert") && !repo.version.at_least(2, 23) {
            return refused(format!(
                "{verb} --skip needs git 2.23; this host has {}",
                repo.version.text
            ));
        }
    }
    if op == Operation::Rebase && matches!(p, Proceed::Continue | Proceed::Skip) {
        refuse_comment_char(reads)?;
    }
    let mut d = Draft::new(repo, format!("{word} the {verb}"), "");
    let mut cmd = d.command(Class::Write, [verb, flag]);
    if p == Proceed::Continue && verb != "am" {
        cmd = cmd.env("GIT_EDITOR", "true");
    }
    d.step(cmd);
    d.changes_files = true;
    Ok(d)
}

/// An interactive rebase's todo is read with the repository's comment character: set to anything
/// but `#` (`core.commentChar`, or git 2.45's `core.commentString`), a `pick` line can read as a
/// comment and its commit is dropped (03 §7.8.2, *verified, git 2.56.1* with `p`). A history
/// edit's own start passes `-c core.commentChar=#`, which a Continue or a Skip cannot: git reads
/// the todo again under the repository's setting. So both fail closed (S4-A review round 1):
/// start or finish such a rebase in a terminal.
pub(crate) fn refuse_comment_char(reads: &Reads) -> Result<(), Refused> {
    for key in ["core.commentChar", "core.commentString"] {
        let value = reads.get(Need::of(&["config", "--get", key]));
        let value = String::from_utf8_lossy(&value);
        let value = value.strip_suffix('\n').unwrap_or(&value);
        if !value.is_empty() && value != "#" {
            return refused(format!(
                "{key} is '{value}': start or finish this rebase in a terminal"
            ));
        }
    }
    Ok(())
}

fn oid_of(value: &Value) -> Option<&Oid> {
    match value {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

/// The restore of the group `stamp` (03 §8.3): its own group first, of every value it is about
/// to replace, then each chosen entry by its row of §8.3's table, in §8.3's order: HEAD's
/// checks and the ref transaction (what can refuse) before the work tree's `reset --hard HEAD`,
/// then HEAD's moves, the changes applied, the files. The move then finds no local change for
/// `reset --keep` to refuse (it refused the redo of a hard reset's undo when it came first), and
/// an untracked or ignored file in its way refuses the restore here, before anything runs.
fn restore_draft<'a>(
    repo: &'a Repo,
    reads: &Reads,
    stamp: &str,
    entries: &[RestoreEntry],
) -> Result<Draft<'a>, Refused> {
    let chosen: Vec<&RestoreEntry> = entries
        .iter()
        .filter(|e| e.chosen && (e.now != e.before || e.replace))
        .collect();
    if chosen.is_empty() {
        return refused("nothing chosen differs from the backup");
    }
    let mut d = Draft::new(
        repo,
        format!("Restore the backup {stamp}"),
        format!("sopen: restore {stamp}"),
    );
    // The commit the work tree's changes are applied onto: a chosen HEAD item's backup, whose
    // move comes first, else HEAD as it is.
    let lands_on: Option<&Oid> = chosen
        .iter()
        .filter(|e| matches!(e.how, RestoreHow::CheckedOut | RestoreHow::Detached))
        .filter_map(|e| e.backup.as_ref().map(|b| &b.oid))
        .next_back()
        .or(repo.head.oid());
    let mut heads: Heads = Heads::default();
    let mut wipe: Vec<Step> = Vec::new();
    let mut ref_lines: Vec<String> = Vec::new();
    let mut later: Vec<Step> = Vec::new();
    let mut paths: Vec<&RestoreEntry> = Vec::new();
    for e in &chosen {
        let backup_oid = e.backup.as_ref().map(|b| b.oid.clone());
        match (e.how, &e.subject) {
            (RestoreHow::Ref, Subject::Ref(refname)) => {
                let now = oid_of(&e.now).cloned();
                if let Some(kind) = backup::kind_for(refname) {
                    d.take(
                        kind,
                        Source::Ref {
                            refname: refname.clone(),
                            value: now.clone(),
                        },
                    );
                }
                let line = match (&backup_oid, &now) {
                    (Some(b), Some(n)) => format!("update {refname} {} {}", b.as_str(), n.as_str()),
                    (Some(b), None) => format!("create {refname} {}", b.as_str()),
                    (None, Some(n)) => format!("delete {refname} {}", n.as_str()),
                    (None, None) => continue,
                };
                ref_lines.push(line);
                d.changes.push(format!(
                    "{refname}: {} → {}",
                    now.as_ref().map_or("absent", |o| o.short()),
                    backup_oid.as_ref().map_or("absent", |o| o.short())
                ));
            }
            (RestoreHow::CheckedOut, Subject::Ref(refname)) => {
                let (Some(b), Some(n)) = (&backup_oid, oid_of(&e.now)) else {
                    return refused(format!("{refname} is checked out: it cannot be deleted"));
                };
                d.take(
                    backup::kind_for(refname).expect("a branch"),
                    Source::Ref {
                        refname: refname.clone(),
                        value: Some(n.clone()),
                    },
                );
                head_back(&mut d, reads, stamp, &mut heads, refname, b, n);
            }
            (RestoreHow::Detached, _) => {
                let (Some(b), Some(n)) = (&backup_oid, oid_of(&e.now)) else {
                    return refused("HEAD has no commit to restore");
                };
                d.take(
                    BackupKind::Detached,
                    Source::Ref {
                        refname: "HEAD".into(),
                        value: Some(n.clone()),
                    },
                );
                head_back(&mut d, reads, stamp, &mut heads, "HEAD (detached)", b, n);
            }
            (RestoreHow::Stash, Subject::Stash { commit }) => match (&e.before, &e.backup) {
                (Value::Object(_), Some(b)) => {
                    d.take(
                        BackupKind::Stash,
                        Source::Stash {
                            commit: commit.clone(),
                            listed: false,
                        },
                    );
                    later.push(Step::Git(d.command(
                        Class::Write,
                        ["stash", "store", "-m", b.subject.as_str(), b.oid.as_str()],
                    )));
                    d.changes
                        .push(format!("stash@{{0}}: {} {}", commit.short(), b.subject));
                }
                _ => {
                    let Some(k) = repo.stashes.iter().find(|s| &s.oid == commit) else {
                        continue;
                    };
                    let sel = selector(k)?;
                    d.take(
                        BackupKind::Stash,
                        Source::Stash {
                            commit: commit.clone(),
                            listed: true,
                        },
                    );
                    later.push(Step::Git(
                        d.command(Class::Write, ["stash", "drop", sel.as_str()]),
                    ));
                    d.changes
                        .push(format!("{sel}: {} → dropped", commit.short()));
                }
            },
            (RestoreHow::WorkTree, _) => {
                hard(&mut d, reads, "HEAD", "Restoring the work tree")?;
                wipe.push(Step::Git(
                    d.command(Class::Write, ["reset", "--hard", "HEAD"]),
                ));
                if let (Value::Object(_), Some(b)) = (&e.before, &backup_oid) {
                    refuse_in_the_way_of_apply(&d, reads, b)?;
                    later.push(Step::Git(
                        d.command(Class::Write, ["stash", "apply", "--index", b.as_str()]),
                    ));
                    let base = reads.lines(Need(vec![
                        "rev-parse".into(),
                        "-q".into(),
                        "--verify".into(),
                        format!("{}^1", b.as_str()),
                    ]));
                    let onto = lands_on.map(|o| o.as_str().to_string());
                    if !base.is_empty() && base.first() != onto.as_ref() {
                        d.notes.push(
                            "HEAD has moved since the backup: the changes are applied onto the \
                             current HEAD, may conflict, and a redo is then not exact"
                                .into(),
                        );
                    }
                    d.changes
                        .push("the tracked changes as they were (staged and unstaged)".into());
                } else {
                    d.changes.push("the tracked changes removed (clean)".into());
                }
                d.changes_files = true;
            }
            (RestoreHow::Files, Subject::Path(_)) => paths.push(e),
            (RestoreHow::Pushed, Subject::Pushed { remote, refname }) => {
                let change = match &e.backup {
                    Some(b) => Overwrite::Replace {
                        src: b.refname.clone(),
                    },
                    None => Overwrite::Delete,
                };
                d.take(
                    backup::pushed_kind(remote, refname),
                    Source::Pushed {
                        remote: remote.clone(),
                        refname: refname.clone(),
                    },
                );
                later.push(Step::Remote(RemoteChange {
                    remote: remote.clone(),
                    refname: refname.clone(),
                    change,
                    options: PushOptions {
                        set_upstream: false,
                        no_verify: false,
                    },
                }));
                d.changes.push(format!("{refname} on {remote}"));
            }
            _ => return refused("a restore entry does not match its subject"),
        }
    }
    refuse_in_the_way_of_move(&d, reads, &heads.keeps)?;
    // 03 §8.3's order: what can refuse first — every HEAD item's check, then the ref transaction
    // (compare-and-swap) — so a refusal there leaves the work tree as it is; then the work tree's
    // reset to the HEAD the checks saw (the one §8.6's look in `hard` read), HEAD's moves, `stash
    // apply` (in `later`), and the files last, where the moves made room.
    let mut steps = heads.checks;
    if !ref_lines.is_empty() {
        let mut input = ref_lines.join("\n");
        input.push('\n');
        steps.push(Step::Git(
            d.command(Class::Write, ["update-ref", "--stdin"])
                .feed("ref transaction", input.into_bytes()),
        ));
    }
    steps.append(&mut wipe);
    steps.append(&mut heads.moves);
    steps.extend(later);
    if !paths.is_empty() {
        files_restore(&mut d, reads, stamp, &paths, &mut steps);
    }
    d.steps = steps;
    Ok(d)
}

/// 03 §8.6 for the work tree's restore (review round 1): `stash apply --index <backup>` writes
/// the paths the backup changes (its work tree and its index against its base), and overwrites
/// an ignored file there with no flag to stop it; any untracked or ignored entry there that the
/// reset before it does not back up and remove refuses the restore, naming it.
fn refuse_in_the_way_of_apply(d: &Draft, reads: &Reads, backup_oid: &Oid) -> Result<(), Refused> {
    let b = backup_oid.as_str();
    let mut writes = diff_paths(reads, &format!("{b}^1"), b);
    writes.extend(diff_paths(reads, &format!("{b}^1"), &format!("{b}^2")));
    let footprint: BTreeSet<String> = writes.into_iter().collect();
    let taken: Vec<&String> = d
        .takes
        .iter()
        .filter_map(|t| match &t.from {
            Source::Files(paths) => Some(paths),
            _ => None,
        })
        .flatten()
        .collect();
    let hits: Vec<String> = others(reads)
        .into_iter()
        .filter(|e| collides(e, &footprint))
        .filter(|e| {
            let at = e.trim_end_matches('/');
            !taken
                .iter()
                .any(|t| t.as_str() == at || t.starts_with(&format!("{at}/")))
        })
        .collect();
    if hits.is_empty() {
        return Ok(());
    }
    refused(format!(
        "restoring the tracked changes would write over untracked or ignored files: {}. Move \
         them away first.",
        few(&hits)
    ))
}

/// A restore's HEAD items (03 §8.3): their checks, their moves, and the commits the `--keep`
/// moves go to.
#[derive(Default)]
struct Heads {
    checks: Vec<Step>,
    moves: Vec<Step>,
    keeps: Vec<Oid>,
}

/// 03 §8.6 for HEAD's `reset --keep <backup>` in a restore: it refuses an untracked or ignored
/// entry at a path the backup's commit has and HEAD and the index lack — after the work tree's
/// reset, by then. So the restore refuses first, naming them; one the work tree's reset backs up
/// and removes first is not in the way.
fn refuse_in_the_way_of_move(d: &Draft, reads: &Reads, keeps: &[Oid]) -> Result<(), Refused> {
    if keeps.is_empty() {
        return Ok(());
    }
    let names = |rev: &str| -> BTreeSet<String> {
        reads
            .records(Need(
                ["ls-tree", "-r", "-z", "--name-only", "--full-tree", rev]
                    .map(String::from)
                    .to_vec(),
            ))
            .into_iter()
            .collect()
    };
    let mut here = names("HEAD");
    here.extend(reads.records(Need::of(&["ls-files", "-z"])));
    let mut footprint: BTreeSet<String> = BTreeSet::new();
    for b in keeps {
        footprint.extend(names(b.as_str()).into_iter().filter(|p| !here.contains(p)));
    }
    let taken: Vec<&String> = d
        .takes
        .iter()
        .filter_map(|t| match &t.from {
            Source::Files(paths) => Some(paths),
            _ => None,
        })
        .flatten()
        .collect();
    let hits: Vec<String> = others(reads)
        .into_iter()
        .filter(|e| collides(e, &footprint))
        .filter(|e| {
            let at = e.trim_end_matches('/');
            !taken
                .iter()
                .any(|t| t.as_str() == at || t.starts_with(&format!("{at}/")))
        })
        .collect();
    if hits.is_empty() {
        return Ok(());
    }
    refused(format!(
        "putting HEAD back would write over untracked or ignored files: {}. Move them away \
         first.",
        few(&hits)
    ))
}

/// Puts HEAD — the checked-out branch, or a detached HEAD — back to `backup` (03 §8.3), after
/// checking it is still `now`: the inverse of a reset uses that reset's own mode (coordinator,
/// 2026-11-23), read from the group's manifest — `reset --soft` (HEAD only) and `reset --mixed`
/// (HEAD and the index) never touch the working files, which already hold the backup's content;
/// anything else is `reset --keep`, which refuses rather than lose a local change (a hard reset's
/// files come back from its `files` backup). Adds the check and the move to `heads`, and a
/// `--keep` move's target to its `keeps`.
fn head_back(
    d: &mut Draft,
    reads: &Reads,
    stamp: &str,
    heads: &mut Heads,
    what: &str,
    backup_oid: &Oid,
    now: &Oid,
) {
    let manifest = reads.get(Need(vec![
        "cat-file".into(),
        "commit".into(),
        format!("{}{stamp}/manifest", backup::PREFIX),
    ]));
    let text = String::from_utf8_lossy(&manifest);
    let moved_by = text
        .split_once("\n\n")
        .and_then(|(_, message)| backup::Manifest::parse(message).and_then(|m| m.moved_by));
    let (flag, how) = match moved_by {
        Some(ResetMode::Soft) => (
            "--soft",
            "HEAD goes back; the index and working files are left as they are",
        ),
        Some(ResetMode::Mixed) => (
            "--mixed",
            "HEAD and the index go back; working files are left as they are",
        ),
        _ => (
            "--keep",
            "reset --keep: refused rather than lose a local change",
        ),
    };
    let check = d
        .command(Class::Write, ["update-ref", "--stdin"])
        .feed("ref transaction", format!("verify HEAD {}\n", now.as_str()));
    let back = d.command(Class::Write, ["reset", flag, backup_oid.as_str()]);
    heads.checks.push(Step::Git(check));
    heads.moves.push(Step::Git(back));
    if flag == "--keep" {
        heads.keeps.push(backup_oid.clone());
    }
    d.changes.push(format!(
        "{what}: {} → {} ({how})",
        now.short(),
        backup_oid.short()
    ));
    if flag == "--keep" {
        d.changes_files = true;
    }
}

/// The `files` rows of a restore: their own backup of what is there (for "replace": the leaf or
/// the parent in the way, a folder's files whole), then the files restore snippet, `w` (never
/// over anything), `r` (replace) or `d` (absent before: `clean -f`).
fn files_restore(
    d: &mut Draft,
    reads: &Reads,
    stamp: &str,
    entries: &[&RestoreEntry],
    steps: &mut Vec<Step>,
) {
    let wanted: Vec<String> = entries
        .iter()
        .filter_map(|e| match &e.subject {
            Subject::Path(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    let tree: Vec<backup::TreeEntry> = if entries.iter().any(|e| oid_of(&e.before).is_some()) {
        backup::tree_entries(&reads.get(Need(vec![
            "ls-tree".into(),
            "-r".into(),
            "-z".into(),
            "--full-tree".into(),
            format!("{}{stamp}/files", backup::PREFIX),
        ])))
    } else {
        Vec::new()
    };
    let looked = look(reads, &wanted);
    let mut take: Vec<String> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    for e in entries {
        let Subject::Path(p) = &e.subject else {
            continue;
        };
        let seen = looked.iter().find(|(path, _, _)| path == p);
        let word = seen.map_or("free", |(_, w, _)| w.as_str());
        match word {
            "folder" if e.replace => take.extend(reads.records(Need(vec![
                "ls-files".into(),
                "-z".into(),
                "--cached".into(),
                "--others".into(),
                "--".into(),
                format!("{p}/"),
            ]))),
            "notfolder" if e.replace => {
                if let Some((_, _, component)) = seen {
                    take.push(component.clone());
                }
            }
            "folder" | "notfolder" => {}
            _ => take.push(p.clone()),
        }
        match oid_of(&e.before) {
            Some(_) => {
                let Some(t) = tree.iter().find(|t| &t.path == p) else {
                    continue;
                };
                let op = if e.replace { "r" } else { "w" };
                words.extend([
                    op.to_string(),
                    t.mode.clone(),
                    t.oid.as_str().to_string(),
                    p.clone(),
                ]);
            }
            None => words.extend(["d".to_string(), "-".into(), "-".into(), p.clone()]),
        }
    }
    take.sort();
    take.dedup();
    d.take_files(take);
    let replaced = entries.iter().filter(|e| e.replace).count();
    d.changes.push(format!(
        "{} written back, never over anything{}",
        plural(entries.len(), "path"),
        if replaced > 0 {
            format!(" ({replaced} replacing what is there, backed up first)")
        } else {
            String::new()
        }
    ));
    for batch in backup::batches(&words, 4) {
        steps.push(Step::Git(
            GitCommand::snippet(
                Class::Write,
                d.repo.top.as_str(),
                &backup::FILES_RESTORE,
                batch,
            )
            .git_dir(&d.repo.git_dir),
        ));
    }
    d.changes_files = true;
}

// =============================================================================================
// Running (S3-A)

/// A placeholder for the lease value in a remote change's line.
const LEASE: &str = "%LEASE%";

/// A remote change's push (03 §8.4 step 5), its lease `value` (`""`: must not exist).
fn remote_command(change: &RemoteChange, top: &str, value: &str) -> GitCommand {
    let mut args = vec![
        "push".to_string(),
        format!("--force-with-lease={}:{value}", change.refname),
    ];
    match &change.change {
        Overwrite::Replace { src } => {
            if change.options.set_upstream {
                args.push("--set-upstream".into());
            }
            if change.options.no_verify {
                args.push("--no-verify".into());
            }
            args.extend([change.remote.clone(), format!("{src}:{}", change.refname)]);
        }
        Overwrite::Delete => {
            if change.options.no_verify {
                args.push("--no-verify".into());
            }
            args.extend([
                change.remote.clone(),
                "--delete".into(),
                change.refname.clone(),
            ]);
        }
    }
    GitCommand::new(Class::Network, top, args)
}

/// A step's line in the confirmation and the result dialog: a git command's
/// [`GitCommand::command_line`]; a backup's kinds under `refs/sopen-backup/<stamp>/`; a remote
/// change's push with `<value at Run>` for its lease; an archive's command and where it goes.
pub fn step_line(step: &Step, plan: &Plan) -> String {
    match step {
        Step::Backup => {
            let mut names: Vec<String> = plan
                .backups
                .iter()
                .map(|t| backup::kind_name(&t.kind))
                .collect();
            names.push("manifest".into());
            format!(
                "backup {} under {}<stamp>/",
                names.join(", "),
                backup::PREFIX
            )
        }
        Step::Git(cmd) => cmd.command_line(),
        Step::Remote(change) => remote_command(change, ".", LEASE)
            .command_line()
            .replace(LEASE, "<value at Run>"),
        Step::Archive { command, to } => {
            format!("{} → {}", command.command_line(), to.display())
        }
    }
}

/// `stash@{n}`.
fn is_selector(word: &str) -> bool {
    word.strip_prefix("stash@{")
        .and_then(|r| r.strip_suffix('}'))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// 03 §7.5: every `stash@{n}` a step names is checked against the stash the plan backs up
/// before anything runs; a mismatch (the list changed) refuses the whole run.
fn check_stashes(
    plan: &Plan,
    top: &str,
    runner: &GitRunner,
    handle: &Handle,
) -> Result<Vec<Done>, Vec<Done>> {
    let Some(commit) = plan.backups.iter().find_map(|t| match &t.from {
        Source::Stash {
            commit,
            listed: true,
        } => Some(commit.clone()),
        _ => None,
    }) else {
        return Ok(Vec::new());
    };
    let mut ran = Vec::new();
    let mut selectors: Vec<String> = plan
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Git(cmd) => Some(cmd.args()),
            _ => None,
        })
        .flatten()
        .filter_map(|w| w.to_str().filter(|w| is_selector(w)).map(str::to_string))
        .collect();
    selectors.sort();
    selectors.dedup();
    if selectors.len() > 1 {
        let line = format!("rev-parse {}", selectors.join(" "));
        ran.push(backup::refusal(
            runner,
            top,
            &line,
            &format!(
                "a plan acts on one stash entry; this one names {}",
                selectors.join(", ")
            ),
        ));
        return Err(ran);
    }
    for step in &plan.steps {
        let Step::Git(cmd) = step else { continue };
        for word in cmd.args() {
            let Some(word) = word.to_str().filter(|w| is_selector(w)) else {
                continue;
            };
            let check = GitCommand::new(Class::Read, top, ["rev-parse", "-q", "--verify", word]);
            let done = runner.run(&check, handle);
            let now = String::from_utf8_lossy(&done.stdout).trim().to_string();
            let line = done.entry.line.clone();
            let ok = matches!(done.ending, super::runner::Ending::Exit(0 | 1));
            ran.push(done);
            if !ok {
                return Err(ran);
            }
            if now != commit.as_str() {
                let shown = if now.is_empty() {
                    "gone"
                } else {
                    &now[..now.len().min(8)]
                };
                ran.push(backup::refusal(
                    runner,
                    top,
                    &line,
                    &format!(
                        "the stash list changed: {word} is now {shown}, not {}; nothing was \
                         done, and the refresh reads the list again",
                        commit.short()
                    ),
                ));
                return Err(ran);
            }
        }
    }
    Ok(ran)
}

/// 03 §8.6 for a pull's rebase, again at Run (review round 1): its first check ran against the
/// remote branch as last fetched; once the fetch has brought `FETCH_HEAD`, the paths the rebase
/// writes are listed again — the diff from HEAD to `FETCH_HEAD` and every path the replayed
/// commits change — against the ignored files, and any collision stops it before it runs.
fn recheck_fetched(top: &str, runner: &GitRunner, handle: &Handle) -> (bool, Vec<Done>) {
    let mut ran = Vec::new();
    let mut read = |args: &[&str]| -> Option<Vec<String>> {
        let done = runner.run(
            &GitCommand::new(Class::Read, top, args.iter().copied()),
            handle,
        );
        let out = done.ok().then(|| backup::records(&done.stdout));
        ran.push(done);
        out
    };
    let ignored = read(&[
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ]);
    let diff = read(&[
        "diff-tree",
        "-r",
        "-z",
        "--name-only",
        "--no-renames",
        "HEAD",
        "FETCH_HEAD",
    ]);
    let replayed = read(&[
        "log",
        "-z",
        "--name-only",
        "--format=",
        "--no-renames",
        "FETCH_HEAD..HEAD",
    ]);
    let (Some(ignored), Some(diff), Some(replayed)) = (ignored, diff, replayed) else {
        return (false, ran);
    };
    let footprint: BTreeSet<String> = diff.into_iter().chain(replayed).collect();
    let hits: Vec<String> = ignored
        .into_iter()
        .filter(|e| collides(e, &footprint))
        .collect();
    if hits.is_empty() {
        return (true, ran);
    }
    ran.push(backup::refusal(
        runner,
        top,
        "git rebase FETCH_HEAD",
        &format!(
            "the fetch brought paths where ignored files are: {}; nothing was rebased (the \
             fetch stays). Move them away, or use Reset hard, which backs them up.",
            few(&hits)
        ),
    ));
    (false, ran)
}

/// A `rebase … FETCH_HEAD` step (a pull's second step).
fn rebases_fetched(cmd: &GitCommand) -> bool {
    let args = cmd.args();
    args.first().is_some_and(|a| a == "rebase") && args.last().is_some_and(|a| a == "FETCH_HEAD")
}

/// How the pruning trim ended.
enum Trimmed {
    /// The group stays (with or without refs taken out of it).
    Kept,
    /// Nothing was pruned: the group went whole.
    Gone,
    /// The trim could not be read or written: the group holds more than was pruned.
    Failed(String),
}

/// The archive step: `command`'s stdout streamed into 01 §6a's staged file and published at
/// `to`, never over anything.
fn archive(
    command: &GitCommand,
    to: &std::path::Path,
    top: &str,
    runner: &GitRunner,
    handle: &Handle,
) -> (bool, Vec<Done>) {
    use std::io::Write as _;
    let line = command.command_line();
    if std::fs::symlink_metadata(to).is_ok() {
        let why = format!(
            "{} exists: an archive is never saved over anything",
            to.display()
        );
        return (false, vec![backup::refusal(runner, top, &line, &why)]);
    }
    let command = command.clone().max_record(super::runner::MAX_OUTPUT);
    let mut ran: Option<Done> = None;
    let saved = crate::gui::binary::save_new(to, |file| {
        let mut running = match runner.start(&command, handle) {
            Ok(r) => r,
            Err(done) => {
                ran = Some(*done);
                anyhow::bail!("git archive was not sent");
            }
        };
        let mut written: u64 = 0;
        let mut failed: Option<std::io::Error> = None;
        while let Some(record) = running.read_record(b'\n') {
            if failed.is_none() {
                if let Err(e) = file.write_all(&record).and_then(|_| file.write_all(b"\n")) {
                    failed = Some(e);
                }
            }
            written += record.len() as u64 + 1;
        }
        let done = running.finish();
        let total = done.entry.stdout_bytes;
        let ok = done.ok();
        ran = Some(done);
        if let Some(e) = failed {
            return Err(e.into());
        }
        anyhow::ensure!(ok, "git archive failed");
        if written == total + 1 {
            // The last record had no newline; the one written after it goes.
            file.set_len(total)?;
        }
        Ok(total)
    });
    let mut dones: Vec<Done> = ran.into_iter().collect();
    match saved {
        Ok(_) => (true, dones),
        Err(e) => {
            if dones.iter().all(|d| d.ok()) {
                dones.push(backup::refusal(runner, top, &line, &e.to_string()));
            }
            (false, dones)
        }
    }
}

/// §8.2 "Pruning": after a pruning command, the backups of refs it did not delete (still at
/// the same value) are taken out of the group in one transaction; when nothing was pruned the
/// group goes whole.
fn trim(
    plan: &Plan,
    record: &backup::Record,
    top: &str,
    runner: &GitRunner,
    handle: &Handle,
    ran: &mut Vec<Done>,
) -> Trimmed {
    let candidates: Vec<(&BackupKind, &str, &Oid)> = plan
        .backups
        .iter()
        .filter(|t| t.pruned_only)
        .filter_map(|t| match &t.from {
            Source::Ref {
                refname,
                value: Some(v),
            } => Some((&t.kind, refname.as_str(), v)),
            _ => None,
        })
        .collect();
    if candidates.is_empty() {
        return Trimmed::Kept;
    }
    let read = GitCommand::new(
        Class::Read,
        top,
        [
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/remotes/",
            "refs/tags/",
        ],
    );
    let done = runner.run(&read, handle);
    let ok = done.ok();
    let now: Vec<(String, String)> = String::from_utf8_lossy(&done.stdout)
        .lines()
        .filter_map(|l| {
            l.split_once(' ')
                .map(|(o, r)| (r.to_string(), o.to_string()))
        })
        .collect();
    ran.push(done);
    if !ok {
        return Trimmed::Failed(
            "the refs the prune left could not be read, so the group holds more than was pruned"
                .into(),
        );
    }
    let mut lines = Vec::new();
    let mut kept = 0;
    for (kind, refname, v) in &candidates {
        let still = now.iter().any(|(r, o)| r == refname && o == v.as_str());
        let Some(b) = record.backup_ref(kind) else {
            continue;
        };
        if still {
            lines.push(format!("delete {} {}", b.refname, b.oid.as_str()));
        } else {
            kept += 1;
        }
    }
    let whole = kept == 0 && plan.backups.iter().all(|t| t.pruned_only);
    if whole {
        for b in &record.group.refs {
            if !lines.iter().any(|l| l.contains(&b.refname)) {
                lines.push(format!("delete {} {}", b.refname, b.oid.as_str()));
            }
        }
    }
    if lines.is_empty() {
        return Trimmed::Kept;
    }
    let mut input = lines.join("\n");
    input.push('\n');
    let cmd = GitCommand::new(Class::Write, top, ["update-ref", "--stdin"])
        .feed("ref transaction", input.into_bytes());
    let done = runner.run(&cmd, handle);
    let ok = done.ok();
    ran.push(done);
    match (ok, whole) {
        (true, true) => Trimmed::Gone,
        (true, false) => Trimmed::Kept,
        (false, _) => Trimmed::Failed(
            "the refs the prune did not delete could not be taken out of the group".into(),
        ),
    }
}

/// The mode of the `reset` that moves HEAD in `plan` (a `reset --<mode> <commit>` step; `reset
/// … HEAD` moves nothing), which the group's manifest records ([`backup::Manifest::moved_by`]).
fn moved_by(plan: &Plan) -> Option<ResetMode> {
    plan.steps.iter().find_map(|step| {
        let Step::Git(cmd) = step else { return None };
        let args = cmd.args();
        let words: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        let mode = match words.as_slice() {
            ["reset", flag, target] if *target != "HEAD" => *flag,
            _ => return None,
        };
        match mode {
            "--soft" => Some(ResetMode::Soft),
            "--mixed" => Some(ResetMode::Mixed),
            "--keep" => Some(ResetMode::Keep),
            "--hard" => Some(ResetMode::Hard),
            _ => None,
        }
    })
}

/// The step `i` of `plan` comes after a restore's work tree reset (`reset --hard HEAD`) and
/// before its apply — the first step past HEAD's moves, where the apply is or would be (03
/// §8.3): stopped there, the tracked changes are reset and not yet back.
fn between_the_wipe_and_the_apply(plan: &Plan, i: usize) -> bool {
    let words = |step: &Step| -> Vec<String> {
        match step {
            Step::Git(cmd) => cmd
                .args()
                .iter()
                .filter_map(|a| a.to_str())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    };
    let Some(wipe) = plan
        .steps
        .iter()
        .position(|s| words(s) == ["reset", "--hard", "HEAD"])
    else {
        return false;
    };
    let moves = |s: &Step| matches!(words(s).as_slice(), [r, _, t] if r == "reset" && t != "HEAD");
    let apply = plan.steps[wipe + 1..]
        .iter()
        .position(|s| !moves(s))
        .map_or(plan.steps.len(), |k| wipe + 1 + k);
    wipe < i && i < apply
}

/// Runs `plan` in `top` (03 §3.8): its steps in order, the first failure leaving the rest `not
/// run` (see the module's notes). Blocks: call it on a worker, holding the repository's write
/// lock (02 §6); `handle` stops it.
pub fn execute(plan: &Plan, top: &str, runner: &GitRunner, handle: &Handle) -> Outcome {
    let mut steps: Vec<StepResult> = plan
        .steps
        .iter()
        .map(|s| StepResult {
            line: step_line(s, plan),
            outcome: StepOutcome::NotRun,
            ran: Vec::new(),
        })
        .collect();
    if steps.is_empty() {
        return Outcome {
            steps,
            backup: None,
        };
    }
    let checked = match check_stashes(plan, top, runner, handle) {
        Ok(ran) => ran,
        Err(ran) => {
            steps[0].outcome = StepOutcome::Failed;
            steps[0].ran = ran;
            return Outcome {
                steps,
                backup: None,
            };
        }
    };
    let mut record: Option<backup::Record> = None;
    let mut failed = false;
    for (i, step) in plan.steps.iter().enumerate() {
        let (ok, mut ran) = match step {
            Step::Backup => {
                let (result, mut ran) = backup::write_logged(
                    runner,
                    handle,
                    top,
                    &plan.message,
                    &plan.backups,
                    moved_by(plan),
                );
                match result {
                    Ok(r) => {
                        record = Some(r);
                        (true, ran)
                    }
                    Err(reason) => {
                        if ran.iter().all(|d| d.ok()) || !reason.contains(": ") {
                            ran.push(backup::refusal(runner, top, &steps[i].line, &reason));
                        }
                        (false, ran)
                    }
                }
            }
            Step::Git(cmd) if rebases_fetched(cmd) => {
                let (clear, mut ran) = recheck_fetched(top, runner, handle);
                if clear {
                    let done = runner.run(cmd, handle);
                    let ok = done.ok();
                    ran.push(done);
                    (ok, ran)
                } else {
                    (false, ran)
                }
            }
            Step::Git(cmd) => {
                let done = runner.run(cmd, handle);
                (done.ok(), vec![done])
            }
            Step::Remote(change) => {
                let subject = Subject::Pushed {
                    remote: change.remote.clone(),
                    refname: change.refname.clone(),
                };
                match record.as_ref().and_then(|r| r.before(&subject)) {
                    Some(value) => {
                        let lease = oid_of(value).map_or("", |o| o.as_str());
                        let done = runner.run(&remote_command(change, top, lease), handle);
                        (done.ok(), vec![done])
                    }
                    None => (
                        false,
                        vec![backup::refusal(
                            runner,
                            top,
                            &steps[i].line,
                            "the remote's value was not backed up, so it is not overwritten",
                        )],
                    ),
                }
            }
            Step::Archive { command, to } => archive(command, to, top, runner, handle),
        };
        if i == 0 && !checked.is_empty() {
            let mut all = checked.clone();
            all.append(&mut ran);
            ran = all;
        }
        steps[i].ran = ran;
        steps[i].outcome = if ok {
            StepOutcome::Done
        } else {
            StepOutcome::Failed
        };
        if !ok {
            failed = true;
            break;
        }
    }
    // 03 §8.3: a restore stopped after its work tree's reset and before the apply (HEAD's move
    // refused) leaves the tracked changes reset; they are in the restore's own `worktree`
    // backup, and the result says so — after git's own text, so the dialog shows why as well as
    // where they went.
    let refused_at = steps.iter().position(|s| s.outcome == StepOutcome::Failed);
    if let (Some(i), Some(r)) = (refused_at, &record) {
        let kept = matches!(r.before(&Subject::WorkTree), Some(Value::Object(_)));
        if kept && between_the_wipe_and_the_apply(plan, i) {
            let reason = format!(
                "{} was refused; your uncommitted changes were set aside in backup {} — restore \
                 it to bring them back",
                steps[i].line, r.group.stamp
            );
            let why = steps[i]
                .ran
                .iter()
                .rev()
                .find(|d| !d.ok())
                .map(|d| d.error_text())
                .unwrap_or_default();
            let mut said = backup::refusal(runner, top, &steps[i].line, &reason);
            if !why.trim().is_empty() {
                said.stderr = format!("{}\nsopen: {reason}", why.trim_end());
            }
            steps[i].ran.push(said);
        }
    }
    let mut stamp = record.as_ref().map(|r| r.group.stamp.clone());
    if let Some(r) = &record {
        let mut extra = Vec::new();
        let mut incomplete: Vec<String> = Vec::new();
        if !failed {
            match trim(plan, r, top, runner, handle, &mut extra) {
                Trimmed::Gone => stamp = None,
                Trimmed::Kept => {}
                Trimmed::Failed(why) => incomplete.push(why),
            }
        }
        if let Some(st) = &stamp {
            let (sealed, ran) = backup::seal(runner, handle, top, r);
            extra.extend(ran);
            if let Err(why) = sealed {
                incomplete.push(format!("its after record was not written ({why})"));
            }
            if !incomplete.is_empty() {
                let reason = format!("backup {st} incomplete: {}", incomplete.join("; "));
                extra.push(backup::refusal(
                    runner,
                    top,
                    &format!("backup {st}"),
                    &reason,
                ));
            }
        }
        if let Some(last) = steps
            .iter_mut()
            .rev()
            .find(|s| s.outcome != StepOutcome::NotRun)
        {
            last.ran.extend(extra);
        }
    }
    Outcome {
        steps,
        backup: stamp,
    }
}
