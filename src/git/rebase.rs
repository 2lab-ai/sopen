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
//! S4-0 scaffold: the types and the entry points. The ids, the verbs' words and letters,
//! [`Quick::id`] and [`Model::new`] are written in full: they are data, or a plain constructor.
//! S4-A filled in the rest and added fields, variants and types; renaming or removing one goes
//! through the dispatcher, as for `actions` (02 §5).
//!
//! # What S4-A added (for S4-B and S4-C)
//!
//! - **Fields.** [`Model::read`] (the model as read — the rows' order and the run's options —
//!   for [`Model::changed`], a [`Read`]) and [`Model::fold`] (`fold`'s target; only
//!   [`Quick::Fold`] sets it), both set by [`Model::new`]; [`AuthorLine::message`] (a literal of
//!   `AuthorLine` needs `message: None`). [`Problem`] gained `Busy` and `NothingStaged`.
//! - **A squash group's message** is [`squash_message`]'s join: the planner pane and the quick
//!   squash dialog call it too (S4-C dropped S4-B's copy), so the pane shows what the todo
//!   writes.
//! - **The run's command** is `git -c core.commentChar=# rebase -i --reschedule-failed-exec
//!   [--autostash] [--update-refs | --no-update-refs] <base>|--root`, `GIT_SEQUENCE_EDITOR` as an
//!   `env` word and the todo on stdin: `core.commentChar` (or `core.commentString`) set to a
//!   letter would make git read `pick` lines as comments and drop those commits while the
//!   `exec` lines still run; `--no-update-refs` (git ≥ 2.38) keeps `rebase.updateRefs` from
//!   moving a branch no backup holds. Planner runs need git 2.22 (`--reschedule-failed-exec`).
//! - **A run that stops** (an `edit` line) exits 0, so its step is `done`; a conflict or a
//!   failed `exec` line exits 1, so its step is `failed`, with the banner showing the stop. The
//!   executor writes the group's `after` record when the steps end — at the stop — so after a
//!   Continue the branch reads as `moved since` in a restore (03 §8.3), as the executor does for
//!   every action.
//! - **Quick edits on a dirty tree** run with `--autostash`, which the confirmation names; the
//!   planner's model starts with it on when there are uncommitted changes ([`open`]).

