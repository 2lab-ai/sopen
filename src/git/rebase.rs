//! History edits (03 §7.8): the quick edits of a commit's menu (§7.8.1) and the planner
//! (§7.8.2). Each of them is a run of the planner, one `git rebase -i` whose todo sopen writes on
//! stdin; only the HEAD forms of `reword` and `author` amend instead. The plans are
//! `git::actions`' own [`Plan`]s, so the confirmation (§8.1), the backup group (§8.2) and the
//! executor (`actions::execute`) are S3's, unchanged. S4-A writes the logic; the planner pane, the
//! quick edits' dialogs, the commit menu's history row and an edit stop's Amend and Split are
//! S4-B's (`gui::browser::git_panel::{planner, menu, in_progress}`), the wiring S4-C's.
//!
//! # The API (S4-0's contract between S4-A and S4-B)
//!
//! - **Ids.** [`EditId`] is the last row of 03 §7.1's table: the eleven history-edit ids,
//!   `reword` to `plan`, spelled as §7.8.1 spells them. They are not `actions::ActionId`s: S3's
//!   menu, dialogs and tests hold that enum to §7.1–§7.7 (S3-A's id test pins
//!   `ActionId::parse("drop")` to `None`). `SOPEN_STATE dialog=<id>` (02 §9) takes either:
//!   `ActionId::parse`, then [`EditId::parse`]; no spelling is in both.
//! - **Quick edits.** A quick edit's dialog starts from the [`Commit`] it edits (for `squash`,
//!   also the commit below), which S4-C reads and hands in: its prefill (§7.8.1). It collects its
//!   options into a [`Quick`] and asks [`plan`] for the [`Plan`] of [`Edit::Quick`], again
//!   whenever an option changes, as S3's dialogs do (§8.1). On an older commit the quick edit is
//!   one change on its range's [`Model`] ([`Quick::apply`]), and the model's [`Run`] is what the
//!   plan runs (§7.8: "the quick edits are plans with one change").
//! - **The planner.** [`open`] reads the range above a [`Base`] up to HEAD into a [`Model`]: its
//!   [`Row`]s, newest first as the graph shows them, and the branches in the range. The pane's
//!   keys become [`Op`]s, which [`Model::apply`] applies or refuses; [`Model::problems`] are the
//!   checks before the run; Cmd/Ctrl+Enter plans [`Edit::Plan`].
//! - **Seams.** S4-B's pane and dialogs take `Model::apply`, `Model::changed`,
//!   `Model::problems` (as text lines) and [`plan`] through function seams of their own, as S3-B's
//!   dialogs take `actions::plan` (`dialogs::Planner`), so S4-B's tests run on fakes before
//!   S4-A's bodies land; S4-C passes these. [`Model::new`] is written, so a test builds a model.
//! - **The run.** [`Run`] is what a plan runs: the [`Base`], the todo ([`TodoLine`]s, oldest
//!   first), `--autostash`, `--update-refs`, the branches it moves ([`BranchMove`]) and those it
//!   leaves where they are. The todo can hold only the lines §7.8.2's table writes: `reword` and
//!   `squash` are never written, and `exec` is sopen's message or author line ([`Exec`]), never
//!   the owner's own command (§10.2).
//! - **Reads.** As in `actions`: [`needs`] and [`open_needs`] list the reads [`plan`] and [`open`]
//!   would make that `Repo::seen` does not hold yet, as `actions::Need`s, which
//!   `actions::inspect` makes on a worker; `plan` and `open` read and run nothing.
//! - **An edit stop** (§8.5: Amend, Split, Continue, Abort). Amend is §7.6's
//!   `actions::Action::Amend`. Split is [`Edit::SplitStop`]; after it the window keeps the
//!   [`Split`] ([`split_of`]), and the banner's Continue goes through [`Edit::ContinueSplit`]
//!   while it is kept.
//!
//! S4-0 scaffold: the types and the entry points, with `todo!()` bodies. The ids, the verbs'
//! words and letters, [`Quick::id`] and [`Model::new`] are written in full: they are data, or a
//! plain constructor. S4-A fills in the rest and adds fields, variants and types; renaming or
//! removing one goes through the dispatcher, as for `actions` (02 §5).

use super::actions::{Need, Plan, Refused, Repo};
use super::model::{CommitRow, Oid};

