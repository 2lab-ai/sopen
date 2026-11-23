//! Backups, restore and undo (03 §8.2–§8.3): the group written before a history rewrite or a
//! destructive action — refs under `refs/sopen-backup/<stamp>/` and a manifest, in one
//! `update-ref --stdin` transaction; a remote's value through `refs/sopen-staging/<stamp>/`
//! (§8.4); `files` by the files snapshot snippet (§3.2) — the Backups list's groups, a restore's
//! entries and the window's undo. A group as the Backups list shows it is `model::Backup`, its
//! kinds `model::BackupKind`. S3-A writes it, with the files snapshot and files restore
//! [`Snippet`](super::runner::Snippet)s; the Backups list that shows it is S3-B's
//! (`gui::browser::git_panel::backups`).
//!
//! # The API (for S3-B and S3-C)
//!
//! - [`write`] writes a plan's group (the executor, `actions::execute`, calls it for
//!   `Step::Backup`); [`list`] reads every group, newest first, for the Backups list; [`group`]
//!   reads one.
//! - [`entries`] reads the values now of a group's manifest lines and makes the restore
//!   dialog's [`RestoreEntry`]s; the dialog toggles `chosen` and `replace`, and its
//!   `Action::Restore { stamp, entries }` is planned like any action. Undo is the same for
//!   [`Undo::newest`]'s group.
//! - Every group a run writes gets, once the run has ended, an `after` record
//!   ([`seal`]): the values its refs had then, which is what [`entries`] compares the values now
//!   with to say `moved since` (03 §8.3). It is a commit of the empty tree beside the manifest
//!   (`refs/sopen-backup/<stamp>/after`), not a kind of 03 §8.2's table, and the Backups list
//!   does not show it.
//! - What a backup commits (the manifest, the `files` commit, `after`) is made under sopen's own
//!   identity ([`IDENTITY`]), so a repository without `user.name` (a server one only reads) can
//!   still be backed up.

use super::actions::ResetMode;
use super::model::{Backup, BackupKind, BackupRef, Head, Oid};
use super::runner::{Class, Done, Ending, GitCommand, GitRunner, Handle, LogEntry, Snippet};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

/// Where every group's refs live: `refs/sopen-backup/<stamp>/<kind>` (03 §8.2).
pub const PREFIX: &str = "refs/sopen-backup/";

/// Where a remote's value is fetched before the transaction makes it `pushed/<r>/…` (03 §8.2,
/// §8.4): `refs/sopen-staging/<stamp>/<n>/<ref without refs/>`.
pub const STAGING: &str = "refs/sopen-staging/";

/// The name under the stamp of a group's `after` record ([`seal`]).
pub const AFTER: &str = "after";

/// The first line of a manifest's lines, after the title and a blank line: what tells a
/// manifest from any other commit message ([`Manifest::parse`]).
const MANIFEST_MARK: &str = "sopen-manifest";
/// The same for an `after` record.
const AFTER_MARK: &str = "sopen-after";

/// The author and committer of every commit a backup makes (the manifest, `files`, `after`,
/// and `stash create`'s `worktree`), set in the command's environment: a backup never fails
/// for a missing `user.name`, and never takes the owner's identity for what is not the owner's
/// commit.
pub const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "sopen"),
    ("GIT_AUTHOR_EMAIL", "sopen@localhost"),
    ("GIT_COMMITTER_NAME", "sopen"),
    ("GIT_COMMITTER_EMAIL", "sopen@localhost"),
];

/// The most argument bytes a snippet run takes before the paths go on in a second run (03 §3.2:
/// batches of at most 64 KiB, the runner's `MAX_ARGS`; a little is kept for the other words).
const BATCH: usize = 60 * 1024;

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
    /// manifest. The transaction that writes the group also verifies the ref still has that
    /// value, so a ref moved since the dialog fails the backup and nothing runs.
    Ref { refname: String, value: Option<Oid> },
    /// A stash list entry, by its commit (`stash`). `listed`: the entry is in the list now, so
    /// its commit is backed up; not listed (a restore about to store it back): the manifest
    /// records it `absent` with this commit, so the inverse can drop that entry by its hash
    /// (§8.3).
    Stash { commit: Oid, listed: bool },
    /// `stash create`'s commit of the index and the tracked changes at Run (`worktree`); none
    /// when clean, which the manifest records as `clean`.
    WorkTree,
    /// The current bytes of these paths at Run, by the files snapshot snippet (`files`): every
    /// collision found, never a subset (§8.2, §8.6); checked against the backup tree before the
    /// destructive command runs. A path with nothing at it at Run is recorded `absent` (a
    /// restore's own group: what it is about to create); one that is there but not in the tree
    /// (unreadable, a fifo) fails the backup, naming it.
    Files(Vec<String>),
    /// The remote's value of `refname`, fetched from each push endpoint into a staging ref at Run
    /// (`pushed/<r>/…`, §8.4); refused when the endpoints disagree.
    Pushed { remote: String, refname: String },
}

/// What a manifest line is about (03 §8.2: every ref and path the action can change).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    /// A local ref, whole (`refs/heads/main`, `refs/tags/v1`, `refs/remotes/origin/main`).
    Ref(String),
    /// A remote's ref (`pushed/<r>/…`, §8.4): the remote and its ref (`refs/heads/b`), so a line
    /// that says `absent` still names where to delete it.
    Pushed { remote: String, refname: String },
    /// HEAD, detached.
    Head,
    /// A stash list entry, by its commit (`stash list --format=%H`). The commit is the entry's
    /// identity even where the line says `absent` (an entry a restore stored back): the inverse
    /// drops the entry with that hash (§8.3), and no ref of the group holds it then.
    Stash { commit: Oid },
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
    /// The mode of the `reset` that moved HEAD in the run that wrote the group, when it ran one
    /// (S3-A): a reset's inverse uses its own mode, so undoing a soft or mixed reset moves HEAD
    /// (and the index) back and leaves the working files as they are (coordinator, 2026-11-23).
    /// Recorded as the line `moved-by <soft|mixed|keep|hard>` after `sopen-manifest`.
    pub moved_by: Option<ResetMode>,
}

impl Manifest {
    /// The commit message that records it, after the group's one-line `title` (03 §8.2): the
    /// title, a blank line, `sopen-manifest`, then one line per entry — `ref <refname> <value>`,
    /// `head <value>`, `stash <commit> <value>`, `worktree <value>`, `pushed <remote> <refname>
    /// <value>`, `path <value> <path>` — each value an object id, `absent` or `clean`. A path is
    /// the rest of its line, with `\` written `\\` and a newline `\n`; ref and remote names hold
    /// neither a space nor a newline (git's own rule).
    pub fn message(&self, title: &str) -> String {
        let mut text = one_line(title);
        text.push_str("\n\n");
        text.push_str(MANIFEST_MARK);
        text.push('\n');
        if let Some(mode) = self.moved_by {
            text.push_str(&format!("moved-by {}\n", mode_word(mode)));
        }
        for line in lines(&self.entries) {
            text.push_str(&line);
            text.push('\n');
        }
        text
    }

    /// The manifest a manifest commit's message records; `None` when it is not one (03 §8.3: a
    /// restore reads it).
    pub fn parse(message: &str) -> Option<Manifest> {
        let (_, body) = message.split_once("\n\n")?;
        let mut rows = body.split('\n');
        if rows.next()? != MANIFEST_MARK {
            return None;
        }
        let mut moved_by = None;
        let mut entries = Vec::new();
        for row in rows.filter(|row| !row.is_empty()) {
            match row.strip_prefix("moved-by ") {
                Some(word) => moved_by = Some(parse_mode(word)?),
                None => entries.push(parse_line(row)?),
            }
        }
        Some(Manifest { entries, moved_by })
    }
}