use super::actions::{
    self, on_remotes, rebase_paths, refuse_comment_char, refuse_ignored, Action, Banner, Draft,
    Need, Plan, Proceed, Reads, Refused, Repo,
};
use super::backup::{self, Source};
use super::model::{BackupKind, CommitRow, Head, Oid, Operation, RefKind, Sequence, StatusState};
use super::runner::Class;
use std::collections::BTreeSet;

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
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let off = self.offset_minutes.unsigned_abs();
        format!("@{} {sign}{:02}{:02}", self.seconds, off / 60, off % 60)
    }

    /// git's raw date (`--date=raw`: `<unix> <±hhmm>`).
    fn parse_raw(text: &str) -> Option<Date> {
        let (seconds, zone) = text.trim().split_once(' ')?;
        let seconds = seconds.parse().ok()?;
        let (sign, digits) = match zone.as_bytes().first()? {
            b'+' => (1, &zone[1..]),
            b'-' => (-1, &zone[1..]),
            _ => return None,
        };
        if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let hours: i32 = digits[..2].parse().ok()?;
        let minutes: i32 = digits[2..].parse().ok()?;
        Some(Date {
            seconds,
            offset_minutes: sign * (hours * 60 + minutes),
        })
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
    pub fn line(&self, committed: &Date) -> AuthorLine {
        AuthorLine {
            name: self.name.clone(),
            email: self.email.clone(),
            date: self.date,
            committer: match self.committer {
                CommitterDate::Keep => Some(*committed),
                CommitterDate::Now => None,
                CommitterDate::Author => Some(self.date),
            },
            message: None,
        }
    }

    /// What the author line can write (03 §7.8.2): a name and an email on one line each, with
    /// no `<` or `>` (git reads `name <email>`), a date git takes.
    fn check(&self) -> Result<(), Refused> {
        if self.name.trim().is_empty() {
            return refused("the author's name is empty");
        }
        for (what, text) in [("name", &self.name), ("email", &self.email)] {
            if text.contains(['<', '>']) {
                return refused(format!("the author's {what} holds `<` or `>`"));
            }
            if text.contains(['\n', '\r', '\0']) {
                return refused(format!("the author's {what} must be one line"));
            }
            if text.contains('\u{fffd}') {
                return refused(format!(
                    "the author's {what} is not valid UTF-8 as read; type it again"
                ));
            }
        }
        if self.date.seconds < 0 {
            return refused("the author date is before 1970");
        }
        if self.date.offset_minutes.unsigned_abs() >= 24 * 60 {
            return refused("the author date's zone is more than a day from UTC");
        }
        Ok(())
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
    /// git has an operation in progress (03 §8.5's banner), which a rebase cannot start over:
    /// `a rebase`, `a merge`, … (S4-A).
    Busy(String),
    /// `fold` with nothing staged (S4-A).
    NothingStaged,
    /// An author set on a `squash` or `fixup` row: its commit goes into the group's head, whose
    /// author the result keeps, so the change would be lost (S4-A review round 1).
    AuthorLost { commit: Oid, into: Oid },
}

impl Problem {
    /// What the pane says (03 §7.8.2: the merges by name).
    pub fn text(&self) -> String {
        match self {
            Problem::Merges(merges) => format!(
                "the range holds {}: {}; pick a base above the newest merge (the planner does \
                 not rewrite merges)",
                plural(merges.len(), "merge commit"),
                merges.iter().map(Oid::short).collect::<Vec<_>>().join(", ")
            ),
            Problem::NothingBelow(c) => format!(
                "{} is a squash or fixup with no kept commit below it to go into",
                c.short()
            ),
            Problem::Uncommitted => "there are uncommitted changes: stash them for the run \
                                     (--autostash) or commit them first"
                .into(),
            Problem::Busy(what) => {
                format!("{what} is in progress (the banner): finish it or abort it first")
            }
            Problem::NothingStaged => "nothing is staged to fold in".into(),
            Problem::AuthorLost { commit, into } => format!(
                "{}: its author would be lost — it is squashed into {}; set the author on {}",
                commit.short(),
                into.short(),
                into.short()
            ),
        }
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
    /// The model as it was read (S4-A): what [`Model::changed`] compares with.
    pub read: Read,
    /// `fold` (03 §7.8.1, S4-A): the staged changes are committed with `commit --fixup=<c>`
    /// before the run, and that commit's line goes right after `<c>`'s as `fixup`
    /// ([`Named::Folded`]). Only [`Quick::Fold`] sets it; the planner has no key for it.
    pub fold: Option<Oid>,
}

/// A model as it was read (S4-A), for [`Model::changed`]: the rows' order, newest first, and
/// the run's options ([`open`] turns `--autostash` on when there are uncommitted changes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub order: Vec<Oid>,
    pub autostash: bool,
    pub update_refs: bool,
}

impl Model {
    /// The model of `commits` (newest first) above `base` (03 §7.8.2): each row a `pick` as the
    /// commit is, no option on and no branch. A plain constructor, so a test needs no struct
    /// literal and S4-A may add fields.
    pub fn new(base: Base, commits: Vec<Commit>) -> Model {
        let read = Read {
            order: commits.iter().map(|c| c.row.oid.clone()).collect(),
            autostash: false,
            update_refs: false,
        };
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
            read,
            fold: None,
        }
    }

    /// The row of `commit`, or the refusal.
    fn index(&self, commit: &Oid) -> Result<usize, Refused> {
        self.rows
            .iter()
            .position(|r| &r.commit.row.oid == commit)
            .ok_or_else(|| Refused(format!("{} is not in the range", commit.short())))
    }

    /// Applies `op`, or refuses it with the reason, for the status row: reword or squash of a
    /// commit whose message is not UTF-8 (03 §3.3), a move past either end.
    pub fn apply(&mut self, op: Op) -> Result<(), Refused> {
        match op {
            Op::Verb { commit, verb } => {
                let i = self.index(&commit)?;
                if matches!(verb, Verb::Reword | Verb::Squash) {
                    kept_whole(&self.rows[i].commit)?;
                }
                self.rows[i].verb = verb;
            }
            Op::Author { commit, author } => {
                let i = self.index(&commit)?;
                if let Some(a) = &author {
                    kept_whole(&self.rows[i].commit)?;
                    a.check()?;
                }
                self.rows[i].author = author;
            }
            Op::Message { commit, message } => {
                let i = self.index(&commit)?;
                check_message(&message)?;
                match self.rows[i].verb {
                    Verb::Reword => self.rows[i].message = Some(message),
                    Verb::Squash => {
                        for j in self.squashes_with(i) {
                            self.rows[j].message = Some(message.clone());
                        }
                    }
                    other => {
                        return refused(format!(
                            "Enter edits the message of a reword or squash row; {} is a {}",
                            commit.short(),
                            other.word()
                        ))
                    }
                }
            }
            Op::Move { commit, toward } => {
                let i = self.index(&commit)?;
                match toward {
                    Toward::Up if i == 0 => {
                        return refused(format!(
                            "{} is at the top: it cannot move up",
                            commit.short()
                        ))
                    }
                    Toward::Down if i + 1 == self.rows.len() => {
                        return refused(format!(
                            "{} is at the bottom: it cannot move down",
                            commit.short()
                        ))
                    }
                    Toward::Up => self.rows.swap(i, i - 1),
                    Toward::Down => self.rows.swap(i, i + 1),
                }
            }
        }
        Ok(())
    }

    /// The `squash` rows of the group row `i` is in (03 §7.8.2: a squash row's message is the
    /// group's).
    fn squashes_with(&self, i: usize) -> Vec<usize> {
        units(&self.rows)
            .into_iter()
            .find_map(|u| match u {
                Unit::Group { head, members } if head == i || members.contains(&i) => Some(
                    std::iter::once(head)
                        .chain(members)
                        .filter(|&m| self.rows[m].verb == Verb::Squash)
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_else(|| vec![i])
    }

    /// Something changed since it was read: Esc then asks (03 §7.8.2, §9.2) — a row's verb,
    /// author or place, a typed message (kept on the row even when its verb goes back to
    /// `pick`), `fold`, or a run option (`--autostash`, `--update-refs`) changed from
    /// [`Model::read`]. A move that comes back to its place is no change.
    pub fn changed(&self) -> bool {
        !self
            .rows
            .iter()
            .map(|r| &r.commit.row.oid)
            .eq(self.read.order.iter())
            || self
                .rows
                .iter()
                .any(|r| r.verb != Verb::Pick || r.author.is_some() || r.message.is_some())
            || self.fold.is_some()
            || self.autostash != self.read.autostash
            || self.update_refs != self.read.update_refs
    }

    /// The checks before the run that fail now (03 §7.8.2); `repo` says whether there are
    /// uncommitted changes.
    pub fn problems(&self, repo: &Repo) -> Vec<Problem> {
        let mut problems = Vec::new();
        if let Some(op) = repo.in_progress.operation {
            problems.push(Problem::Busy(operation_name(op).into()));
        }
        let merges: Vec<Oid> = self
            .rows
            .iter()
            .filter(|r| r.commit.row.parents.len() > 1)
            .map(|r| r.commit.row.oid.clone())
            .collect();
        if !merges.is_empty() {
            problems.push(Problem::Merges(merges));
        }
        for (i, row) in self.rows.iter().enumerate() {
            let folds = matches!(row.verb, Verb::Squash | Verb::Fixup);
            if folds && self.rows[i + 1..].iter().all(|r| r.verb == Verb::Drop) {
                problems.push(Problem::NothingBelow(row.commit.row.oid.clone()));
            }
        }
        for unit in units(&self.rows) {
            let Unit::Group { head, members } = unit else {
                continue;
            };
            for m in members {
                let row = &self.rows[m];
                if row.author.is_some() && matches!(row.verb, Verb::Squash | Verb::Fixup) {
                    problems.push(Problem::AuthorLost {
                        commit: row.commit.row.oid.clone(),
                        into: self.rows[head].commit.row.oid.clone(),
                    });
                }
            }
        }
        let fold = self.fold.is_some();
        if fold && !staged(repo) {
            problems.push(Problem::NothingStaged);
        }
        if !self.autostash && dirty(repo, fold) {
            problems.push(Problem::Uncommitted);
        }
        problems
    }

    /// The run of the model as it is (03 §7.8.2's todo table), or the first problem's refusal.
    pub fn run(&self, repo: &Repo) -> Result<Run, Refused> {
        if let Some(p) = self.problems(repo).first() {
            return refused(p.text());
        }
        if let Some(into) = &self.fold {
            // The commit `--fixup` makes is in the range git reads; a todo without its line
            // would drop it, and the staged changes with it.
            if !self
                .rows
                .iter()
                .any(|r| &r.commit.row.oid == into && r.verb != Verb::Drop)
            {
                return refused(format!(
                    "the staged changes fold into {}, which is not a kept commit of the range",
                    into.short()
                ));
            }
        }
        let moving: Vec<&BranchTip> = if self.update_refs {
            self.branches
                .iter()
                .filter(|b| !self.left_out.contains(b))
                .collect()
        } else {
            Vec::new()
        };
        let mut todo = Vec::new();
        let mut moves = Vec::new();
        // The kept commit the last unit made: where a dropped tip's branch goes (as git does).
        let mut kept: Option<Oid> = match &self.base {
            Base::Commit(b) => Some(b.clone()),
            Base::Root => None,
        };
        let mut place = |todo: &mut Vec<TodoLine>, tips: &[&Oid], after: Option<&Oid>| {
            for b in moving.iter().filter(|b| tips.contains(&&b.tip)) {
                let Some(after) = after else {
                    return refused(format!(
                        "{} points at {}, which is dropped with no kept commit below it to \
                         move to",
                        short_branch(&b.refname),
                        b.tip.short()
                    ));
                };
                todo.push(TodoLine::UpdateRef {
                    refname: b.refname.clone(),
                });
                moves.push(BranchMove {
                    refname: b.refname.clone(),
                    from: b.tip.clone(),
                    after: after.clone(),
                });
            }
            Ok(())
        };
        for unit in units(&self.rows) {
            match unit {
                Unit::Drop(i) => {
                    let c = &self.rows[i].commit;
                    todo.push(TodoLine::Drop {
                        commit: c.row.oid.clone(),
                        subject: c.row.subject.clone(),
                    });
                    place(&mut todo, &[&c.row.oid], kept.as_ref())?;
                }
                Unit::Group { head, members } => {
                    let h = &self.rows[head];
                    let oid = h.commit.row.oid.clone();
                    let subject = h.commit.row.subject.clone();
                    todo.push(match h.verb {
                        Verb::Edit => TodoLine::Edit {
                            commit: oid.clone(),
                            subject,
                        },
                        _ => TodoLine::Pick {
                            commit: oid.clone(),
                            subject,
                        },
                    });
                    self.folded_after(&mut todo, head);
                    for &m in &members {
                        let c = &self.rows[m].commit;
                        todo.push(match self.rows[m].verb {
                            Verb::Drop => TodoLine::Drop {
                                commit: c.row.oid.clone(),
                                subject: c.row.subject.clone(),
                            },
                            _ => TodoLine::Fixup {
                                commit: Named::Commit(c.row.oid.clone()),
                                subject: c.row.subject.clone(),
                            },
                        });
                        self.folded_after(&mut todo, m);
                    }
                    let message = self.group_message(head, &members)?;
                    if let Some(m) = message.as_ref().filter(|m| !fits(m)) {
                        return refused(format!(
                            "the message of {} is {} bytes, too long for one line of git's todo \
                             ({} KiB): type a shorter one",
                            oid.short(),
                            m.len(),
                            TODO_MESSAGE_MAX / 1024
                        ));
                    }
                    match &h.author {
                        Some(a) => {
                            let mut line = a.line(&h.commit.committed);
                            line.message = match (message, h.verb) {
                                (Some(m), _) => Some(m),
                                (None, Verb::Edit) => None,
                                // Past what one todo line carries, the line reads it from
                                // HEAD when it runs.
                                (None, _) if fits(&h.commit.message) => {
                                    Some(h.commit.message.clone())
                                }
                                (None, _) => None,
                            };
                            todo.push(TodoLine::Exec(Exec::Author(line)));
                        }
                        None => {
                            if let Some(m) = message {
                                todo.push(TodoLine::Exec(Exec::Message(m)));
                            }
                        }
                    }
                    let mut tips = vec![&self.rows[head].commit.row.oid];
                    tips.extend(members.iter().map(|&m| &self.rows[m].commit.row.oid));
                    place(&mut todo, &tips, Some(&oid))?;
                    kept = Some(oid);
                }
            }
        }
        Ok(Run {
            base: self.base.clone(),
            todo,
            autostash: self.autostash,
            update_refs: self.update_refs,
            moves,
            left_out: if self.update_refs {
                self.left_out.clone()
            } else {
                Vec::new()
            },
        })
    }

    /// `fold`'s line after row `i`'s own, when the staged changes fold into it.
    fn folded_after(&self, todo: &mut Vec<TodoLine>, i: usize) {
        let row = &self.rows[i];
        if self.fold.as_ref() == Some(&row.commit.row.oid) && row.verb != Verb::Drop {
            todo.push(TodoLine::Fixup {
                commit: Named::Folded,
                subject: format!("fixup! {}", row.commit.row.subject),
            });
        }
    }

    /// The message a group's last line writes: with a `squash` row, the group's — typed (each
    /// squash row holds it), else its messages joined by [`squash_message`], the oldest first:
    /// the head's (a `reword` head's own), then each squash row's, a fixup row's left out, as
    /// S4-B's pane shows it (`planner::group_message`); else a `reword` head's; else `None`, the
    /// head's own, which git keeps.
    fn group_message(&self, head: usize, members: &[usize]) -> Result<Option<String>, Refused> {
        let squashes: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&m| self.rows[m].verb == Verb::Squash)
            .collect();
        let h = &self.rows[head];
        // A row's own message: a reword's typed one, else the commit's (refused when it is not
        // UTF-8, as reword is).
        let own = |row: &Row| -> Result<String, Refused> {
            match &row.message {
                Some(m) if row.verb == Verb::Reword => Ok(m.clone()),
                _ => {
                    kept_whole(&row.commit)?;
                    Ok(row.commit.message.clone())
                }
            }
        };
        if squashes.is_empty() {
            return (h.verb == Verb::Reword).then(|| own(h)).transpose();
        }
        // Newest first, as S4-B's pane scans: two groups' typed messages meet in one group
        // when the row between them becomes a fixup.
        if let Some(m) = squashes
            .iter()
            .rev()
            .find_map(|&s| self.rows[s].message.clone())
        {
            return Ok(Some(m));
        }
        let mut joined = own(h)?;
        for s in squashes {
            let c = &self.rows[s].commit;
            kept_whole(c)?;
            joined = squash_message(&joined, &c.message);
        }
        Ok(Some(joined))
    }
}

/// `squash`'s message (03 §7.8.1): both messages, the older first, a blank line between. Each is
/// kept whole, byte for byte; the blank line is added after the older one's last line. The
/// planner pane and the quick squash dialog prefill with it too, so they show what the todo
/// writes.
pub fn squash_message(older: &str, newer: &str) -> String {
    let gap = if older.ends_with('\n') { "\n" } else { "\n\n" };
    format!("{older}{gap}{newer}")
}

/// A unit of the todo, oldest first: a dropped row on its own, or a head row (`pick`, `reword`,
/// `edit`) with the `squash`, `fixup` and `drop` rows after it up to its last `squash` or
/// `fixup` (git folds those into the head's commit; a `drop` between is only a line). Indexes
/// are into [`Model::rows`].
enum Unit {
    Drop(usize),
    Group { head: usize, members: Vec<usize> },
}

fn units(rows: &[Row]) -> Vec<Unit> {
    let order: Vec<usize> = (0..rows.len()).rev().collect();
    let mut units = Vec::new();
    let mut k = 0;
    while k < order.len() {
        let head = order[k];
        if rows[head].verb == Verb::Drop {
            units.push(Unit::Drop(head));
            k += 1;
            continue;
        }
        let mut last = k;
        let mut j = k + 1;
        while j < order.len()
            && matches!(rows[order[j]].verb, Verb::Squash | Verb::Fixup | Verb::Drop)
        {
            if rows[order[j]].verb != Verb::Drop {
                last = j;
            }
            j += 1;
        }
        units.push(Unit::Group {
            head,
            members: order[k + 1..=last].to_vec(),
        });
        k = last + 1;
    }
    units
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
    /// git reads the hash. A control character in a subject is written as a space, so the line
    /// stays one line. `fold`'s commit is named `HEAD`: git reads the todo, resolving each name,
    /// before the rebase checks anything out, when HEAD is still the commit `--fixup` made (03's
    /// estimate, settled by `g_h3_fold_staged_changes_into_an_older_commit`, git 2.56.1).
    pub fn text(&self) -> String {
        let named = |verb: &str, hash: &str, subject: &str| {
            let subject: String = subject
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            if subject.is_empty() {
                format!("{verb} {hash}")
            } else {
                format!("{verb} {hash} {subject}")
            }
        };
        match self {
            TodoLine::Pick { commit, subject } => named("pick", commit.as_str(), subject),
            TodoLine::Drop { commit, subject } => named("drop", commit.as_str(), subject),
            TodoLine::Edit { commit, subject } => named("edit", commit.as_str(), subject),
            TodoLine::Fixup { commit, subject } => {
                let hash = match commit {
                    Named::Commit(c) => c.as_str(),
                    Named::Folded => "HEAD",
                };
                named("fixup", hash, subject)
            }
            TodoLine::Exec(exec) => format!("exec {}", exec.command()),
            TodoLine::UpdateRef { refname } => format!("update-ref {refname}"),
        }
    }
}

impl Exec {
    /// The command after `exec` (03 §7.8.2), which git runs with its own `/bin/sh`.
    fn command(&self) -> String {
        const AMEND: &str = "git commit --amend --only --allow-empty --cleanup=verbatim";
        match self {
            Exec::Message(m) => format!("printf '%b' {} | {AMEND} -F -", printf_word(m)),
            Exec::Author(line) => {
                let who = format!(
                    "--author={} --date={}",
                    sh_word(&format!("{} <{}>", line.name, line.email)),
                    sh_word(&line.date.word())
                );
                let committer = line.committer.map_or(String::new(), |cd| {
                    format!("GIT_COMMITTER_DATE={} ", sh_word(&cd.word()))
                });
                match &line.message {
                    Some(m) => format!(
                        "printf '%b' {} | {committer}{AMEND} -F - {who}",
                        printf_word(m)
                    ),
                    None => format!("{HEAD_MESSAGE} | {committer}{AMEND} -F - {who}"),
                }
            }
        }
    }
}

/// The message of HEAD as it is when the line runs, byte for byte: `git cat-file commit HEAD`'s
/// body after the header's blank line (the header read line by line up to the empty one, the
/// rest copied as it is). An author line without [`AuthorLine::message`] pipes it into `commit
/// -F -` (S4-A review round 1): `--no-edit`, even with `--cleanup=verbatim`, dropped a leading
/// blank line (git 2.56.1), and at an `edit` stop the message may have been changed.
pub const HEAD_MESSAGE: &str =
    r#"git cat-file commit HEAD | { while IFS= read -r l && [ -n "$l" ]; do :; done; cat; }"#;

/// `word` as one POSIX shell word: single quotes, each `'` as `'\''` (git's `exec` runs under
/// `/bin/sh`).
fn sh_word(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// A message as `printf '%b'`'s argument (03 §7.8.2): `\` doubled, each newline `\n`, every
/// other byte as it is, quoted as one shell word.
fn printf_word(message: &str) -> String {
    sh_word(&message.replace('\\', r"\\").replace('\n', r"\n"))
}

/// sopen's two `exec` lines (03 §7.8.2), the only `exec` a todo holds (§10.2). git runs them
/// with its own `/bin/sh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exec {
    /// The message line: `exec printf '%b' '<m>' | git commit --amend --only --allow-empty
    /// --cleanup=verbatim -F -`, `<m>` the message with `\` doubled and each newline written
    /// `\n`, quoted as one shell word, every other byte as it is.
    Message(String),
    /// The author line: `exec printf '%b' '<m>' | [GIT_COMMITTER_DATE='<cd>'] git commit --amend
    /// --only --allow-empty --cleanup=verbatim -F - --author='<name> <<email>>' --date='<ad>'`
    /// with [`AuthorLine::message`]; without it the message comes from HEAD as it is then
    /// ([`HEAD_MESSAGE`]` | … -F - …`). S4-A: 03's `--no-edit` re-cleans the message, dropping
    /// trailing blank lines and spaces, and with `--cleanup=verbatim` still drops a leading blank
    /// line (git 2.56.1).
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
    /// The message the line writes as well (S4-A): `Some` pipes it in as the message line does
    /// (`printf '%b' '<m>' | … -F -`), so one line sets both the message and the committer date
    /// — the last line to amend a commit decides both. `None` writes HEAD's message as it is
    /// when the line runs, read byte for byte ([`HEAD_MESSAGE`]): an `edit` row's, which the stop
    /// may change, or a commit's own too long for one todo line.
    pub message: Option<String>,
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
        self.todo.iter().map(|l| l.text() + "\n").collect()
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
    /// `author`. HEAD: `commit --amend --only --allow-empty --cleanup=verbatim -F - --author=<a>
    /// --date=<d>` with the commit's own message on stdin (S4-A: `--no-edit` drops a leading
    /// blank line), `GIT_COMMITTER_DATE` per the choice. Older: `pick <c>` and the author line.
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
    pub fn apply(&self, model: &mut Model) -> Result<(), Refused> {
        let verb = |model: &mut Model, commit: &Oid, verb: Verb| {
            model.apply(Op::Verb {
                commit: commit.clone(),
                verb,
            })
        };
        let message = |model: &mut Model, commit: &Oid, message: &str| {
            model.apply(Op::Message {
                commit: commit.clone(),
                message: message.to_string(),
            })
        };
        match self {
            Quick::Reword { commit, message: m } => {
                verb(model, commit, Verb::Reword)?;
                message(model, commit, m)
            }
            Quick::Author { commit, author } => model.apply(Op::Author {
                commit: commit.clone(),
                author: Some(author.clone()),
            }),
            Quick::Squash { commit, message: m } => {
                verb(model, commit, Verb::Squash)?;
                message(model, commit, m)
            }
            Quick::Fixup { commit } => verb(model, commit, Verb::Fixup),
            Quick::Move { commit, toward } => model.apply(Op::Move {
                commit: commit.clone(),
                toward: *toward,
            }),
            Quick::Drop { commit } => verb(model, commit, Verb::Drop),
            Quick::Edit { commit } => verb(model, commit, Verb::Edit),
            Quick::Split { commit } => {
                let i = model.index(commit)?;
                if model.rows[i].commit.row.parents.is_empty() {
                    return refused(format!(
                        "{} is the root commit: it has no HEAD^ to split it from",
                        commit.short()
                    ));
                }
                verb(model, commit, Verb::Edit)
            }
            Quick::Fold { into } => {
                model.index(into)?;
                model.fold = Some(into.clone());
                Ok(())
            }
        }
    }

    /// The commit it edits.
    fn commit(&self) -> &Oid {
        match self {
            Quick::Reword { commit, .. }
            | Quick::Author { commit, .. }
            | Quick::Squash { commit, .. }
            | Quick::Fixup { commit }
            | Quick::Move { commit, .. }
            | Quick::Drop { commit }
            | Quick::Edit { commit }
            | Quick::Split { commit }
            | Quick::Fold { into: commit } => commit,
        }
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
    /// (`diff-tree -r -z --name-only --no-renames <c>^1 <c>`: both names of a rename, where 03's
    /// `diff --name-only` gives only the new one), then `reset --mixed <c>^1`, backed up as
    /// `detached` (its undo is a mixed reset back). Its changes are then in the uncommitted row,
    /// the files it added untracked, to be committed in parts, file by file. Refused away from an
    /// edit stop at `commit`, on a root commit, on a commit that changes nothing, and with
    /// uncommitted changes at the stop.
    SplitStop { commit: Oid },
    /// Continue at a split's stop: refused while any recorded path still differs from HEAD
    /// (`status --porcelain=v2 -z --untracked-files=all --ignored -- <paths>` not empty: without
    /// `--ignored` an added file that is ignored would not show), since git would continue and
    /// leave an added file out of history; else the banner's own Continue
    /// (`actions::Action::Proceed`).
    ContinueSplit(Split),
}

/// What the window keeps after [`Edit::SplitStop`] ran, for [`Edit::ContinueSplit`] (03 §7.8.1
/// `split`): the commit split and its paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub commit: Oid,
    /// `diff-tree -r -z --name-only --no-renames <c>^1 <c>`, from the top level, in git's order.
    pub paths: Vec<String>,
}

/// The [`Split`] of `commit` (03 §7.8.1 `split`), from the read [`Edit::SplitStop`]'s plan made
/// (`repo.seen`); `None` when it was not read. Call it before the refresh that follows the
/// Split clears `repo.seen`.
pub fn split_of(commit: &Oid, repo: &Repo) -> Option<Split> {
    let near = parse_log(repo.seen.get(&commit_need(commit))?).ok()?;
    let parent = near.first()?.row.parents.first()?.clone();
    let bytes = repo.seen.get(&paths_need(&parent, commit))?;
    Some(Split {
        commit: commit.clone(),
        paths: split_paths(bytes).ok()?,
    })
}

/// The plan of `edit` on `repo` (03 §7.8, §8.1–§8.2): pure, as `actions::plan` is. It reads and
/// runs nothing, so a dialog asks again as its options change. A read it needs that
/// `repo.seen` lacks refuses it with `not read yet: …` ([`needs`] lists them). Its first step
/// writes the backup group: `heads/<current>` (or `detached`) and `heads/<b>` for each branch it
/// moves (§8.2). Its confirmation names the commits of the range a remote-tracking branch
/// already holds, as every rewrite's does (§7.8.2).
pub fn plan(edit: &Edit, repo: &Repo) -> Result<Plan, Refused> {
    let reads = Reads::new(&repo.seen);
    let drafted = draft(edit, repo, &reads);
    let missing = reads.missing.into_inner();
    if let Some(first) = missing.first() {
        return refused(format!("not read yet: {}", first.line()));
    }
    drafted
}

/// The reads `plan(edit, repo)` would make that `repo.seen` does not hold yet (03 §7.8, as
/// `actions::needs` for §7.1–§7.7): empty when `plan` has all it needs.
pub fn needs(edit: &Edit, repo: &Repo) -> Vec<Need> {
    let reads = Reads::new(&repo.seen);
    let _ = draft(edit, repo, &reads);
    reads.missing.into_inner()
}

/// The planner's model of the range above `base` up to HEAD (03 §7.8.2), from the reads
/// [`open_needs`] lists. A read missing refuses it with `not read yet: …`. `autostash` starts on
/// when there are uncommitted changes; the branches in the range are the local branches whose
/// tip is one of its commits (the refs read), the current one left out.
pub fn open(base: &Base, repo: &Repo) -> Result<Model, Refused> {
    let reads = Reads::new(&repo.seen);
    let opened = open_in(base, repo, &reads);
    let missing = reads.missing.into_inner();
    if let Some(first) = missing.first() {
        return refused(format!("not read yet: {}", first.line()));
    }
    opened
}

/// The reads `open(base, repo)` would make that `repo.seen` does not hold yet (03 §7.8.2).
pub fn open_needs(base: &Base, repo: &Repo) -> Vec<Need> {
    let reads = Reads::new(&repo.seen);
    let _ = open_in(base, repo, &reads);
    reads.missing.into_inner()
}

// =============================================================================================
// Reading (S4-A)

/// The commit read's fields (03 §7.8.2: verb, hash, subject, author, date, and what the edits
/// start from), NUL-separated, each commit ending in NUL (`-z`): `%H`, `%P`, the mailmapped
/// author for the list, the raw author for the author line, both dates raw, `%s`, `%B` whole.
const FORMAT: &str = "--format=%H%x00%P%x00%aN%x00%aE%x00%an%x00%ae%x00%ad%x00%cd%x00%s%x00%B";
const FIELDS: usize = 10;
/// `CommitRow`'s cuts (model.rs): the author's name and email, the subject.
const NAME_MAX: usize = 256;
const SUBJECT_MAX: usize = 1024;

fn need(words: &[&str]) -> Need {
    Need(words.iter().map(|w| w.to_string()).collect())
}

/// The range above `base` up to HEAD, newest first, every child before its parents.
fn range_need(base: &Base) -> Need {
    let range = match base {
        Base::Commit(b) => format!("{}..HEAD", b.as_str()),
        Base::Root => "HEAD".into(),
    };
    need(&["log", "-z", "--topo-order", "--date=raw", FORMAT, &range])
}

/// `c`, its parent and its grandparent (the first parent's line): where a quick edit's range
/// starts, and whether `c` is HEAD's commit.
fn commit_need(c: &Oid) -> Need {
    need(&[
        "log",
        "-z",
        "--first-parent",
        "--date=raw",
        FORMAT,
        "-n",
        "3",
        c.as_str(),
    ])
}

/// The worktrees and the branches they have checked out (03 §7.8.2).
fn worktrees_need() -> Need {
    need(&["worktree", "list", "--porcelain"])
}

/// What a split records (03 §7.8.1, both names of a rename).
fn paths_need(parent: &Oid, c: &Oid) -> Need {
    need(&[
        "diff-tree",
        "-r",
        "-z",
        "--name-only",
        "--no-renames",
        parent.as_str(),
        c.as_str(),
    ])
}

/// A commit as stored (`cat-file commit <c>`): `git log` re-encodes a message and its author per
/// the commit's `encoding` header, so only the raw object tells what bytes it holds.
fn raw_need(c: &Oid) -> Need {
    need(&["cat-file", "commit", c.as_str()])
}

/// sopen writes a commit's message and author only when they are UTF-8 as stored (S4-A review
/// round 1): an `encoding` header other than UTF-8, or a message or author line whose bytes are
/// not UTF-8, refuses the edit. Not read yet: no refusal here (the plan refuses as `not read
/// yet`).
fn rewritable(reads: &Reads, c: &Oid) -> Result<(), Refused> {
    let raw = reads.get(raw_need(c));
    if raw.is_empty() {
        return Ok(());
    }
    let (header, message) = match raw.windows(2).position(|w| w == b"\n\n") {
        Some(at) => (&raw[..at], &raw[at + 2..]),
        None => (&raw[..], &raw[raw.len()..]),
    };
    let mut encoding: Option<String> = None;
    let mut author: &[u8] = b"";
    for line in header.split(|&b| b == b'\n') {
        if let Some(v) = line.strip_prefix(b"encoding ") {
            encoding = Some(String::from_utf8_lossy(v).into_owned());
        } else if let Some(v) = line.strip_prefix(b"author ") {
            author = v;
        }
    }
    let foreign = encoding
        .as_deref()
        .filter(|e| !e.eq_ignore_ascii_case("utf-8") && !e.eq_ignore_ascii_case("utf8"));
    let message_bad = foreign.is_some() || std::str::from_utf8(message).is_err();
    let author_bad =
        std::str::from_utf8(author).is_err() || (foreign.is_some() && !author.is_ascii());
    let what = match (message_bad, author_bad) {
        (false, false) => return Ok(()),
        (true, true) => "message and author are",
        (true, false) => "message is",
        (false, true) => "author is",
    };
    refused(format!(
        "{}: its {what} not UTF-8 ({}); sopen rewrites UTF-8 only",
        c.short(),
        foreign.unwrap_or("no encoding header")
    ))
}

/// The commits of a run whose message or author sopen's own lines write: a reword's, an author
/// change's, and a squash group's head and squash rows (the group's message and author).
fn written(model: &Model) -> Vec<&Oid> {
    let mut out = Vec::new();
    for unit in units(&model.rows) {
        let Unit::Group { head, members } = unit else {
            continue;
        };
        let h = &model.rows[head];
        let squashes: Vec<usize> = members
            .into_iter()
            .filter(|&m| model.rows[m].verb == Verb::Squash)
            .collect();
        if h.verb == Verb::Reword || h.author.is_some() || !squashes.is_empty() {
            out.push(&h.commit.row.oid);
        }
        out.extend(squashes.into_iter().map(|m| &model.rows[m].commit.row.oid));
    }
    out
}

/// The commits [`FORMAT`] printed.
fn parse_log(bytes: &[u8]) -> Result<Vec<Commit>, Refused> {
    let mut fields: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    if fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    if !fields.len().is_multiple_of(FIELDS) {
        return refused("git printed the commits in a form sopen does not read");
    }
    fields.chunks(FIELDS).map(commit_of).collect()
}

fn commit_of(f: &[&[u8]]) -> Result<Commit, Refused> {
    let bad = || Refused("git printed a commit sopen does not read".into());
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let oid = std::str::from_utf8(f[0])
        .ok()
        .and_then(Oid::parse)
        .ok_or_else(bad)?;
    let parents = std::str::from_utf8(f[1])
        .map_err(|_| bad())?
        .split_whitespace()
        .map(|p| Oid::parse(p).ok_or_else(bad))
        .collect::<Result<Vec<_>, _>>()?;
    let authored = std::str::from_utf8(f[6])
        .ok()
        .and_then(Date::parse_raw)
        .ok_or_else(bad)?;
    let committed = std::str::from_utf8(f[7])
        .ok()
        .and_then(Date::parse_raw)
        .ok_or_else(bad)?;
    Ok(Commit {
        row: CommitRow {
            oid,
            parents,
            author_name: cut(text(f[2]), NAME_MAX),
            author_email: cut(text(f[3]), NAME_MAX),
            author_time: authored.seconds,
            commit_time: committed.seconds,
            subject: cut(text(f[8]), SUBJECT_MAX),
        },
        message: text(f[9]),
        message_lossy: std::str::from_utf8(f[9]).is_err(),
        author: Author {
            name: text(f[4]),
            email: text(f[5]),
            date: authored,
            committer: CommitterDate::Keep,
        },
        committed,
    })
}

/// `text` cut to at most `max` bytes, never inside a character.
fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// A split's paths; refused when one is not UTF-8 (a path sopen cannot name back to git).
fn split_paths(bytes: &[u8]) -> Result<Vec<String>, Refused> {
    bytes
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            std::str::from_utf8(p).map(str::to_string).map_err(|_| {
                Refused(format!(
                    "a path of the commit is not UTF-8 ({}), so sopen cannot check it at \
                     Continue",
                    String::from_utf8_lossy(p)
                ))
            })
        })
        .collect()
}