/// The history-edit ids (03 §7.1's last row, §7.8.1): the commit menu's history row and the
/// values `SOPEN_STATE dialog=` takes for it (02 §9). They are not `actions::ActionId`s (see the
/// module's notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditId {
    /// Reword… (F2 on a commit).
    Reword,
    /// Author and date…
    Author,
    /// Squash into the commit below…
    Squash,
    /// Fix up into the commit below.
    Fixup,
    /// Move up.
    MoveUp,
    /// Move down.
    MoveDown,
    /// Drop… (F8 on a commit).
    Drop,
    /// Stop here to edit…
    Edit,
    /// Split…
    Split,
    /// Fold the staged changes into this commit…
    Fold,
    /// Edit history from here… (Cmd/Ctrl+E): opens the planner on this commit's row, with no
    /// plan of its own.
    Plan,
}

impl EditId {
    /// Every id, in 03 §7.8.1's table order.
    pub const ALL: [EditId; 11] = [
        EditId::Reword,
        EditId::Author,
        EditId::Squash,
        EditId::Fixup,
        EditId::MoveUp,
        EditId::MoveDown,
        EditId::Drop,
        EditId::Edit,
        EditId::Split,
        EditId::Fold,
        EditId::Plan,
    ];

    /// The id as 03 §7.8.1 spells it (`move-up`): `SOPEN_STATE dialog=`'s value (02 §9).
    pub fn as_str(self) -> &'static str {
        match self {
            EditId::Reword => "reword",
            EditId::Author => "author",
            EditId::Squash => "squash",
            EditId::Fixup => "fixup",
            EditId::MoveUp => "move-up",
            EditId::MoveDown => "move-down",
            EditId::Drop => "drop",
            EditId::Edit => "edit",
            EditId::Split => "split",
            EditId::Fold => "fold",
            EditId::Plan => "plan",
        }
    }

    /// The id spelled `text` as 03 §7.8.1 spells it, or `None`.
    pub fn parse(text: &str) -> Option<EditId> {
        EditId::ALL.into_iter().find(|id| id.as_str() == text)
    }
}

/// A planner row's verb (03 §7.8.2). Its key is git's own todo letter (git-rebase(1)). `reword`
/// and `squash` are row verbs only: the todo never holds them ([`TodoLine`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Verb {
    /// The commit as it is (the default).
    #[default]
    Pick,
    /// A new message ([`Row::message`]).
    Reword,
    /// The run stops after it (§8.5's edit stop).
    Edit,
    /// Into the row below, with the group's message.
    Squash,
    /// Into the row below, keeping the older message.
    Fixup,
    /// Left out of the history.
    Drop,
}

impl Verb {
    /// Every verb, in 03 §7.8.2's order.
    pub const ALL: [Verb; 6] = [
        Verb::Pick,
        Verb::Reword,
        Verb::Edit,
        Verb::Squash,
        Verb::Fixup,
        Verb::Drop,
    ];

    /// The word the pane shows (03 §7.8.2: `pick`, `reword`, `edit`, `squash`, `fixup`,
    /// `drop`).
    pub fn word(self) -> &'static str {
        match self {
            Verb::Pick => "pick",
            Verb::Reword => "reword",
            Verb::Edit => "edit",
            Verb::Squash => "squash",
            Verb::Fixup => "fixup",
            Verb::Drop => "drop",
        }
    }

    /// Its key (03 §7.8.2's `P` `R` `E` `S` `F` `D`), as a lowercase letter.
    pub fn letter(self) -> char {
        match self {
            Verb::Pick => 'p',
            Verb::Reword => 'r',
            Verb::Edit => 'e',
            Verb::Squash => 's',
            Verb::Fixup => 'f',
            Verb::Drop => 'd',
        }
    }

    /// The verb a typed letter sets, in either case, or `None` (03 §7.8.2: typed letters set
    /// verbs in the planner and do not start Find).
    pub fn of_letter(letter: char) -> Option<Verb> {
        let letter = letter.to_ascii_lowercase();
        Verb::ALL.into_iter().find(|v| v.letter() == letter)
    }
}

/// A date as the author line writes it (03 §7.8.2): `@<unix> <±hhmm>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    /// Unix seconds.
    pub seconds: i64,
    /// The zone's offset from UTC in minutes (`+0900` is 540).
    pub offset_minutes: i32,
}