fn mode_word(mode: ResetMode) -> &'static str {
    match mode {
        ResetMode::Soft => "soft",
        ResetMode::Mixed => "mixed",
        ResetMode::Keep => "keep",
        ResetMode::Hard => "hard",
    }
}

fn parse_mode(word: &str) -> Option<ResetMode> {
    match word {
        "soft" => Some(ResetMode::Soft),
        "mixed" => Some(ResetMode::Mixed),
        "keep" => Some(ResetMode::Keep),
        "hard" => Some(ResetMode::Hard),
        _ => None,
    }
}

/// `text` on one line: newlines and carriage returns become spaces (a reflog message, a title).
pub fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

fn value_word(value: &Value) -> &str {
    match value {
        Value::Object(oid) => oid.as_str(),
        Value::Absent => "absent",
        Value::Clean => "clean",
        // Never recorded: a value now only.
        Value::Occupied => "occupied",
    }
}

fn parse_value(word: &str) -> Option<Value> {
    match word {
        "absent" => Some(Value::Absent),
        "clean" => Some(Value::Clean),
        _ => Oid::parse(word).map(Value::Object),
    }
}

/// A path as a manifest line keeps it: `\` → `\\`, newline → `\n`.
fn escape(path: &str) -> String {
    path.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            _ => return None,
        }
    }
    Some(out)
}

/// Each entry as its manifest line.
fn lines(entries: &[ManifestEntry]) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let v = value_word(&e.before);
            match &e.subject {
                Subject::Ref(refname) => format!("ref {refname} {v}"),
                Subject::Head => format!("head {v}"),
                Subject::Stash { commit } => format!("stash {} {v}", commit.as_str()),
                Subject::WorkTree => format!("worktree {v}"),
                Subject::Pushed { remote, refname } => format!("pushed {remote} {refname} {v}"),
                Subject::Path(path) => format!("path {v} {}", escape(path)),
            }
        })
        .collect()
}

fn parse_line(line: &str) -> Option<ManifestEntry> {
    let (word, rest) = line.split_once(' ')?;
    let (subject, before) = match word {
        "ref" => {
            let (refname, v) = rest.rsplit_once(' ')?;
            (Subject::Ref(refname.to_string()), parse_value(v)?)
        }
        "head" => (Subject::Head, parse_value(rest)?),
        "stash" => {
            let (commit, v) = rest.split_once(' ')?;
            (
                Subject::Stash {
                    commit: Oid::parse(commit)?,
                },
                parse_value(v)?,
            )
        }
        "worktree" => (Subject::WorkTree, parse_value(rest)?),
        "pushed" => {
            let mut w = rest.splitn(3, ' ');
            let (remote, refname, v) = (w.next()?, w.next()?, w.next()?);
            (
                Subject::Pushed {
                    remote: remote.to_string(),
                    refname: refname.to_string(),
                },
                parse_value(v)?,
            )
        }
        "path" => {
            let (v, path) = rest.split_once(' ')?;
            (Subject::Path(unescape(path)?), parse_value(v)?)
        }
        _ => return None,
    };
    Some(ManifestEntry { subject, before })
}

/// A backup group as written, or as the Backups list reads it, with its manifest (03 §8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub group: Backup,
    pub manifest: Manifest,
}

impl Record {
    /// The group's ref of `kind`.
    pub fn backup_ref(&self, kind: &BackupKind) -> Option<&BackupRef> {
        self.group.refs.iter().find(|r| &r.kind == kind)
    }

    /// The manifest's value of `subject`.
    pub fn before(&self, subject: &Subject) -> Option<&Value> {
        self.manifest
            .entries
            .iter()
            .find(|e| &e.subject == subject)
            .map(|e| &e.before)
    }
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
    /// A detached HEAD: `reset --keep <backup>`, after checking HEAD is still the value read for
    /// the dialog (as [`RestoreHow::CheckedOut`]; it refuses rather than lose a local change).
    Detached,
    /// `stash store -m '<its subject>' <backup>` (back at `stash@{0}`); `absent`: `stash drop
    /// stash@{k}`, `k` the entry whose hash is [`Subject::Stash`]'s commit, after its own `stash`
    /// backup.
    Stash,
    /// The tracked state backed up first, then `reset --hard HEAD` and, unless `clean`, `stash
    /// apply --index <backup>`; exact only while the changes land on the backup's base. With
    /// HEAD restored too, the reset comes before HEAD's move and the apply after it.
    WorkTree,
    /// The files restore snippet: raw, never over anything, never through a link.
    Files,
    /// `push --force-with-lease=<ref>:<the remote's value now> <r> <backup>:<ref>`; `absent`: the
    /// same lease with `--delete <ref>`, `<r>` and `<ref>` from [`Subject::Pushed`].
    Pushed,
}

/// One entry of a restore's dialog (03 §8.3): a manifest line of the group, its value then and
/// now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreEntry {
    pub subject: Subject,
    pub how: RestoreHow,
    /// The group's ref that holds the value to put back; `None` where the manifest says `absent`
    /// or `clean`: the restore removes, drops or cleans what the subject names (a stash entry by
    /// its commit, a remote's ref by its remote).
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

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

/// `<stamp>` (03 §8.2): the UTC time of Run as `YYYYMMDDTHHMMSSZ`, with `-2`, `-3`… when a stamp
/// in `taken` has it.
pub fn stamp(now: SystemTime, taken: &[String]) -> String {
    let base = crate::cache::utc_stamp(now);
    if !taken.contains(&base) {
        return base;
    }
    (2u32..)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken.contains(s))
        .expect("a free stamp")
}

/// `heads/main`, `detached`, `tags/v1`, `stash`, … : a kind's name under the stamp (03 §8.2).
pub fn kind_name(kind: &BackupKind) -> String {
    match kind {
        BackupKind::Head(b) => format!("heads/{b}"),
        BackupKind::Detached => "detached".into(),
        BackupKind::Tag(t) => format!("tags/{t}"),
        BackupKind::Stash => "stash".into(),
        BackupKind::Worktree => "worktree".into(),
        BackupKind::Files => "files".into(),
        BackupKind::Pushed(p) => format!("pushed/{p}"),
        BackupKind::Remote(r) => format!("remotes/{r}"),
        BackupKind::Manifest => "manifest".into(),
    }
}

/// The kind a name under the stamp says; `None` for anything else (an `after` record).
pub fn kind_of(name: &str) -> Option<BackupKind> {
    let kind = match name {
        "detached" => BackupKind::Detached,
        "stash" => BackupKind::Stash,
        "worktree" => BackupKind::Worktree,
        "files" => BackupKind::Files,
        "manifest" => BackupKind::Manifest,
        _ => {
            let (head, rest) = name.split_once('/')?;
            let rest = rest.to_string();
            match head {
                "heads" => BackupKind::Head(rest),
                "tags" => BackupKind::Tag(rest),
                "pushed" => BackupKind::Pushed(rest),
                "remotes" => BackupKind::Remote(rest),
                _ => return None,
            }
        }
    };
    Some(kind)
}

/// The kind a local ref is backed up as: `refs/heads/b` → `heads/b`, `refs/tags/t` → `tags/t`,
/// `refs/remotes/r/b` → `remotes/r/b`, `HEAD` → `detached`.
pub fn kind_for(refname: &str) -> Option<BackupKind> {
    if refname == "HEAD" {
        return Some(BackupKind::Detached);
    }
    let (prefix, rest) = [("refs/heads/", 0), ("refs/tags/", 1), ("refs/remotes/", 2)]
        .iter()
        .find_map(|(p, k)| refname.strip_prefix(p).map(|rest| (*k, rest)))?;
    let rest = rest.to_string();
    Some(match prefix {
        0 => BackupKind::Head(rest),
        1 => BackupKind::Tag(rest),
        _ => BackupKind::Remote(rest),
    })
}

/// The whole ref name of `kind` in the group `stamp`.
pub fn ref_name(stamp: &str, kind: &BackupKind) -> String {
    format!("{PREFIX}{stamp}/{}", kind_name(kind))
}