/// The branches the worktrees have checked out (`branch <ref>` lines).
fn checked_out(bytes: &[u8]) -> BTreeSet<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|l| l.strip_prefix("branch "))
        .map(str::to_string)
        .collect()
}

fn open_in(base: &Base, repo: &Repo, reads: &Reads) -> Result<Model, Refused> {
    let Some(head) = repo.head.oid().cloned() else {
        return refused("the branch has no commit yet");
    };
    let log = reads.get(range_need(base));
    let worktrees = reads.get(worktrees_need());
    let commits = parse_log(&log)?;
    let Some(newest) = commits.first() else {
        return refused(format!(
            "there is no commit above {} up to HEAD",
            base_shown(base)
        ));
    };
    if newest.row.oid != head {
        return refused(format!(
            "HEAD is now {}, not {}: read the history again",
            head.short(),
            newest.row.oid.short()
        ));
    }
    // Without merges (a problem the pane names) the range is one line of history down to the
    // base, or the base is not below HEAD and the run would move the branch onto it.
    if commits.iter().all(|c| c.row.parents.len() <= 1) {
        let line = commits
            .windows(2)
            .all(|w| w[0].row.parents.first() == Some(&w[1].row.oid));
        let oldest = &commits[commits.len() - 1].row;
        let bottom = match base {
            Base::Commit(b) => oldest.parents.first() == Some(b),
            Base::Root => oldest.parents.is_empty(),
        };
        if !line || !bottom {
            return refused(format!(
                "{} is not below HEAD on its line of history",
                base_shown(base)
            ));
        }
    }
    let mut model = Model::new(base.clone(), commits);
    model.autostash = dirty(repo, false);
    model.read.autostash = model.autostash;
    let current = current(repo);
    let mut tips: Vec<(usize, BranchTip)> = repo
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::LocalBranch && !r.lossy)
        .filter(|r| Some(r.refname.as_str()) != current)
        .filter_map(|r| {
            let at = model
                .rows
                .iter()
                .position(|row| row.commit.row.oid == r.object)?;
            Some((
                at,
                BranchTip {
                    refname: r.refname.clone(),
                    tip: r.object.clone(),
                },
            ))
        })
        .collect();
    tips.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.refname.cmp(&b.1.refname)));
    let elsewhere = checked_out(&worktrees);
    model.branches = tips.into_iter().map(|(_, t)| t).collect();
    model.left_out = model
        .branches
        .iter()
        .filter(|b| elsewhere.contains(&b.refname))
        .cloned()
        .collect();
    Ok(model)
}