impl Date {
    /// `@<unix> <±hhmm>` (03 §7.8.2, verified there).
    pub fn word(&self) -> String {
        todo!("S4-A")
    }
}

/// `author`'s committer date (03 §7.8.1): keep (the default), now, or the author date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommitterDate {
    /// The commit's own committer date ([`Commit::committed`]), written back through
    /// `GIT_COMMITTER_DATE`.
    #[default]
    Keep,
    /// Now: no `GIT_COMMITTER_DATE`, so the rewrite's own date (§7.8.2's last point).
    Now,
    /// The author date, through `GIT_COMMITTER_DATE`.
    Author,
}

/// `author`'s options (03 §7.8.1) and the planner's `A` (§7.8.2): what the author line writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
    /// The author date (`--date=<d>`).
    pub date: Date,
    pub committer: CommitterDate,
}

impl Author {
    /// The author line's values (03 §7.8.2) for a commit whose committer date is `committed`:
    /// [`CommitterDate`] resolved (§7.8.1).
    pub fn line(&self, _committed: &Date) -> AuthorLine {
        todo!("S4-A")
    }
}

/// A commit of the range as the planner reads it (03 §7.8.2: verb, hash, subject, author,
/// date), with what its edits start from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// The graph's row: hash, parents (more than one: a merge, which the planner refuses),
    /// subject, and author and date as the list shows them.
    pub row: CommitRow,
    /// `%B` whole: what `reword` prefills and `squash` joins (both messages, the older first, a
    /// blank line between; §7.8.1).
    pub message: String,
    /// `%B` is not valid UTF-8: reword and squash refuse the commit, with the reason (03 §3.3,
    /// G-H3).
    pub message_lossy: bool,
    /// The author and author date as the commit holds them, not `.mailmap`'s (the line writes
    /// them into history): what `A` prefills, with [`CommitterDate::Keep`].
    pub author: Author,
    /// Its committer date, which [`CommitterDate::Keep`] writes back.
    pub committed: Date,
}

/// One row of the planner (03 §7.8.2): a commit of the range and what the run does to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub commit: Commit,
    pub verb: Verb,
    /// The message its message line writes: a `reword` row's own; a `squash` row's is its
    /// group's (§7.8.2), the same on each `squash` row of the group. `None`: the prefill
    /// (§7.8.1).
    pub message: Option<String>,
    /// The author line's values, when the author or a date changed: the row's `A` mark.
    pub author: Option<Author>,
}

/// Which way Cmd/Ctrl+↑/↓ moves a row (03 §7.8.2), and `move-up` / `move-down` (§7.8.1): up is
/// newer, as the pane lists the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Up,
    Down,
}

/// One change to the planner's model, from its keys (03 §7.8.2). A row is named by its commit,
/// not by its place, as the graph's bar is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// `P` `R` `E` `S` `F` `D`: the row's verb.
    Verb { commit: Oid, verb: Verb },
    /// `A`: the author and dates; `None` puts the commit's own back, and the mark goes.
    Author { commit: Oid, author: Option<Author> },
    /// Enter on a `reword` or `squash` row: its message (a `squash` row's: its group's).
    Message { commit: Oid, message: String },
    /// Cmd/Ctrl+↑/↓: the row one place up or down.
    Move { commit: Oid, toward: Toward },
}

/// A check before the run that fails (03 §7.8.2): the run is refused while one remains, and the
/// pane names it. S4-B shows [`Problem::text`] and does not match on the variants, so S4-A may
/// add some.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The range holds these merge commits: a range with merges is refused, naming them; a base
    /// above the newest merge works (§10.2, G-H4).
    Merges(Vec<Oid>),
    /// A `squash` or `fixup` row with no row below it that is not dropped.
    NothingBelow(Oid),
    /// Uncommitted changes, and the run does not stash them (`--autostash` off).
    Uncommitted,
}

impl Problem {
    /// What the pane says (03 §7.8.2: the merges by name).
    pub fn text(&self) -> String {
        todo!("S4-A")
    }
}

/// Where a run starts (03 §7.8.2): `rebase -i <base>`, or `--root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// `<base>`: the commits after it, up to HEAD. The planner's is the row it was opened on
    /// (§7.8.1 `plan`: "the rows above it up to HEAD").
    Commit(Oid),
    /// `--root`: every commit up to HEAD, the root commit included.
    Root,
}