/// `pushed/<r>/<ref without refs/>`: the kind of a remote's ref (03 §8.2).
pub fn pushed_kind(remote: &str, refname: &str) -> BackupKind {
    BackupKind::Pushed(format!(
        "{remote}/{}",
        refname.strip_prefix("refs/").unwrap_or(refname)
    ))
}

// ---------------------------------------------------------------------------------------------
// The snippets (03 §3.2: exactly three besides `<locate>`; the probe is `actions`')

/// The start of every snippet that runs git: finds it as `<locate>` does (03 §2.4) — `SOPEN_GIT`
/// first when the session has it (locally that is sopen's own, 03 §2.4), then `PATH`, then
/// Homebrew's and the usual folders, `/usr/bin/git` on Darwin only when `xcode-select -p`
/// succeeds — into `$g`; without one `sopen: no git` and exit 127.
macro_rules! locate_git {
    () => {
        concat!(
            r#"ok() { [ -f "$1" ] && [ -x "$1" ] || return 1; [ "$1" = /usr/bin/git ] || return 0; "#,
            r#"[ "$(uname -s)" = Darwin ] || return 0; xcode-select -p >/dev/null 2>&1; }; "#,
            r#"g=; if [ -n "${SOPEN_GIT:-}" ] && ok "$SOPEN_GIT"; then g=$SOPEN_GIT; fi; "#,
            r#"if [ -z "$g" ]; then c=$(command -v git 2>/dev/null) && ok "$c" && g=$c; fi; "#,
            r#"if [ -z "$g" ]; then for c in /opt/homebrew/bin/git /usr/local/bin/git /usr/bin/git; do "#,
            r#"if ok "$c"; then g=$c; break; fi; done; fi; "#,
            r#"if [ -z "$g" ]; then echo "sopen: no git" >&2; exit 127; fi; "#,
        )
    };
}

/// The files snapshot (03 §8.2): `"$1"` a commit to start from (the previous batch's) or empty,
/// then the paths. Under a temporary index (`--git-path sopen-backup-<pid>.index`, removed on
/// exit, a stop included), from `read-tree --empty` (or the base): a symlink by `update-index
/// --add`, a regular file by `hash-object -w --no-filters` and one `--index-info` record (mode
/// `100755` when executable), anything else named on stderr and left out; then `write-tree` and
/// `commit-tree -p HEAD` (no parent while HEAD is unborn). Prints `absent <path>` for each path
/// with nothing at it and `commit <hash>` last, each ending in NUL. The tree is checked against
/// the paths by the caller.
pub static FILES_SNAPSHOT: Snippet = Snippet {
    name: "sopen-files-snapshot",
    script: concat!(
        locate_git!(),
        r#"base=$1; shift; "#,
        r#"idx=$("$g" rev-parse --git-path "sopen-backup-$$.index") || exit 1; "#,
        r#"case $idx in /*) ;; *) idx=$(pwd)/$idx ;; esac; "#,
        r#"trap 'rm -f "$idx" "$idx.lock"' EXIT; trap 'exit 1' HUP INT TERM; "#,
        r#"GIT_INDEX_FILE=$idx; export GIT_INDEX_FILE; "#,
        r#"if [ -n "$base" ]; then "$g" read-tree "$base" || exit 1; "#,
        r#"else "$g" read-tree --empty || exit 1; fi; "#,
        // A path behind a parent that is not a real folder (a link, a file) has nothing at it
        // within the work tree: recorded absent, never read through the link.
        r#"inside() { d=.; rest=$1; while case $rest in */*) true;; *) false;; esac; do "#,
        r#"c=${rest%%/*}; rest=${rest#*/}; d=$d/$c; "#,
        r#"if [ -L "$d" ] || [ ! -d "$d" ]; then return 1; fi; done; return 0; }; "#,
        r#"for p in "$@"; do "#,
        r#"if ! inside "$p"; then printf 'absent %s\0' "$p"; "#,
        r#"elif [ -L "./$p" ]; then "$g" update-index --add -- "./$p" || echo "sopen: not backed up: $p" >&2; "#,
        r#"elif [ -f "./$p" ]; then :; "#,
        r#"elif [ -e "./$p" ]; then echo "sopen: not a file or a link, not backed up: $p" >&2; "#,
        r#"else printf 'absent %s\0' "$p"; fi; done; "#,
        r#"for p in "$@"; do if inside "$p" && [ ! -L "./$p" ] && [ -f "./$p" ]; then "#,
        r#"if s=$("$g" hash-object -w --no-filters -- "./$p"); then "#,
        r#"if [ -x "./$p" ]; then m=100755; else m=100644; fi; "#,
        r#"printf '%s %s\t%s\0' "$m" "$s" "$p"; "#,
        r#"else echo "sopen: cannot read $p" >&2; fi; fi; done | "$g" update-index -z --index-info || exit 1; "#,
        r#"t=$("$g" write-tree) || exit 1; "#,
        r#"if "$g" rev-parse -q --verify HEAD >/dev/null; then "#,
        r#"c=$(echo 'sopen: files' | "$g" commit-tree -p HEAD "$t") || exit 1; "#,
        r#"else c=$(echo 'sopen: files' | "$g" commit-tree "$t") || exit 1; fi; "#,
        r#"printf 'commit %s\0' "$c""#,
    ),
};