// =============================================================================================
// Planning (S4-A)

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

/// The first few of `items`, for a confirmation or a refusal: `a, b, c and 9 more`.
fn few(items: &[String]) -> String {
    const SHOWN: usize = 5;
    let mut text = items
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > SHOWN {
        text.push_str(&format!(" and {} more", items.len() - SHOWN));
    }
    text
}

/// A subject for a title or a change line: at most 60 characters, never cut inside one.
fn shown(subject: &str) -> String {
    let mut chars = subject.chars();
    let mut text: String = chars.by_ref().take(60).collect();
    if chars.next().is_some() {
        text.push('…');
    }
    text
}

fn short_branch(refname: &str) -> &str {
    refname.strip_prefix("refs/heads/").unwrap_or(refname)
}

/// The checked-out branch's ref, when it can be named.
fn current(repo: &Repo) -> Option<&str> {
    match &repo.head {
        Head::Branch {
            refname,
            lossy: false,
            ..
        } => Some(refname),
        _ => None,
    }
}

/// The checked-out branch's short name, or `HEAD`.
fn head_shown(repo: &Repo) -> String {
    current(repo).map_or_else(|| "HEAD".into(), |r| short_branch(r).to_string())
}

fn base_shown(base: &Base) -> String {
    match base {
        Base::Commit(b) => b.short().to_string(),
        Base::Root => "the root".into(),
    }
}