/// A local branch whose tip is in the range (03 §7.8.2 "Other branches in the range"), the
/// current branch left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchTip {
    /// `refs/heads/<b>`.
    pub refname: String,
    /// Its commit now.
    pub tip: Oid,
}

/// The planner's editable model (03 §7.8.2): the range above [`Base`] up to HEAD and the run's
/// options. Made by [`open`], or by [`Model::new`] in a test; S4-A may add fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub base: Base,
    /// Newest first, as the graph shows them.
    pub rows: Vec<Row>,
    /// `--autostash`: the uncommitted changes stashed for the run, shown in the confirmation;
    /// off with uncommitted changes, the run is refused ([`Problem::Uncommitted`]).
    pub autostash: bool,
    /// `--update-refs` (offered on git ≥ 2.38): the branches in the range move along.
    pub update_refs: bool,
    /// The local branches whose tip is in the range (`for-each-ref --merged HEAD --no-merged
    /// <upstream> refs/heads/`, the current branch left out).
    pub branches: Vec<BranchTip>,
    /// Those of [`Model::branches`] checked out in another worktree (`worktree list
    /// --porcelain`'s `branch` lines): no `update-ref` line; they stay where they are, and the
    /// plan view says so (`mid is checked out in another worktree; it stays where it is`).
    pub left_out: Vec<BranchTip>,
}

impl Model {
    /// The model of `commits` (newest first) above `base` (03 §7.8.2): each row a `pick` as the
    /// commit is, no option on and no branch. A plain constructor, so a test needs no struct
    /// literal and S4-A may add fields.
    pub fn new(base: Base, commits: Vec<Commit>) -> Model {
        Model {
            base,
            rows: commits
                .into_iter()
                .map(|commit| Row {
                    commit,
                    verb: Verb::Pick,
                    message: None,
                    author: None,
                })
                .collect(),
            autostash: false,
            update_refs: false,
            branches: Vec::new(),
            left_out: Vec::new(),
        }
    }

    /// Applies `op`, or refuses it with the reason, for the status row: reword or squash of a
    /// commit whose message is not UTF-8 (03 §3.3), a move past either end.
    pub fn apply(&mut self, _op: Op) -> Result<(), Refused> {
        todo!("S4-A")
    }

    /// Something changed since it was read: Esc then asks (03 §7.8.2, §9.2).
    pub fn changed(&self) -> bool {
        todo!("S4-A")
    }

    /// The checks before the run that fail now (03 §7.8.2); `repo` says whether there are
    /// uncommitted changes.
    pub fn problems(&self, _repo: &Repo) -> Vec<Problem> {
        todo!("S4-A")
    }

    /// The run of the model as it is (03 §7.8.2's todo table), or the first problem's refusal.
    pub fn run(&self, _repo: &Repo) -> Result<Run, Refused> {
        todo!("S4-A")
    }
}

/// The commit a todo line names (03 §7.8.2: git reads the hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    /// A commit of the range.
    Commit(Oid),
    /// `fold`'s commit (§7.8.1): the plan's own `commit --fixup=<c>` step makes it, so it has no
    /// hash when the plan is made. How its line names it is S4-A's to settle; 03 marks `fold` an
    /// estimate.
    Folded,
}

/// One line of a run's todo (03 §7.8.2's table), oldest first. These are all it holds: `reword`
/// and `squash` are never written, since they call the editor (a message line carries the message
/// instead), and `break`, `label`, `reset`, `merge` and the owner's own `exec` are left out
/// (§10.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoLine {
    /// `pick <c> <subject>`: a pick, and the first line of a reword and of a squash group.
    Pick { commit: Oid, subject: String },
    /// `drop <c> <subject>`.
    Drop { commit: Oid, subject: String },
    /// `edit <c> <subject>`: the run stops after `<c>` (§7.8.1 `edit` and `split`, §8.5).
    Edit { commit: Oid, subject: String },
    /// `fixup <c> <subject>`: a `fixup` row, each squashed row of a group, `fold`'s commit.
    Fixup { commit: Named, subject: String },
    /// `exec …`: sopen's message or author line.
    Exec(Exec),
    /// `update-ref refs/heads/<b>` (with `--update-refs`), after the line(s) of the commit the
    /// branch points at.
    UpdateRef { refname: String },
}

