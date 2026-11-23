//! Backups, restore and undo (03 §8.2–§8.3): the group written before a history rewrite or a
//! destructive action — refs under `refs/sopen-backup/<stamp>/` and a manifest, in one
//! `update-ref --stdin` transaction; a remote's value through `refs/sopen-staging/<stamp>/`
//! (§8.4); `files` by the files snapshot snippet (§3.2) — the Backups list's groups, a restore's
//! entries and the window's undo. A group as the Backups list shows it is `model::Backup`, its
//! kinds `model::BackupKind`. S3-A writes it, with the files snapshot and files restore
//! [`Snippet`](super::runner::Snippet)s; the Backups list that shows it is S3-B's
//! (`gui::browser::git_panel::backups`).
//!
//! S3-0 scaffold: the types and the entry points 02 §6.2 names, `todo!()` inside. S3-A fills them
//! in and adds fields, variants and types; renaming or removing one goes through the dispatcher.

use super::model::{Backup, BackupKind, BackupRef, Oid};
use super::runner::{Done, GitRunner, Handle};
use std::time::SystemTime;

/// Where every group's refs live: `refs/sopen-backup/<stamp>/<kind>` (03 §8.2).
pub const PREFIX: &str = "refs/sopen-backup/";

/// Where a remote's value is fetched before the transaction makes it `pushed/<r>/…` (03 §8.2,
/// §8.4): `refs/sopen-staging/<stamp>/<n>/<ref without refs/>`.
pub const STAGING: &str = "refs/sopen-staging/";

/// One item a plan backs up before its first step (03 §8.2's table): the name it gets under the
/// stamp and where its value comes from at Run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Take {
    pub kind: BackupKind,
    pub from: Source,
    /// A ref a pruning command may delete (§8.2 "Pruning"): removed from the group after the
    /// command when it is still at the same value, so the group holds exactly what was pruned (a
    /// failed command keeps the whole group).
    pub pruned_only: bool,
}

/// Where a [`Take`]'s value comes from (03 §8.2, §8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A local ref — a branch, `HEAD` when detached, a tag (its tag object when annotated), a
    /// remote-tracking ref — at the value read for the dialog; `None`: absent, recorded so in the
    /// manifest.
    Ref { refname: String, value: Option<Oid> },
    /// A stash entry's commit (`stash`).
    Stash(Oid),
    /// `stash create`'s commit of the index and the tracked changes at Run (`worktree`); none
    /// when clean, which the manifest records as `clean`.
    WorkTree,
    /// The current bytes of these paths at Run, by the files snapshot snippet (`files`): every
    /// collision found, never a subset (§8.2, §8.6); checked against the backup tree before the
    /// destructive command runs.
    Files(Vec<String>),
    /// The remote's value of `refname`, fetched from each push endpoint into a staging ref at Run
    /// (`pushed/<r>/…`, §8.4); refused when the endpoints disagree.
    Pushed { remote: String, refname: String },
}

/// What a manifest line is about (03 §8.2: every ref and path the action can change).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    /// A ref, whole (`refs/heads/main`, `refs/tags/v1`, a remote's `refs/heads/b` under
    /// `pushed/`).
    Ref(String),
    /// HEAD, detached.
    Head,
    /// A stash list entry.
    Stash,
    /// The tracked state of the work tree and the index.
    WorkTree,
    /// A path from the top level.
    Path(String),
}

/// A manifest line's value (03 §8.2–§8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// An object id.
    Object(Oid),
    /// Nothing there: a restore removes it.
    Absent,
    /// The work tree had nothing to back up (§8.3's `worktree`).
    Clean,
    /// Only as a path's value now (a restore's dialog): something is at the path — a file, a
    /// folder, a link — and a restore never writes over it unless "replace" is chosen (§8.3).
    Occupied,
}

/// One line of a [`Manifest`] (03 §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    pub subject: Subject,
    /// Its value before the action.
    pub before: Value,
}

/// A group's manifest (03 §8.2): a commit of the empty tree whose message records, for every ref
/// and path the action can change, its value before — an object id or `absent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// The commit message that records it, after the group's one-line `title` (03 §8.2).
    pub fn message(&self, _title: &str) -> String {
        todo!("S3-A")
    }

    /// The manifest a manifest commit's message records; `None` when it is not one (03 §8.3: a
    /// restore reads it).
    pub fn parse(_message: &str) -> Option<Manifest> {
        todo!("S3-A")
    }
}

