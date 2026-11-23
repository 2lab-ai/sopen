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
//!   (§3.8), with the commands it ran as the log keeps them. The executor that runs a plan on a
//!   worker is S3-A's to add here.
//!
//! S3-0 scaffold: the types and the entry points, `todo!()` inside. S3-A fills them in and adds
//! fields, variants and types; renaming or removing one goes through the dispatcher, as for
//! `model` (02 §5).

use super::backup::{RestoreEntry, Take};
use super::model::{GitVersion, Head, InProgress, Oid, RefLabel, StashRow, Status};
use super::runner::{Done, GitCommand};
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
    /// `push`: to each remote in turn, stopping at the first failure; `overwrite` goes through
    /// §8.4's lease.
    Push {
        branch: String,
        remotes: Vec<String>,
        set_upstream: bool,
        overwrite: bool,
        no_verify: bool,
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
/// `pushed/<r>/…`, then the push with that value as the lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteChange {
    pub remote: String,
    /// The remote's ref (`refs/heads/<b>`, `refs/tags/<t>`).
    pub refname: String,
    pub change: Overwrite,
}

/// What [`RemoteChange`] does to the remote's ref (03 §8.4, step 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overwrite {
    /// `push --force-with-lease=<ref>:<value> <r> <src>:<ref>`: an overwriting push or a
    /// replaced tag; `src` the local side (`refs/heads/<b>`, `refs/tags/<t>`).
    Replace { src: String },
    /// `push --force-with-lease=<ref>:<value> <r> --delete <ref>`.
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
    /// §8.4's push for one change, its lease the value the backup step fetched.
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
/// asks again as its options change.
pub fn plan(_action: &Action, _repo: &Repo) -> Result<Plan, Refused> {
    todo!("S3-A")
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
    pub fn of(_in_progress: &InProgress, _status: &Status) -> Option<Banner> {
        todo!("S3-A")
    }

    /// Its text (03 §8.5): `REBASE 3/7 · stopped at abc12345 <subject> · 2 conflicts`, `AM · 1 conflict`,
    /// `MERGE of feature · 1 conflict`, `CHERRY-PICK abc12345`, `REVERT abc12345`,
    /// `BISECT · Reset`; an edit stop `stopped to edit abc12345`.
    pub fn text(&self) -> String {
        todo!("S3-A")
    }

    /// The keys §8.5's table offers for the state, in order. An edit stop adds §7.6's `amend`
    /// (and S4 its Split).
    pub fn offers(&self) -> Vec<Proceed> {
        todo!("S3-A")
    }
}