impl TodoLine {
    /// The line as git reads it (03 §7.8.2's table): the subject is only for git's messages;
    /// git reads the hash.
    pub fn text(&self) -> String {
        todo!("S4-A")
    }
}

/// sopen's two `exec` lines (03 §7.8.2), the only `exec` a todo holds (§10.2). git runs them
/// with its own `/bin/sh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exec {
    /// The message line: `exec printf '%b' '<m>' | git commit --amend --only --allow-empty
    /// --cleanup=verbatim -F -`, `<m>` the message with `\` doubled and each newline written
    /// `\n`, quoted as one shell word, every other byte as it is.
    Message(String),
    /// The author line: `exec [GIT_COMMITTER_DATE='<cd>'] git commit --amend --only
    /// --allow-empty --no-edit --author='<name> <<email>>' --date='<ad>'`.
    Author(AuthorLine),
}

/// The author line's values (03 §7.8.2), its committer date resolved ([`Author::line`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorLine {
    pub name: String,
    pub email: String,
    /// `--date='<ad>'`.
    pub date: Date,
    /// `GIT_COMMITTER_DATE='<cd>'`; `None` is now: no assignment.
    pub committer: Option<Date>,
}

/// A branch a run moves along (03 §7.8.2, with `--update-refs`), backed up first as
/// `heads/<b>` (§8.2). The current branch, or a detached HEAD, is the rebase's own and is not
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMove {
    /// `refs/heads/<b>`.
    pub refname: String,
    /// Its commit now: the backup's value.
    pub from: Oid,
    /// The commit whose line(s) its `update-ref` line follows: its tip, or, when that is
    /// dropped, the nearest kept commit below it, as git does (§7.8.2, an estimate there).
    pub after: Oid,
}

/// What a history edit runs (03 §7.8.2 "The run"): `GIT_SEQUENCE_EDITOR=`[`SEQUENCE_EDITOR`]
/// `git rebase -i --reschedule-failed-exec [--autostash] [--update-refs] <base>|--root`, with
/// the todo on stdin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub base: Base,
    /// Oldest first.
    pub todo: Vec<TodoLine>,
    pub autostash: bool,
    pub update_refs: bool,
    /// The branches it moves along.
    pub moves: Vec<BranchMove>,
    /// The branches in the range it leaves where they are (another worktree's), for the plan
    /// view.
    pub left_out: Vec<BranchTip>,
}

impl Run {
    /// The todo as git's stdin (03 §7.8.2): one line each, oldest first.
    pub fn todo_text(&self) -> String {
        todo!("S4-A")
    }
}

/// The sequence editor (03 §7.8.2): it replaces git's todo file with stdin, so sopen writes no
/// file anywhere. It goes as one of the `env` words (§3.2).
pub const SEQUENCE_EDITOR: &str = r#"f(){ cat >"$1";};f"#;

/// A quick edit (03 §7.8.1), as its dialog's options make it: one change to a commit of the
/// current branch or the detached HEAD, an ancestor of HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quick {
    /// `reword`. HEAD: `commit --amend --only --allow-empty --cleanup=verbatim -F -`, no
    /// rebase. Older: `pick <c>` and the message line.
    Reword { commit: Oid, message: String },
    /// `author`. HEAD: `commit --amend --only --allow-empty --no-edit --author=<a> --date=<d>`,
    /// `GIT_COMMITTER_DATE` per the choice. Older: `pick <c>` and the author line.
    Author { commit: Oid, author: Author },
    /// `squash` into the commit below: `pick <parent>`, `fixup <c>`, the message line.
    Squash { commit: Oid, message: String },
    /// `fixup` into the commit below: `pick <parent>`, `fixup <c>`; the older message stays.
    Fixup { commit: Oid },
    /// `move-up` and `move-down`: the two lines swapped.
    Move { commit: Oid, toward: Toward },
    /// `drop <c>`: any commit of the range.
    Drop { commit: Oid },
    /// `edit <c>`: the run stops after it, and the banner offers Amend, Split, Continue and
    /// Abort (§8.5).
    Edit { commit: Oid },
    /// `split`: `edit <c>`, then at the stop [`Edit::SplitStop`]. Refused on a root commit, which
    /// has no `HEAD^`.
    Split { commit: Oid },
    /// `fold`: `commit --fixup=<c>` (git's `fixup! <subject>` message, no editor), then the new
    /// commit's line right after `<c>`'s, as `fixup` ([`Named::Folded`]).
    Fold { into: Oid },
}