/// A backup group as written, or as the Backups list reads it, with its manifest (03 §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub group: Backup,
    pub manifest: Manifest,
}

/// Which row of 03 §8.3's table puts an entry back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreHow {
    /// A ref that is not the checked-out branch, a tag as its tag object: `update-ref --stdin`,
    /// compare-and-swap against the value read for the dialog (`update`, `create`, `delete`).
    Ref,
    /// The checked-out branch: `reset --keep <backup>`, after checking HEAD is still the value
    /// read for the dialog.
    CheckedOut,
    /// A detached HEAD: `checkout --no-overwrite-ignore --detach <backup>`.
    Detached,
    /// `stash store -m '<its subject>' <backup>` (back at `stash@{0}`); `absent`: `stash drop
    /// stash@{k}` after its own `stash` backup.
    Stash,
    /// The tracked state backed up first, then `reset --hard HEAD` and, unless `clean`, `stash
    /// apply --index <backup>`; exact only while HEAD is the backup's base.
    WorkTree,
    /// The files restore snippet: raw, never over anything, never through a link.
    Files,
    /// `push --force-with-lease=<ref>:<the remote's value now> <r> <backup>:<ref>`.
    Pushed,
}

/// One entry of a restore's dialog (03 §8.3): a manifest line of the group, its value then and
/// now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreEntry {
    pub subject: Subject,
    pub how: RestoreHow,
    /// The group's ref that holds the value to put back; `None` where the manifest says `absent`
    /// or `clean` (the restore removes or cleans).
    pub backup: Option<BackupRef>,
    /// Its value in the manifest.
    pub before: Value,
    /// Its value now, read for the dialog: what the restore compares and swaps against.
    pub now: Value,
    /// Changed since the group was written: listed as `moved since (now …)`; restoring it is a
    /// second, explicit choice.
    pub moved_since: bool,
    /// Chosen in the dialog.
    pub chosen: bool,
    /// A path: "replace" chosen for an occupied leaf or a parent that is not a folder (what is
    /// there is backed up as `files` first, then removed).
    pub replace: bool,
}

/// Cmd/Ctrl+Z's list (03 §8.3): the stamps of the groups written in this window, oldest first.
/// Undo restores the newest, whose restore writes a group of its own that becomes the newest, so
/// undo, redo and undo again each restore the previous group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Undo {
    pub stamps: Vec<String>,
}

impl Undo {
    /// A group written in this window, an action's or a restore's (03 §8.3).
    pub fn push(&mut self, stamp: String) {
        self.stamps.push(stamp);
    }

    /// The group Cmd/Ctrl+Z restores (03 §8.3); `None` before any.
    pub fn newest(&self) -> Option<&str> {
        self.stamps.last().map(String::as_str)
    }
}

/// Why a group was not written or read (03 §8.2): the reason, and the commands as the log keeps
/// them (§3.8).
#[derive(Debug, Clone)]
pub struct Failed {
    pub reason: String,
    pub ran: Vec<Done>,
}

/// `<stamp>` (03 §8.2): the UTC time of Run as `YYYYMMDDTHHMMSSZ`, with `-2`, `-3`… when a stamp
/// in `taken` has it.
pub fn stamp(_now: SystemTime, _taken: &[String]) -> String {
    todo!("S3-A")
}

/// Writes a plan's group in `top` (03 §8.2): the files snapshot and the staging fetches first,
/// then one `update-ref --create-reflog -m <message> --stdin` transaction creating every ref,
/// the manifest among them, and deleting the staging refs — all or nothing. When it fails the
/// action does not run, and staging refs left are deleted in a second transaction.
pub fn write(
    _runner: &GitRunner,
    _handle: &Handle,
    _top: &str,
    _message: &str,
    _takes: &[Take],
) -> Result<Record, Failed> {
    todo!("S3-A")
}

/// The Backups list (03 §8.2, Cmd/Ctrl+B): every group under [`PREFIX`] in `top`, newest
/// first, with its manifest.
pub fn list(_runner: &GitRunner, _handle: &Handle, _top: &str) -> Result<Vec<Record>, Failed> {
    todo!("S3-A")
}