/// The files restore (03 §8.3's `files` row): four arguments per entry, `<op> <mode> <sha>
/// <path>`. `op` is `c` (look only), `w` (write, never over anything), `r` (replace: what is in
/// the way — a leaf, or a parent component that is not a folder — removed first, a link as a
/// link, never its target; its bytes were backed up by the caller) or `d` (the manifest says
/// `absent`: `clean -f -x -- <path>` after the same parent check: the one path named, backed up
/// by the restore's own group, ignored or not). For each entry, (1) every parent
/// component of `./<path>` must be a real folder (a missing one is made with `mkdir`, one level),
/// (2) the leaf must not exist at all, (3) `cat-file blob <sha> > ./<path>` under `set -C`, the
/// executable bit for `100755`, `ln -s <the blob's text>` for `120000`, (4) the parents checked
/// again. Prints one record per entry — `<word>\0<path>\0<detail>\0` — with `<word>`:
/// - `c`: `free`, `file` (detail `<mode> <blob>`), `link`, `folder`, `other`, `notfolder`
///   (detail: the component);
/// - else: `written`, `removed`, `kept` (`d` left something git does not clean), `occupied`,
///   `notfolder` (detail: the component), `changed` (changed while restoring), `failed` (detail:
///   why).
///
/// Exits 1 when an entry was not done, 0 otherwise.
pub static FILES_RESTORE: Snippet = Snippet {
    name: "sopen-files-restore",
    script: concat!(
        locate_git!(),
        r#"rc=0; rec() { printf '%s\0%s\0%s\0' "$1" "$2" "$3"; }; "#,
        // parents OP PATH: 0 = every parent a real folder (made when missing, unless `c`/`d`);
        // else prints the record and returns 1.
        r#"parents() { d=.; rest=$2; "#,
        r#"while case $rest in */*) true;; *) false;; esac; do "#,
        r#"c=${rest%%/*}; rest=${rest#*/}; d=$d/$c; "#,
        r#"if [ -L "$d" ] || { [ -e "$d" ] && [ ! -d "$d" ]; }; then "#,
        r#"if [ "$1" = r ]; then rm -f -- "$d" || { rec failed "$2" "cannot remove $d"; return 1; }; "#,
        r#"else rec notfolder "$2" "${d#./}"; return 1; fi; fi; "#,
        r#"if [ ! -e "$d" ] && [ ! -L "$d" ]; then "#,
        r#"case $1 in c|d) return 0;; esac; mkdir -- "$d" 2>/dev/null; fi; "#,
        r#"if [ -L "$d" ] || [ ! -d "$d" ]; then rec notfolder "$2" "${d#./}"; return 1; fi; "#,
        r#"done; return 0; }; "#,
        r#"same() { d=.; rest=$1; "#,
        r#"while case $rest in */*) true;; *) false;; esac; do "#,
        r#"c=${rest%%/*}; rest=${rest#*/}; d=$d/$c; "#,
        r#"if [ -L "$d" ] || [ ! -d "$d" ]; then return 1; fi; done; return 0; }; "#,
        r#"while [ $# -ge 4 ]; do op=$1; m=$2; s=$3; p=$4; shift 4; "#,
        r#"if ! parents "$op" "$p"; then [ "$op" = c ] || rc=1; continue; fi; "#,
        r#"if [ "$op" = c ]; then "#,
        r#"if [ -L "./$p" ]; then rec link "$p" ""; "#,
        r#"elif [ -d "./$p" ]; then rec folder "$p" ""; "#,
        r#"elif [ -f "./$p" ]; then "#,
        r#"if [ -x "./$p" ]; then fm=100755; else fm=100644; fi; "#,
        r#"if h=$("$g" hash-object --no-filters -- "./$p"); then rec file "$p" "$fm $h"; "#,
        r#"else rec other "$p" ""; fi; "#,
        r#"elif [ -e "./$p" ]; then rec other "$p" ""; "#,
        r#"else rec free "$p" ""; fi; continue; fi; "#,
        r#"if [ "$op" = d ]; then "#,
        r#"if [ -e "./$p" ] || [ -L "./$p" ]; then "$g" clean -f -x -q -- "./$p" >/dev/null; fi; "#,
        r#"if [ -e "./$p" ] || [ -L "./$p" ]; then rec kept "$p" ""; rc=1; else rec removed "$p" ""; fi; "#,
        r#"continue; fi; "#,
        r#"if [ -e "./$p" ] || [ -L "./$p" ]; then "#,
        r#"if [ "$op" = r ]; then "#,
        r#"if [ -d "./$p" ] && [ ! -L "./$p" ]; then rm -rf -- "./$p"; else rm -f -- "./$p"; fi; "#,
        r#"if [ -e "./$p" ] || [ -L "./$p" ]; then rec failed "$p" "cannot remove it"; rc=1; continue; fi; "#,
        r#"else rec occupied "$p" ""; rc=1; continue; fi; fi; "#,
        r#"if [ "$m" = 120000 ]; then "#,
        r#"t=$("$g" cat-file blob "$s") && ln -s -- "$t" "./$p"; ok=$?; "#,
        r#"else ( set -C; "$g" cat-file blob "$s" > "./$p" ); ok=$?; "#,
        r#"if [ $ok = 0 ] && [ "$m" = 100755 ]; then chmod +x -- "./$p"; fi; fi; "#,
        r#"if [ $ok != 0 ]; then rec failed "$p" "cannot write it"; rc=1; continue; fi; "#,
        r#"if ! same "$p"; then rec changed "$p" ""; rc=1; continue; fi; "#,
        r#"rec written "$p" ""; done; exit $rc"#,
    ),
};

/// A path list cut into batches of at most [`BATCH`] quoted bytes (03 §3.2), each with the
/// words of an entry kept together (`per` words per entry).
pub fn batches(words: &[String], per: usize) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut size = 0;
    for entry in words.chunks(per.max(1)) {
        let bytes: usize = entry
            .iter()
            .map(|w| super::runner::quote(w.as_bytes()).len() + 1)
            .sum();
        if out.is_empty() || size + bytes > BATCH {
            out.push(Vec::new());
            size = 0;
        }
        size += bytes;
        out.last_mut().unwrap().extend(entry.iter().cloned());
    }
    out
}

/// One record of [`FILES_RESTORE`]'s output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub word: String,
    pub path: String,
    pub detail: String,
}