fn operation_name(op: Operation) -> &'static str {
    match op {
        Operation::Rebase | Operation::RebaseApply => "a rebase",
        Operation::Am => "an am",
        Operation::Merge => "a merge",
        Operation::CherryPick | Operation::Sequence(Sequence::Pick) => "a cherry-pick",
        Operation::Revert | Operation::Sequence(Sequence::Revert) => "a revert",
    }
}

/// Uncommitted changes a rebase refuses to start over (tracked changes, conflicts; untracked
/// files are not), the staged ones left out for `fold`, which commits them first.
fn dirty(repo: &Repo, fold: bool) -> bool {
    repo.status.entries.iter().any(|e| match &e.state {
        StatusState::Untracked => false,
        StatusState::Conflict(_) => true,
        StatusState::Changed { unstaged, .. } => !fold || unstaged.is_some(),
    })
}

/// Something is staged.
fn staged(repo: &Repo) -> bool {
    repo.status.entries.iter().any(|e| {
        matches!(
            e.state,
            StatusState::Changed {
                staged: Some(_),
                ..
            }
        )
    })
}

/// 03 §3.3, G-H3: a message that is not UTF-8 is not rewritten (reword, squash, a new author).
fn kept_whole(c: &Commit) -> Result<(), Refused> {
    if c.message_lossy {
        return refused(format!(
            "{}'s message is not valid UTF-8, so sopen cannot keep it byte for byte: it is not \
             reworded, squashed or given a new author",
            c.row.oid.short()
        ));
    }
    Ok(())
}