impl Quick {
    /// Its id (03 §7.8.1).
    pub fn id(&self) -> EditId {
        match self {
            Quick::Reword { .. } => EditId::Reword,
            Quick::Author { .. } => EditId::Author,
            Quick::Squash { .. } => EditId::Squash,
            Quick::Fixup { .. } => EditId::Fixup,
            Quick::Move {
                toward: Toward::Up, ..
            } => EditId::MoveUp,
            Quick::Move {
                toward: Toward::Down,
                ..
            } => EditId::MoveDown,
            Quick::Drop { .. } => EditId::Drop,
            Quick::Edit { .. } => EditId::Edit,
            Quick::Split { .. } => EditId::Split,
            Quick::Fold { .. } => EditId::Fold,
        }
    }

    /// The quick edit as one change on `model`, the model of the range it needs: from `<c>`'s
    /// parent, or for `squash`, `fixup` and `move-down` from the parent's parent, or
    /// [`Base::Root`] where there is none (03 §7.8: "the quick edits are plans with one
    /// change").
    pub fn apply(&self, _model: &mut Model) -> Result<(), Refused> {
        todo!("S4-A")
    }
}

/// A history edit to plan (03 §7.8, §8.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// A quick edit from a commit's menu (§7.8.1).
    Quick(Quick),
    /// The planner's run, on Cmd/Ctrl+Enter (§7.8.2).
    Plan(Model),
    /// Split at an edit stop (§7.8.1 `split`, §8.5): the stopped commit's paths recorded
    /// (`diff --name-only -z <c>^1 <c>`), then `reset HEAD^` (mixed). Its changes are then in the
    /// uncommitted row, the files it added untracked, to be committed in parts, file by file.
    SplitStop { commit: Oid },
    /// Continue at a split's stop: refused while any recorded path still differs from HEAD
    /// (`status --porcelain=v2 -z --untracked-files=all -- <paths>` not empty), since git would
    /// continue and leave an added file out of history; else the banner's own Continue
    /// (`actions::Action::Proceed`).
    ContinueSplit(Split),
}

/// What the window keeps after [`Edit::SplitStop`] ran, for [`Edit::ContinueSplit`] (03 §7.8.1
/// `split`): the commit split and its paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub commit: Oid,
    /// `diff --name-only -z <c>^1 <c>`, from the top level.
    pub paths: Vec<String>,
}

/// The [`Split`] of `commit` (03 §7.8.1 `split`), from the read [`Edit::SplitStop`]'s plan made
/// (`repo.seen`); `None` when it was not read.
pub fn split_of(_commit: &Oid, _repo: &Repo) -> Option<Split> {
    todo!("S4-A")
}

/// The plan of `edit` on `repo` (03 §7.8, §8.1–§8.2): pure, as `actions::plan` is. It reads and
/// runs nothing, so a dialog asks again as its options change. A read it needs that
/// `repo.seen` lacks refuses it with `not read yet: …` ([`needs`] lists them). Its first step
/// writes the backup group: `heads/<current>` (or `detached`) and `heads/<b>` for each branch it
/// moves (§8.2). Its confirmation names the commits of the range a remote-tracking branch
/// already holds, as every rewrite's does (§7.8.2).
pub fn plan(_edit: &Edit, _repo: &Repo) -> Result<Plan, Refused> {
    todo!("S4-A")
}

/// The reads `plan(edit, repo)` would make that `repo.seen` does not hold yet (03 §7.8, as
/// `actions::needs` for §7.1–§7.7): empty when `plan` has all it needs.
pub fn needs(_edit: &Edit, _repo: &Repo) -> Vec<Need> {
    todo!("S4-A")
}

/// The planner's model of the range above `base` up to HEAD (03 §7.8.2), from the reads
/// [`open_needs`] lists. A read missing refuses it with `not read yet: …`.
pub fn open(_base: &Base, _repo: &Repo) -> Result<Model, Refused> {
    todo!("S4-A")
}

/// The reads `open(base, repo)` would make that `repo.seen` does not hold yet (03 §7.8.2).
pub fn open_needs(_base: &Base, _repo: &Repo) -> Vec<Need> {
    todo!("S4-A")
}