/// [`FILES_RESTORE`]'s output, its records in order.
pub fn restored(stdout: &[u8]) -> Vec<Restored> {
    let fields: Vec<String> = stdout
        .split(|&b| b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect();
    fields
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| Restored {
            word: c[0].clone(),
            path: c[1].clone(),
            detail: c[2].clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Running the reads and writes of a backup

/// The commands of one backup step (or one read of groups), kept as the log keeps them.
pub(crate) struct Session<'a> {
    pub runner: &'a GitRunner,
    pub handle: &'a Handle,
    pub top: &'a str,
    pub ran: Vec<Done>,
}

impl<'a> Session<'a> {
    pub fn new(runner: &'a GitRunner, handle: &'a Handle, top: &'a str) -> Self {
        Self {
            runner,
            handle,
            top,
            ran: Vec::new(),
        }
    }

    /// Runs `cmd`, keeping its `Done`; its stdout when it exits 0 (or with one of `codes`).
    pub fn run(&mut self, cmd: GitCommand, codes: &[i32]) -> Result<Vec<u8>, String> {
        let done = self.runner.run(&cmd, self.handle);
        let ok = matches!(done.ending, Ending::Exit(c) if c == 0 || codes.contains(&c));
        let out = if ok {
            Ok(done.stdout.clone())
        } else {
            Err(first_error(&done))
        };
        self.ran.push(done);
        out
    }

    pub fn read<I, S>(&mut self, args: I) -> Result<Vec<u8>, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString>,
    {
        self.run(GitCommand::new(Class::Read, self.top, args), &[])
    }

    fn failed(self, reason: String) -> Failed {
        Failed {
            reason,
            ran: self.ran,
        }
    }
}

/// A command's failure in one line: `<line>: <first error line or ending>`.
pub(crate) fn first_error(done: &Done) -> String {
    let text = done.error_text();
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("fatal:") || l.starts_with("error:") || l.starts_with("sopen:"))
        .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("")
        .to_string();
    format!("{}: {line}", done.entry.line)
}

/// A refusal made by sopen at Run, not by git: a `Done` that was never sent, its reason as its
/// text (03 §3.8's error dialog shows it as it shows git's), under `line`.
pub fn refusal(runner: &GitRunner, top: &str, line: &str, reason: &str) -> Done {
    Done {
        ending: Ending::NotSent(reason.to_string()),
        stdout: Vec::new(),
        stdout_cut: false,
        stderr: format!("sopen: {reason}"),
        entry: LogEntry {
            time: SystemTime::now(),
            host: runner.host().to_string(),
            repo: top.to_string(),
            line: line.to_string(),
            stdin: None,
            ending: Ending::NotSent(reason.to_string()),
            duration: Duration::ZERO,
            stdout_bytes: 0,
            stderr: format!("sopen: {reason}"),
        },
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// NUL-terminated records as text.
pub(crate) fn records(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|r| !r.is_empty())
        .map(|r| String::from_utf8_lossy(r).into_owned())
        .collect()
}

/// One `ls-tree -r -z` entry: `<mode> SP <type> SP <oid> TAB <path>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub oid: Oid,
    pub path: String,
}

/// `ls-tree -r -z`'s output.
pub fn tree_entries(bytes: &[u8]) -> Vec<TreeEntry> {
    records(bytes)
        .iter()
        .filter_map(|r| {
            let (head, path) = r.split_once('\t')?;
            let mut w = head.split(' ');
            let (mode, _, oid) = (w.next()?, w.next()?, w.next()?);
            Some(TreeEntry {
                mode: mode.to_string(),
                oid: Oid::parse(oid)?,
                path: path.to_string(),
            })
        })
        .collect()
}

/// The zero id in a repository whose ids are `like`'s length: `verify <ref> <zero>` says "must
/// not exist".
fn zero(like: &Oid) -> String {
    "0".repeat(like.as_str().len())
}

/// The empty tree's id in the repository (03 §6.2's form, which works in a SHA-256 one too).
fn empty_tree(s: &mut Session) -> Result<Oid, String> {
    let out = s.read(["hash-object", "-t", "tree", "--stdin"])?;
    Oid::parse(text(&out).trim()).ok_or_else(|| "git gave no empty tree".to_string())
}

/// `commit-tree <tree> [-p <parent>]` with `message` on stdin, under [`IDENTITY`].
fn commit_tree(
    s: &mut Session,
    tree: &Oid,
    parent: Option<&Oid>,
    message: &str,
) -> Result<Oid, String> {
    let mut args = vec!["commit-tree".to_string(), tree.as_str().to_string()];
    if let Some(p) = parent {
        args.extend(["-p".to_string(), p.as_str().to_string()]);
    }
    let mut cmd = GitCommand::new(Class::Write, s.top, args).feed("message", message.as_bytes());
    for (k, v) in IDENTITY {
        cmd = cmd.env(k, v);
    }
    let out = s.run(cmd, &[])?;
    Oid::parse(text(&out).trim()).ok_or_else(|| "commit-tree gave no commit".to_string())
}

/// Every stamp a group or a staging area already has (`for-each-ref` of both prefixes).
fn stamps_taken(s: &mut Session) -> Result<Vec<String>, String> {
    let out = s.read([
        "for-each-ref",
        "--format=%(refname)",
        "refs/sopen-backup/",
        "refs/sopen-staging/",
    ])?;
    let mut stamps: Vec<String> = text(&out)
        .lines()
        .filter_map(|r| {
            let rest = r.strip_prefix(PREFIX).or_else(|| r.strip_prefix(STAGING))?;
            rest.split('/').next().map(str::to_string)
        })
        .collect();
    stamps.dedup();
    Ok(stamps)
}

/// What one push endpoint holds of a ref (03 §8.4 step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
struct EndpointValue {
    url: String,
    value: Option<Oid>,
    staging: Option<String>,
}

/// 03 §8.4 steps 1–3 for one remote ref: the push endpoints (`remote get-url --push --all`),
/// each endpoint's value fetched into a staging ref under `stamp` (`ls-remote` says when it is
/// missing), and refused when they disagree. The value, and the staging refs to delete.
fn remote_value(
    s: &mut Session,
    stamp: &str,
    n: &mut usize,
    remote: &str,
    refname: &str,
    staged: &mut Vec<(String, Oid)>,
) -> Result<Option<Oid>, String> {
    let out = s.read(["remote", "get-url", "--push", "--all", remote])?;
    let urls: Vec<String> = text(&out)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if urls.is_empty() {
        return Err(format!("{remote} has no push URL"));
    }
    for url in &urls {
        super::actions::plain_word(url)?;
    }
    let short = refname.strip_prefix("refs/").unwrap_or(refname);
    let mut values = Vec::new();
    for url in urls {
        let listed = s.run(
            GitCommand::new(Class::Network, s.top, ["ls-remote", url.as_str(), refname]),
            &[],
        )?;
        let present = text(&listed)
            .lines()
            .any(|l| l.split_once('\t').is_some_and(|(_, name)| name == refname));
        if !present {
            values.push(EndpointValue {
                url,
                value: None,
                staging: None,
            });
            continue;
        }
        *n += 1;
        let staging = format!("{STAGING}{stamp}/{n}/{short}");
        s.run(
            GitCommand::new(
                Class::Network,
                s.top,
                [
                    "fetch".to_string(),
                    "--no-tags".to_string(),
                    "--no-write-fetch-head".to_string(),
                    url.clone(),
                    format!("+{refname}:{staging}"),
                ],
            ),
            &[],
        )?;
        let got = s.read(["rev-parse", "-q", "--verify", &staging])?;
        let value =
            Oid::parse(text(&got).trim()).ok_or_else(|| format!("{staging} was not written"))?;
        staged.push((staging.clone(), value.clone()));
        values.push(EndpointValue {
            url,
            value: Some(value),
            staging: Some(staging),
        });
    }
    let first = values[0].value.clone();
    if values.iter().any(|v| v.value != first) {
        let each: Vec<String> = values
            .iter()
            .map(|v| {
                let shown = v.value.as_ref().map_or("absent", |o| o.as_str());
                format!(
                    "{} has {shown}",
                    crate::git::runner::mask_credentials(&v.url)
                )
            })
            .collect();
        return Err(format!(
            "the push URLs of {remote} disagree on {refname}: {}",
            each.join("; ")
        ));
    }
    Ok(first)
}

/// Writes a plan's group in `top` (03 §8.2): the files snapshot and the staging fetches first,
/// then one `update-ref --create-reflog -m <message> --stdin` transaction creating every ref,
/// the manifest among them, and deleting the staging refs — all or nothing. When it fails the
/// action does not run, and staging refs left are deleted in a second transaction.
pub fn write(
    runner: &GitRunner,
    handle: &Handle,
    top: &str,
    message: &str,
    takes: &[Take],
) -> Result<Record, Failed> {
    let (result, ran) = write_logged(runner, handle, top, message, takes, None);
    result.map_err(|reason| Failed { reason, ran })
}

/// [`write`], with the commands it ran whatever the result (the executor's step lists them), and
/// the manifest's [`Manifest::moved_by`].
pub fn write_logged(
    runner: &GitRunner,
    handle: &Handle,
    top: &str,
    message: &str,
    takes: &[Take],
    moved_by: Option<ResetMode>,
) -> (Result<Record, String>, Vec<Done>) {
    let mut s = Session::new(runner, handle, top);
    let mut staged: Vec<(String, Oid)> = Vec::new();
    let result = write_in(&mut s, message, takes, moved_by, &mut staged);
    if result.is_err() && !staged.is_empty() {
        // 03 §8.2: the staging refs are deleted in a second transaction; any left is named.
        let input: String = staged
            .iter()
            .map(|(r, v)| format!("delete {r} {}\n", v.as_str()))
            .collect();
        let cmd = GitCommand::new(Class::Write, top, ["update-ref", "--stdin"])
            .feed("ref transaction", input.into_bytes());
        if s.run(cmd, &[]).is_err() {
            let names: Vec<&str> = staged.iter().map(|(r, _)| r.as_str()).collect();
            let result = result.map_err(|e| {
                format!(
                    "{e}; these staging refs could not be deleted: {}",
                    names.join(", ")
                )
            });
            return (result, s.ran);
        }
    }
    (result, s.ran)
}

fn write_in(
    s: &mut Session,
    message: &str,
    takes: &[Take],
    moved_by: Option<ResetMode>,
    staged: &mut Vec<(String, Oid)>,
) -> Result<Record, String> {
    let taken = stamps_taken(s)?;
    let stamp = stamp(SystemTime::now(), &taken);
    let empty = empty_tree(s)?;
    let mut lines: Vec<String> = Vec::new();
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let mut n = 0usize;
    let mut stash_list: Option<Vec<String>> = None;
    for take in takes {
        match &take.from {
            Source::Ref { refname, value } => {
                let subject = if refname == "HEAD" {
                    Subject::Head
                } else {
                    Subject::Ref(refname.clone())
                };
                match value {
                    Some(v) => {
                        lines.push(format!("verify {refname} {}", v.as_str()));
                        lines.push(format!(
                            "create {} {}",
                            ref_name(&stamp, &take.kind),
                            v.as_str()
                        ));
                        entries.push(ManifestEntry {
                            subject,
                            before: Value::Object(v.clone()),
                        });
                    }
                    None => {
                        lines.push(format!("verify {refname} {}", zero(&empty)));
                        entries.push(ManifestEntry {
                            subject,
                            before: Value::Absent,
                        });
                    }
                }
            }
            Source::Stash { commit, listed } => {
                if *listed {
                    let list = match &stash_list {
                        Some(l) => l,
                        None => {
                            let out = s.read(["stash", "list", "--format=%H"])?;
                            stash_list.insert(text(&out).lines().map(str::to_string).collect())
                        }
                    };
                    if !list.iter().any(|h| h == commit.as_str()) {
                        return Err(format!(
                            "the stash list changed: {} is no longer in it",
                            commit.short()
                        ));
                    }
                    lines.push(format!(
                        "create {} {}",
                        ref_name(&stamp, &take.kind),
                        commit.as_str()
                    ));
                }
                entries.push(ManifestEntry {
                    subject: Subject::Stash {
                        commit: commit.clone(),
                    },
                    before: if *listed {
                        Value::Object(commit.clone())
                    } else {
                        Value::Absent
                    },
                });
            }
            Source::WorkTree => {
                let mut cmd = GitCommand::new(Class::Write, s.top, ["stash", "create"]);
                for (k, v) in IDENTITY {
                    cmd = cmd.env(k, v);
                }
                let out = s.run(cmd, &[])?;
                let made = text(&out).trim().to_string();
                let before = if made.is_empty() {
                    Value::Clean
                } else {
                    let oid = Oid::parse(&made)
                        .ok_or_else(|| format!("stash create printed {made:?}"))?;
                    lines.push(format!(
                        "create {} {}",
                        ref_name(&stamp, &take.kind),
                        oid.as_str()
                    ));
                    Value::Object(oid)
                };
                entries.push(ManifestEntry {
                    subject: Subject::WorkTree,
                    before,
                });
            }
            Source::Files(paths) => {
                let (commit, values) = snapshot(s, paths)?;
                if let Some(commit) = commit {
                    lines.push(format!(
                        "create {} {}",
                        ref_name(&stamp, &take.kind),
                        commit.as_str()
                    ));
                }
                for (path, before) in values {
                    entries.push(ManifestEntry {
                        subject: Subject::Path(path),
                        before,
                    });
                }
            }
            Source::Pushed { remote, refname } => {
                let before = staged.len();
                let value = remote_value(s, &stamp, &mut n, remote, refname, staged)?;
                for (staging, v) in &staged[before..] {
                    lines.push(format!("delete {staging} {}", v.as_str()));
                }
                if let Some(v) = &value {
                    lines.push(format!(
                        "create {} {}",
                        ref_name(&stamp, &take.kind),
                        v.as_str()
                    ));
                }
                entries.push(ManifestEntry {
                    subject: Subject::Pushed {
                        remote: remote.clone(),
                        refname: refname.clone(),
                    },
                    before: value.map_or(Value::Absent, Value::Object),
                });
            }
        }
    }
    let manifest = Manifest { entries, moved_by };
    let title = one_line(message);
    let manifest_commit = commit_tree(s, &empty, None, &manifest.message(&title))?;
    lines.push(format!(
        "create {} {}",
        ref_name(&stamp, &BackupKind::Manifest),
        manifest_commit.as_str()
    ));
    let mut input = lines.join("\n");
    input.push('\n');
    let cmd = GitCommand::new(
        Class::Write,
        s.top,
        [
            "update-ref",
            "--create-reflog",
            "-m",
            title.as_str(),
            "--stdin",
        ],
    )
    .feed("ref transaction", input.into_bytes());
    s.run(cmd, &[])?;
    staged.clear();
    match read_groups(s, Some(&stamp))?.into_iter().next() {
        Some(record) => Ok(record),
        None => Err(format!("the group {stamp} was written but cannot be read")),
    }
}

/// A files snapshot: its commit (`None` when every path is absent) and each path's value.
type Snapshot = (Option<Oid>, Vec<(String, Value)>);

/// The files snapshot of `paths` (03 §8.2), in batches: the commit (`None` when every path is
/// absent) and each path's value — its blob, or `absent` when nothing is at it. A path that is
/// there but did not get into the tree fails it, naming the path.
fn snapshot(s: &mut Session, paths: &[String]) -> Result<Snapshot, String> {
    let mut base = String::new();
    let mut absent: Vec<String> = Vec::new();
    let mut commit: Option<Oid> = None;
    for batch in batches(paths, 1) {
        let mut args = vec![base.clone()];
        args.extend(batch);
        let mut cmd = GitCommand::snippet(Class::Write, s.top, &FILES_SNAPSHOT, args);
        for (k, v) in IDENTITY {
            cmd = cmd.env(k, v);
        }
        let out = s.run(cmd, &[])?;
        for rec in records(&out) {
            if let Some(path) = rec.strip_prefix("absent ") {
                absent.push(path.to_string());
            } else if let Some(c) = rec.strip_prefix("commit ") {
                commit = Oid::parse(c.trim());
            }
        }
        let made = commit
            .clone()
            .ok_or_else(|| "the files snapshot made no commit".to_string())?;
        base = made.as_str().to_string();
    }
    let commit = commit.ok_or_else(|| "the files snapshot made no commit".to_string())?;
    let out = s.read(["ls-tree", "-r", "-z", "--full-tree", commit.as_str()])?;
    let tree: BTreeMap<String, Oid> = tree_entries(&out)
        .into_iter()
        .map(|e| (e.path, e.oid))
        .collect();
    let mut values = Vec::new();
    let mut missing = Vec::new();
    for path in paths {
        match tree.get(path) {
            Some(oid) => values.push((path.clone(), Value::Object(oid.clone()))),
            None if absent.contains(path) => values.push((path.clone(), Value::Absent)),
            None => missing.push(path.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "not backed up, so nothing was done: {}",
            missing.join(", ")
        ));
    }
    let any = values.iter().any(|(_, v)| matches!(v, Value::Object(_)));
    Ok((any.then_some(commit), values))
}

/// The Backups list (03 §8.2, Cmd/Ctrl+B): every group under [`PREFIX`] in `top`, newest
/// first, with its manifest.
pub fn list(runner: &GitRunner, handle: &Handle, top: &str) -> Result<Vec<Record>, Failed> {
    let mut s = Session::new(runner, handle, top);
    match read_groups(&mut s, None) {
        Ok(records) => Ok(records),
        Err(reason) => Err(s.failed(reason)),
    }
}

/// One group, `None` when there is none with that stamp.
pub fn group(
    runner: &GitRunner,
    handle: &Handle,
    top: &str,
    stamp: &str,
) -> Result<Option<Record>, Failed> {
    let mut s = Session::new(runner, handle, top);
    match read_groups(&mut s, Some(stamp)) {
        Ok(records) => Ok(records.into_iter().next()),
        Err(reason) => Err(s.failed(reason)),
    }
}

/// A stamp's order: its time, then its `-N`.
fn stamp_order(stamp: &str) -> (String, u32) {
    match stamp.split_once('-') {
        Some((time, n)) => (time.to_string(), n.parse().unwrap_or(0)),
        None => (stamp.to_string(), 1),
    }
}

/// The groups (or the one of `stamp`), newest first: `for-each-ref` of the refs, then the
/// manifests' messages through one `cat-file --batch`.
fn read_groups(s: &mut Session, stamp: Option<&str>) -> Result<Vec<Record>, String> {
    let pattern = match stamp {
        Some(st) => format!("{PREFIX}{st}/"),
        None => PREFIX.to_string(),
    };
    let out = s.read([
        "for-each-ref".to_string(),
        "--format=%(refname)%00%(objectname)%00%(contents:subject)".to_string(),
        pattern,
    ])?;
    let mut groups: BTreeMap<String, Vec<BackupRef>> = BTreeMap::new();
    for line in text(&out).lines() {
        let mut f = line.splitn(3, '\0');
        let (Some(refname), Some(oid), Some(subject)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Some(rest) = refname.strip_prefix(PREFIX) else {
            continue;
        };
        let Some((st, name)) = rest.split_once('/') else {
            continue;
        };
        if stamp.is_some_and(|want| want != st) {
            continue;
        }
        let (Some(kind), Some(oid)) = (kind_of(name), Oid::parse(oid)) else {
            continue;
        };
        groups.entry(st.to_string()).or_default().push(BackupRef {
            kind,
            refname: refname.to_string(),
            oid,
            subject: subject.to_string(),
        });
    }
    let manifests: Vec<Oid> = groups
        .values()
        .filter_map(|refs| {
            refs.iter()
                .find(|r| r.kind == BackupKind::Manifest)
                .map(|r| r.oid.clone())
        })
        .collect();
    let messages = messages(s, &manifests)?;
    let mut records: Vec<Record> = groups
        .into_iter()
        .map(|(st, refs)| {
            let manifest_ref = refs.iter().find(|r| r.kind == BackupKind::Manifest);
            let message = manifest_ref
                .and_then(|r| messages.get(&r.oid))
                .cloned()
                .unwrap_or_default();
            let title = message.lines().next().unwrap_or("").to_string();
            Record {
                group: Backup {
                    stamp: st,
                    message: title,
                    refs,
                },
                manifest: Manifest::parse(&message).unwrap_or_default(),
            }
        })
        .collect();
    records.sort_by_key(|r| std::cmp::Reverse(stamp_order(&r.group.stamp)));
    Ok(records)
}

/// The messages of these commits, by `cat-file --batch` (`<oid> commit <size>`, the object, a
/// newline).
fn messages(s: &mut Session, commits: &[Oid]) -> Result<BTreeMap<Oid, String>, String> {
    let mut found = BTreeMap::new();
    if commits.is_empty() {
        return Ok(found);
    }
    let input: String = commits
        .iter()
        .map(|c| format!("{}\n", c.as_str()))
        .collect();
    let cmd = GitCommand::new(Class::Read, s.top, ["cat-file", "--batch"])
        .feed("object ids", input.into_bytes());
    let out = s.run(cmd, &[])?;
    let mut rest: &[u8] = &out;
    while !rest.is_empty() {
        let Some(nl) = rest.iter().position(|&b| b == b'\n') else {
            break;
        };
        let header = text(&rest[..nl]);
        rest = &rest[nl + 1..];
        let mut w = header.split(' ');
        let (Some(oid), Some(kind), Some(size)) = (w.next(), w.next(), w.next()) else {
            break;
        };
        let Ok(size) = size.parse::<usize>() else {
            break;
        };
        if size > rest.len() {
            break;
        }
        let object = &rest[..size];
        rest = rest.get(size + 1..).unwrap_or(&[]);
        if kind != "commit" {
            continue;
        }
        let body = object
            .windows(2)
            .position(|w| w == b"\n\n")
            .map_or(&object[..0], |at| &object[at + 2..]);
        if let Some(oid) = Oid::parse(oid) {
            found.insert(oid, text(body));
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------------------------------------
// `after`, restore entries

/// Writes the group's `after` record (see the module's notes): the values now of its ref, HEAD
/// and stash lines, as manifest lines under `sopen-after`, in a commit of the empty tree whose
/// parent is the manifest, created at `refs/sopen-backup/<stamp>/after`. Run by the executor
/// once the plan's steps have ended; a failure leaves `moved since` unknown — [`entries`] then
/// counts every changed line as moved — and the executor says so in the outcome's warning.
pub fn seal(
    runner: &GitRunner,
    handle: &Handle,
    top: &str,
    record: &Record,
) -> (Result<(), String>, Vec<Done>) {
    let mut s = Session::new(runner, handle, top);
    let result = seal_in(&mut s, record);
    (result, s.ran)
}

fn seal_in(s: &mut Session, record: &Record) -> Result<(), String> {
    let Some(manifest) = record.backup_ref(&BackupKind::Manifest) else {
        return Ok(());
    };
    let now = values_now(s, &record.manifest)?;
    let entries: Vec<ManifestEntry> = record
        .manifest
        .entries
        .iter()
        .filter_map(|e| {
            now.get(&e.subject).map(|v| ManifestEntry {
                subject: e.subject.clone(),
                before: v.clone(),
            })
        })
        .collect();
    let mut message = format!("{}\n\n{AFTER_MARK}\n", one_line(&record.group.message));
    for line in lines(&entries) {
        message.push_str(&line);
        message.push('\n');
    }
    let empty = empty_tree(s)?;
    let commit = commit_tree(s, &empty, Some(&manifest.oid), &message)?;
    let name = format!("{PREFIX}{}/{AFTER}", record.group.stamp);
    let input = format!("create {name} {}\n", commit.as_str());
    let cmd = GitCommand::new(Class::Write, s.top, ["update-ref", "--stdin"])
        .feed("ref transaction", input.into_bytes());
    s.run(cmd, &[])?;
    Ok(())
}

/// The values now of a manifest's ref, HEAD and stash lines (absent included).
fn values_now(s: &mut Session, manifest: &Manifest) -> Result<HashMap<Subject, Value>, String> {
    let mut now = HashMap::new();
    let refs: Vec<String> = manifest
        .entries
        .iter()
        .filter_map(|e| match &e.subject {
            Subject::Ref(r) => Some(r.clone()),
            _ => None,
        })
        .collect();
    if !refs.is_empty() {
        let mut args = vec![
            "for-each-ref".to_string(),
            "--format=%(objectname) %(refname)".to_string(),
        ];
        args.extend(refs.iter().cloned());
        let out = s.read(args)?;
        let found: BTreeMap<String, Oid> = text(&out)
            .lines()
            .filter_map(|l| {
                let (oid, name) = l.split_once(' ')?;
                Some((name.to_string(), Oid::parse(oid)?))
            })
            .collect();
        for r in refs {
            let v = found.get(&r).cloned().map_or(Value::Absent, Value::Object);
            now.insert(Subject::Ref(r), v);
        }
    }
    if manifest.entries.iter().any(|e| e.subject == Subject::Head) {
        let out = s.run(
            GitCommand::new(Class::Read, s.top, ["rev-parse", "-q", "--verify", "HEAD"]),
            &[1],
        )?;
        let v = Oid::parse(text(&out).trim()).map_or(Value::Absent, Value::Object);
        now.insert(Subject::Head, v);
    }
    let stashes: Vec<&Oid> = manifest
        .entries
        .iter()
        .filter_map(|e| match &e.subject {
            Subject::Stash { commit } => Some(commit),
            _ => None,
        })
        .collect();
    if !stashes.is_empty() {
        let out = s.read(["stash", "list", "--format=%H"])?;
        let list = text(&out);
        for c in stashes {
            let listed = list.lines().any(|h| h == c.as_str());
            let v = if listed {
                Value::Object(c.clone())
            } else {
                Value::Absent
            };
            now.insert(Subject::Stash { commit: c.clone() }, v);
        }
    }
    Ok(now)
}

/// A group's `after` record, when it has one.
fn after(s: &mut Session, stamp: &str) -> Result<Option<Manifest>, String> {
    let name = format!("{PREFIX}{stamp}/{AFTER}");
    let out = s.run(
        GitCommand::new(Class::Read, s.top, ["rev-parse", "-q", "--verify", &name]),
        &[1],
    )?;
    let Some(oid) = Oid::parse(text(&out).trim()) else {
        return Ok(None);
    };
    let found = messages(s, std::slice::from_ref(&oid))?;
    Ok(found.get(&oid).and_then(|m| {
        let (_, body) = m.split_once("\n\n")?;
        let mut rows = body.split('\n');
        if rows.next()? != AFTER_MARK {
            return None;
        }
        let entries = rows
            .filter(|r| !r.is_empty())
            .map(parse_line)
            .collect::<Option<Vec<_>>>()?;
        Some(Manifest {
            entries,
            moved_by: None,
        })
    }))
}

/// The restore dialog's entries for `record` (03 §8.3): each manifest line with its value now —
/// refs and HEAD read, the stash list, the work tree's tracked changes, each path looked at by
/// the files restore snippet (`c`), a remote's ref by `ls-remote` — `moved since` against the
/// group's `after` record, and chosen when there is something to put back and it did not move
/// (a path: where it is free, or freed by the chosen HEAD item's move). `head` is HEAD as the
/// panel read it (which branch is checked out).
pub fn entries(
    runner: &GitRunner,
    handle: &Handle,
    top: &str,
    record: &Record,
    head: &Head,
) -> Result<Vec<RestoreEntry>, Failed> {
    let mut s = Session::new(runner, handle, top);
    match entries_in(&mut s, record, head) {
        Ok(entries) => Ok(entries),
        Err(reason) => Err(s.failed(reason)),
    }
}

fn entries_in(s: &mut Session, record: &Record, head: &Head) -> Result<Vec<RestoreEntry>, String> {
    let mut now = values_now(s, &record.manifest)?;
    let left = after(s, &record.group.stamp)?;
    let left_of = |subject: &Subject| -> Option<Value> {
        left.as_ref().and_then(|m| {
            m.entries
                .iter()
                .find(|e| &e.subject == subject)
                .map(|e| e.before.clone())
        })
    };
    // The work tree: occupied when it has tracked changes now.
    if record
        .manifest
        .entries
        .iter()
        .any(|e| e.subject == Subject::WorkTree)
    {
        let out = s.read(["status", "--porcelain=v2", "-z", "--untracked-files=no"])?;
        let v = if out.is_empty() {
            Value::Clean
        } else {
            Value::Occupied
        };
        now.insert(Subject::WorkTree, v);
    }
    // Paths: looked at by the restore snippet.
    let paths: Vec<String> = record
        .manifest
        .entries
        .iter()
        .filter_map(|e| match &e.subject {
            Subject::Path(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    let mut looked: BTreeMap<String, Restored> = BTreeMap::new();
    if !paths.is_empty() {
        let words: Vec<String> = paths
            .iter()
            .flat_map(|p| ["c".to_string(), "-".into(), "-".into(), p.clone()])
            .collect();
        for batch in batches(&words, 4) {
            let out = s.run(
                GitCommand::snippet(Class::Read, s.top, &FILES_RESTORE, batch),
                &[1],
            )?;
            for r in restored(&out) {
                looked.insert(r.path.clone(), r);
            }
        }
    }
    let checked_out = match head {
        Head::Branch { refname, .. } => Some(refname.clone()),
        _ => None,
    };
    let mut entries = Vec::new();
    for e in &record.manifest.entries {
        let (how, kind) = match &e.subject {
            Subject::Ref(r) => {
                let how = if checked_out.as_deref() == Some(r.as_str()) {
                    RestoreHow::CheckedOut
                } else {
                    RestoreHow::Ref
                };
                (how, kind_for(r))
            }
            Subject::Head => (RestoreHow::Detached, Some(BackupKind::Detached)),
            Subject::Stash { .. } => (RestoreHow::Stash, Some(BackupKind::Stash)),
            Subject::WorkTree => (RestoreHow::WorkTree, Some(BackupKind::Worktree)),
            Subject::Path(_) => (RestoreHow::Files, Some(BackupKind::Files)),
            Subject::Pushed { remote, refname } => {
                (RestoreHow::Pushed, Some(pushed_kind(remote, refname)))
            }
        };
        let backup = match (&e.before, kind) {
            (Value::Object(_), Some(kind)) => record.backup_ref(&kind).cloned(),
            _ => None,
        };
        let value_now = match &e.subject {
            Subject::Path(p) => match looked.get(p) {
                Some(r) if r.word == "free" => Value::Absent,
                Some(r) if r.word == "file" => r
                    .detail
                    .split_once(' ')
                    .and_then(|(_, oid)| Oid::parse(oid))
                    .map_or(Value::Occupied, Value::Object),
                _ => Value::Occupied,
            },
            Subject::Pushed { remote, refname } => {
                // From the push URL, as §8.4's value is (the restore's own backup reads every
                // push URL again at Run and refuses when they disagree).
                let url = s
                    .read(["remote", "get-url", "--push", remote.as_str()])
                    .map(|out| text(&out).trim().to_string())
                    .unwrap_or_else(|_| remote.clone());
                if super::actions::plain_word(&url).is_err() {
                    // Never passed to git (review round 1): its value now is unknown.
                    Value::Occupied
                } else {
                    match s.run(
                        GitCommand::new(
                            Class::Network,
                            s.top,
                            ["ls-remote", url.as_str(), refname],
                        ),
                        &[],
                    ) {
                        Ok(out) => text(&out)
                            .lines()
                            .find_map(|l| {
                                let (oid, name) = l.split_once('\t')?;
                                (name == refname).then(|| Oid::parse(oid)).flatten()
                            })
                            .map_or(Value::Absent, Value::Object),
                        Err(_) => Value::Occupied,
                    }
                }
            }
            subject => now.get(subject).cloned().unwrap_or(Value::Absent),
        };
        let moved_since = match &e.subject {
            // A path is never written over: an occupied one waits for "replace" whatever moved.
            Subject::Path(_) | Subject::WorkTree => false,
            Subject::Pushed { .. } => value_now == Value::Occupied,
            subject => match left_of(subject) {
                Some(left) => value_now != e.before && value_now != left,
                // No `after` record (its seal failed): what changed since cannot be told from
                // what the action changed, so a changed line counts as moved and waits for an
                // explicit choice (review round 1).
                None => value_now != e.before,
            },
        };
        let differs = value_now != e.before;
        let chosen = match &e.subject {
            // A path is put back only where nothing is (or removed where the manifest says
            // absent); anything else waits for "replace" (03 §8.3: never over anything).
            Subject::Path(_) => matches!(
                (&e.before, &value_now),
                (Value::Object(_), Value::Absent) | (Value::Absent, Value::Object(_))
            ),
            _ => differs && !moved_since,
        };
        entries.push(RestoreEntry {
            subject: e.subject.clone(),
            how,
            backup,
            before: e.before.clone(),
            now: value_now,
            moved_since,
            chosen,
            replace: false,
        });
    }
    freed_by_the_move(s, &mut entries)?;
    Ok(entries)
}

/// 03 §8.3's order puts the `files` after HEAD's move: a path held now by HEAD's own tracked
/// file, as HEAD has it, where the chosen HEAD item's commit has no such path, is free by then
/// (the move removes it), so it is chosen like a free one — never "replace". The snippet's `w`
/// still refuses it at Run if something is there then.
fn freed_by_the_move(s: &mut Session, entries: &mut [RestoreEntry]) -> Result<(), String> {
    let Some(to) = entries
        .iter()
        .filter(|e| e.chosen && matches!(e.how, RestoreHow::CheckedOut | RestoreHow::Detached))
        .filter_map(|e| e.backup.as_ref().map(|b| b.oid.clone()))
        .next_back()
    else {
        return Ok(());
    };
    let waiting = |e: &RestoreEntry| {
        matches!(e.subject, Subject::Path(_))
            && !e.chosen
            && matches!((&e.before, &e.now), (Value::Object(_), Value::Object(_)))
    };
    if !entries.iter().any(waiting) {
        return Ok(());
    }
    let head = tree_entries(&s.read(["ls-tree", "-r", "-z", "--full-tree", "HEAD"])?);
    let there = tree_entries(&s.read(["ls-tree", "-r", "-z", "--full-tree", to.as_str()])?);
    for e in entries.iter_mut().filter(|e| waiting(e)) {
        let Subject::Path(p) = &e.subject else {
            continue;
        };
        let ours = head
            .iter()
            .any(|t| &t.path == p && e.now == Value::Object(t.oid.clone()));
        let under = format!("{p}/");
        e.chosen = ours
            && !there
                .iter()
                .any(|t| &t.path == p || t.path.starts_with(&under));
    }
    Ok(())
}

/// What [`entries`]' `c` look said of a path that stops a plain restore (an occupied leaf or a
/// parent that is not a folder), as the dialog lists it.
pub fn blocked(entry: &RestoreEntry) -> bool {
    matches!(entry.subject, Subject::Path(_))
        && !matches!(entry.now, Value::Absent)
        && entry.now != entry.before
}