/// The longest message word a todo line carries: git runs an `exec` line as one `sh -c`
/// argument, and Linux caps one argument at 128 KiB.
const TODO_MESSAGE_MAX: usize = 60 * 1024;

/// `message` fits on one line of the todo.
fn fits(message: &str) -> bool {
    printf_word(message).len() <= TODO_MESSAGE_MAX && !message.contains('\0')
}

/// A message a todo line or `-F -` can carry.
fn check_message(message: &str) -> Result<(), Refused> {
    if message.is_empty() {
        return refused("a message cannot be empty");
    }
    if message.contains('\0') {
        return refused("a message cannot hold a NUL byte");
    }
    if !fits(message) {
        return refused(format!(
            "the message is too long for one line of git's todo ({} KiB)",
            TODO_MESSAGE_MAX / 1024
        ));
    }
    Ok(())
}

fn draft(edit: &Edit, repo: &Repo, reads: &Reads) -> Result<Plan, Refused> {
    match edit {
        Edit::Quick(q) => quick(q, repo, reads),
        Edit::Plan(model) => {
            // HEAD against the newest commit as the planner read it, not the first row now: the
            // owner may have moved that one down (S4-C round 2; a moved row is still in the
            // range, and every row is on the todo).
            let newest = model.read.order.first();
            if newest.is_none() || newest != repo.head.oid() {
                return refused(format!(
                    "HEAD is {}, not the plan's newest commit {}: open the planner again",
                    repo.head.oid().map_or("unborn", Oid::short),
                    newest.map_or("(none)", Oid::short)
                ));
            }
            let head = head_shown(repo);
            let title = match &model.base {
                Base::Commit(b) => format!("Edit the history of {head} above {}", b.short()),
                Base::Root => format!("Edit the whole history of {head}"),
            };
            run_plan(
                model,
                repo,
                reads,
                title,
                format!("sopen: edit history of {head}"),
            )
        }
        Edit::SplitStop { commit } => split_stop(commit, repo, reads),
        Edit::ContinueSplit(split) => continue_split(split, repo, reads),
    }
}

/// A quick edit (03 §7.8.1): the HEAD forms of `reword` and `author` amend; every other is one
/// change on the model of the range it needs, run as the planner runs.
fn quick(q: &Quick, repo: &Repo, reads: &Reads) -> Result<Plan, Refused> {
    let c = q.commit();
    let Some(head_oid) = repo.head.oid() else {
        return refused("the branch has no commit yet");
    };
    if let Some(op) = repo.in_progress.operation {
        return refused(Problem::Busy(operation_name(op).into()).text());
    }
    let near = parse_log(&reads.get(commit_need(c)))?;
    let Some(this) = near.first().filter(|n| &n.row.oid == c) else {
        return refused(format!("{} is not read yet", c.short()));
    };
    let head = head_shown(repo);
    let named = format!("{} {}", c.short(), shown(&this.row.subject));
    if c == head_oid {
        match q {
            Quick::Reword { message, .. } => return reword_head(this, message, repo, reads),
            Quick::Author { author, .. } => return author_head(this, author, repo, reads),
            _ => {}
        }
    }
    let below = matches!(
        q,
        Quick::Squash { .. }
            | Quick::Fixup { .. }
            | Quick::Move {
                toward: Toward::Down,
                ..
            }
    );
    let parent = this.row.parents.first();
    let base = match (below, parent) {
        (false, Some(p)) => Base::Commit(p.clone()),
        (false, None) => Base::Root,
        (true, None) => {
            return refused(format!(
                "{named} is the root commit: there is no commit below it"
            ))
        }
        (true, Some(p)) => match near.get(1).filter(|n| &n.row.oid == p) {
            Some(parent) => match parent.row.parents.first() {
                Some(pp) => Base::Commit(pp.clone()),
                None => Base::Root,
            },
            None => return refused(format!("{} is not read yet", p.short())),
        },
    };
    let mut model = open_in(&base, repo, reads)?;
    if matches!(q, Quick::Fold { .. }) {
        model.autostash = dirty(repo, true);
        model.read.autostash = model.autostash;
    }
    q.apply(&mut model)?;
    let (title, message) = match q {
        Quick::Reword { .. } => (
            format!("Reword {named}"),
            format!("reword {} on", c.short()),
        ),
        Quick::Author { .. } => (
            format!("Change the author and date of {named}"),
            format!("author of {} on", c.short()),
        ),
        Quick::Squash { .. } => (
            format!("Squash {named} into the commit below"),
            format!("squash {} on", c.short()),
        ),
        Quick::Fixup { .. } => (
            format!("Fix up {named} into the commit below"),
            format!("fixup {} on", c.short()),
        ),
        Quick::Move { toward, .. } => {
            let way = if *toward == Toward::Up { "up" } else { "down" };
            (
                format!("Move {named} {way}"),
                format!("move {} {way} on", c.short()),
            )
        }
        Quick::Drop { .. } => (format!("Drop {named}"), format!("drop {} from", c.short())),
        Quick::Edit { .. } => (
            format!("Stop at {named} to edit it"),
            format!("edit {} on", c.short()),
        ),
        Quick::Split { .. } => (format!("Split {named}"), format!("split {} on", c.short())),
        Quick::Fold { .. } => (
            format!("Fold the staged changes into {named}"),
            format!("fold into {} on", c.short()),
        ),
    };
    run_plan(
        &model,
        repo,
        reads,
        title,
        format!("sopen: {message} {head}"),
    )
}

/// `reword` of HEAD (03 §7.8.1): `commit --amend --only --allow-empty --cleanup=verbatim -F -`;
/// a staged file stays staged.
fn reword_head(this: &Commit, message: &str, repo: &Repo, reads: &Reads) -> Result<Plan, Refused> {
    kept_whole(this)?;
    rewritable(reads, &this.row.oid)?;
    check_message(message)?;
    let short = this.row.oid.short();
    let mut d = Draft::new(
        repo,
        format!("Reword {short} {}", shown(&this.row.subject)),
        format!("sopen: reword {short} on {}", head_shown(repo)),
    );
    d.take_head()?;
    on_remotes(&mut d, reads, "HEAD^!");
    let cmd = d
        .command(
            Class::Write,
            [
                "commit",
                "--amend",
                "--only",
                "--allow-empty",
                "--cleanup=verbatim",
                "-F",
                "-",
            ],
        )
        .feed("message", message.as_bytes());
    d.step(cmd);
    d.changes.push(format!(
        "{short} {}: its message (the staged changes stay staged)",
        shown(&this.row.subject)
    ));
    Ok(d.finish())
}

/// `author` of HEAD (03 §7.8.1): `commit --amend --only --allow-empty --cleanup=verbatim -F -
/// --author=<a> --date=<d>`, the commit's own message on stdin, `GIT_COMMITTER_DATE` per the
/// choice.
fn author_head(
    this: &Commit,
    author: &Author,
    repo: &Repo,
    reads: &Reads,
) -> Result<Plan, Refused> {
    kept_whole(this)?;
    rewritable(reads, &this.row.oid)?;
    author.check()?;
    let short = this.row.oid.short();
    let mut d = Draft::new(
        repo,
        format!(
            "Change the author and date of {short} {}",
            shown(&this.row.subject)
        ),
        format!("sopen: author of {short} on {}", head_shown(repo)),
    );
    d.take_head()?;
    on_remotes(&mut d, reads, "HEAD^!");
    let line = author.line(&this.committed);
    let mut cmd = d
        .command(
            Class::Write,
            [
                "commit".to_string(),
                "--amend".into(),
                "--only".into(),
                "--allow-empty".into(),
                "--cleanup=verbatim".into(),
                "-F".into(),
                "-".into(),
                format!("--author={} <{}>", line.name, line.email),
                format!("--date={}", line.date.word()),
            ],
        )
        .feed("message", this.message.as_bytes());
    if let Some(cd) = line.committer {
        cmd = cmd.env("GIT_COMMITTER_DATE", &cd.word());
    }
    d.step(cmd);
    d.changes.push(format!(
        "{short} {}: author {} <{}>, {}",
        shown(&this.row.subject),
        line.name,
        line.email,
        line.date.word()
    ));
    Ok(d.finish())
}

/// A planner run (03 §7.8.2 "The run"), and every quick edit but the HEAD forms: the backup
/// group, `fold`'s commit, then one `rebase -i` with the todo on stdin.
fn run_plan(
    model: &Model,
    repo: &Repo,
    reads: &Reads,
    title: String,
    message: String,
) -> Result<Plan, Refused> {
    if !repo.version.at_least(2, 22) {
        return refused(format!(
            "history edits need git 2.22 (--reschedule-failed-exec); this host has {}",
            repo.version.text
        ));
    }
    if model.update_refs && !repo.version.at_least(2, 38) {
        return refused(format!(
            "moving other branches along needs git 2.38; this host has {}",
            repo.version.text
        ));
    }
    let run = model.run(repo)?;
    for c in written(model) {
        rewritable(reads, c)?;
    }
    // The run's `-c core.commentChar=#` covers only the command that starts it: a Continue or
    // Skip reads the todo again under the repository's own setting (review round 1).
    refuse_comment_char(reads)?;
    // 03 §8.6: a rebase writes over an ignored file in its way, with no flag to stop it.
    let paths = match &model.base {
        Base::Commit(b) => rebase_paths(reads, b.as_str()),
        Base::Root => reads.records(need(&[
            "log",
            "-z",
            "--name-only",
            "--format=",
            "--no-renames",
            "HEAD",
        ])),
    };
    refuse_ignored(reads, paths, "This history edit")?;
    let mut d = Draft::new(repo, title, message);
    d.take_head()?;
    for m in &run.moves {
        d.take_ref(&m.refname);
    }
    // The index and the tracked changes as they are (S4-A): `fold` commits the staged ones,
    // and git's `--autostash` puts what was staged back unstaged (it applies its stash without
    // `--index`, git 2.56.1), so without this Undo would not give them back as they were.
    if model.fold.is_some() || (run.autostash && dirty(repo, false)) {
        d.take(BackupKind::Worktree, Source::WorkTree);
    }
    if let Some(into) = &model.fold {
        d.write(["commit".to_string(), format!("--fixup={}", into.as_str())]);
    }
    let mut args: Vec<String> = ["-c", "core.commentChar=#", "rebase", "-i"]
        .map(String::from)
        .to_vec();
    args.push("--reschedule-failed-exec".into());
    if run.autostash {
        args.push("--autostash".into());
    }
    if repo.version.at_least(2, 38) {
        args.push(if run.update_refs {
            "--update-refs".into()
        } else {
            "--no-update-refs".into()
        });
    }
    args.push(match &run.base {
        Base::Commit(b) => b.as_str().to_string(),
        Base::Root => "--root".into(),
    });
    let cmd = d
        .command(Class::Write, args)
        .env("GIT_SEQUENCE_EDITOR", SEQUENCE_EDITOR)
        .feed("todo", run.todo_text());
    d.step(cmd);
    describe(&mut d, model, &run);
    let range = match &model.base {
        Base::Commit(b) => format!("{}..HEAD", b.as_str()),
        Base::Root => "HEAD".into(),
    };
    on_remotes(&mut d, reads, &range);
    d.changes_files = true;
    Ok(d.finish())
}

/// The confirmation's lines of a run (03 §8.1): each row that changes, by hash and subject,
/// the branches it moves, what it drops, the uncommitted changes it stashes, the branches it
/// leaves.
fn describe(d: &mut Draft, model: &Model, run: &Run) {
    let mut dropped = Vec::new();
    for (i, row) in model.rows.iter().enumerate() {
        let c = &row.commit;
        let mut what: Vec<String> = Vec::new();
        match row.verb {
            Verb::Pick => {}
            Verb::Reword => what.push("reworded".into()),
            Verb::Edit => what.push("the run stops after it".into()),
            Verb::Squash => what.push("squashed into the commit below".into()),
            Verb::Fixup => what.push("fixed up into the commit below".into()),
            Verb::Drop => {
                what.push("dropped".into());
                dropped.push(format!("{} {}", c.row.oid.short(), shown(&c.row.subject)));
            }
        }
        if model.read.order.get(i) != Some(&c.row.oid) {
            what.push("moved".into());
        }
        if let Some(a) = &row.author {
            what.push(format!(
                "author {} <{}>, {}",
                a.name,
                a.email,
                a.date.word()
            ));
        }
        if model.fold.as_ref() == Some(&c.row.oid) {
            what.push("the staged changes folded in".into());
        }
        if !what.is_empty() {
            d.changes.push(format!(
                "{} {}: {}",
                c.row.oid.short(),
                shown(&c.row.subject),
                what.join(", ")
            ));
        }
    }
    for m in &run.moves {
        d.changes.push(format!(
            "{}: {} → moves along",
            short_branch(&m.refname),
            m.from.short()
        ));
    }
    if !dropped.is_empty() {
        d.loses.push(format!(
            "{} dropped: {}",
            plural(dropped.len(), "commit"),
            few(&dropped)
        ));
    }
    if run.autostash {
        d.notes.push(
            "the uncommitted changes are stashed for the run and put back after it \
             (--autostash); what was staged comes back unstaged, and Undo puts it back as it was"
                .into(),
        );
    }
    for b in &run.left_out {
        d.notes.push(format!(
            "{} is checked out in another worktree; it stays where it is",
            short_branch(&b.refname)
        ));
    }
    if !model.update_refs {
        for b in &model.branches {
            d.notes.push(format!(
                "{} stays at {}: moving other branches along is off",
                short_branch(&b.refname),
                b.tip.short()
            ));
        }
    }
}

/// Split at an edit stop (03 §7.8.1 `split`, §8.5).
fn split_stop(commit: &Oid, repo: &Repo, reads: &Reads) -> Result<Plan, Refused> {
    if repo.in_progress.operation != Some(Operation::Rebase) {
        return refused("Split is offered at an edit stop, and no rebase is stopped here");
    }
    // The last line done is an `edit` (the probe's `done-last`), with no conflict: not a stop
    // at a failed `exec` line or a conflict (review round 1).
    let at_edit =
        Banner::of(&repo.in_progress, &repo.status).is_some_and(|b| b.edit_stop().is_some());
    if !at_edit {
        return refused(
            "Split is offered at an edit stop, and this rebase stopped for another reason (a \
             conflict or a failed line)",
        );
    }
    if repo.head.oid() != Some(commit) {
        return refused(format!(
            "the edit stop is at {}, not {}",
            repo.head.oid().map_or("(none)", Oid::short),
            commit.short()
        ));
    }
    if dirty(repo, false) {
        return refused(
            "Split needs the work tree as the stop left it: commit or stash the changes first",
        );
    }
    let near = parse_log(&reads.get(commit_need(commit)))?;
    let Some(this) = near.first().filter(|n| &n.row.oid == commit) else {
        return refused(format!("{} is not read yet", commit.short()));
    };
    let short = commit.short();
    let Some(parent) = this.row.parents.first() else {
        return refused(format!(
            "{short} is the root commit: it has no HEAD^ to split it from"
        ));
    };
    if this.row.parents.len() > 1 {
        return refused(format!("{short} is a merge: it is not split"));
    }
    let paths = split_paths(&reads.get(paths_need(parent, commit)))?;
    if paths.is_empty() && reads.missing.borrow().is_empty() {
        return refused(format!(
            "{short} changes no file: there is nothing to split"
        ));
    }
    let mut d = Draft::new(
        repo,
        format!("Split {short} {}", shown(&this.row.subject)),
        format!("sopen: split {short}"),
    );
    d.take_head()?;
    d.write(["reset", "--mixed", parent.as_str()]);
    d.changes.push(format!(
        "HEAD: {short} → {} (mixed: its changes stay in the work tree, uncommitted)",
        parent.short()
    ));
    d.changes.push(format!(
        "{} to commit again in parts: {}",
        plural(paths.len(), "file"),
        few(&paths)
    ));
    d.notes.push(
        "the files it added are untracked now; Continue is refused while any of its files \
         still differs from HEAD"
            .into(),
    );
    d.changes_files = true;
    Ok(d.finish())
}

/// Continue at a split's stop (03 §7.8.1 `split`): refused while a recorded path differs.
fn continue_split(split: &Split, repo: &Repo, reads: &Reads) -> Result<Plan, Refused> {
    let mut differ: Vec<String> = Vec::new();
    for batch in backup::batches(&split.paths, 1) {
        let mut words: Vec<String> = [
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--ignored",
            "--",
        ]
        .map(String::from)
        .to_vec();
        words.extend(batch);
        differ.extend(status_paths(&reads.get(Need(words))));
    }
    if !differ.is_empty() {
        return refused(format!(
            "Continue is refused while {} still {} from HEAD: {}. Commit {} (or discard {}) \
             first: git would continue and leave {} out of history.",
            plural(differ.len(), "file"),
            if differ.len() == 1 {
                "differs"
            } else {
                "differ"
            },
            few(&differ),
            if differ.len() == 1 { "it" } else { "them" },
            if differ.len() == 1 { "it" } else { "them" },
            if differ.len() == 1 { "it" } else { "them" },
        ));
    }
    let cont = Action::Proceed(Proceed::Continue);
    for need in actions::needs(&cont, repo) {
        reads.get(need);
    }
    actions::plan(&cont, repo)
}

/// The paths of `status --porcelain=v2 -z` records: ordinary (`1`), renamed (`2`, then its
/// old path as a record of its own), unmerged (`u`), untracked (`?`) and ignored (`!`).
fn status_paths(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut records = bytes.split(|&b| b == 0).filter(|r| !r.is_empty());
    while let Some(r) = records.next() {
        let r = String::from_utf8_lossy(r);
        let fields = match r.as_bytes().first() {
            Some(b'1') => 8,
            Some(b'2') => 9,
            Some(b'u') => 10,
            Some(b'?' | b'!') => 1,
            _ => continue,
        };
        if let Some(path) = r.splitn(fields + 1, ' ').nth(fields) {
            out.push(path.to_string());
        }
        if r.starts_with('2') {
            records.next();
        }
    }
    out
}
