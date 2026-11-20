//! The engine: a [`Job`]'s items, one at a time, over [`Hosts`] (04 §5).
//!
//! A file is the unit (04 §5.1): it is copied into a staged temp `.<name>.sopen-<random>` in its
//! target folder, checked (its size, the hash of the bytes read against the bytes written where
//! they passed through sopen, and its source's type, size and mtime unchanged), and published by
//! a rename that never replaces ([`FileOps::rename_new`]); or it is not there at all. A folder is
//! walked as it was when its copy began, so nothing made by the copy is copied again. A move
//! removes a source only after its whole item is copied and checked, re-checking each file right
//! before its removal.

use super::{Answer, Clash, Control, Hosts, Item, ItemOutcome, Job, Op, Progress, Report, STOPPED};
use crate::remote::ops::{is_no_rename, is_taken, FileOps, Staged, REMOTE_FOLDER_RENAME};
use crate::remote::{
    describe_change, is_cancelled, join, random_hex, suffixed, Created, EntryKind, Stat,
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The longest name a folder holds (bytes; `NAME_MAX` of the filesystems sopen meets).
const NAME_MAX: usize = 255;

/// Highest `-N` suffix tried for a free name.
const MAX_SUFFIX: u32 = 10_000;

/// Why copy and move refuse a server without `hardlink@openssh.com` (04 §5.6).
const NO_LINK: &str = "this server does not offer hardlink@openssh.com, which sopen needs to put \
    a copy under its name without the risk of replacing anything there; copy and move to it come \
    with slice S6";

/// A report for a job refused before anything happened: every item not reached, the reason
/// its first note.
pub(super) fn refused(names: &[String], reason: String) -> Report {
    Report {
        items: names
            .iter()
            .map(|n| Item {
                name: n.clone(),
                outcome: ItemOutcome::NotReached,
            })
            .collect(),
        notes: vec![reason],
    }
}

/// The outcome of the item Esc stopped inside: what is left of it.
fn stopped(what: &str) -> ItemOutcome {
    ItemOutcome::Failed(format!("{STOPPED}: {what}"))
}

/// Whether `name` can be a name in a folder (04 §5.1 "Names"): not empty, not `.` or `..`, no
/// `/` (nor NUL), at most 255 bytes. The error says why, for the dialog.
pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a name cannot be empty".into());
    }
    if name == "." || name == ".." {
        return Err(format!("'{name}' cannot be a name"));
    }
    if name.contains('/') {
        return Err(format!("a name cannot hold '/': {name}"));
    }
    if name.contains('\0') {
        return Err("a name cannot hold a NUL byte".into());
    }
    if name.len() > NAME_MAX {
        return Err(format!(
            "a name has at most {NAME_MAX} bytes; this one has {}",
            name.len()
        ));
    }
    Ok(())
}

/// `.<name>.sopen-<random>`, within [`NAME_MAX`] (a long name is shortened at a character).
fn temp_name(name: &str) -> String {
    let tail = format!(".sopen-{}", random_hex());
    let mut keep = NAME_MAX - 1 - tail.len();
    while !name.is_char_boundary(keep.min(name.len())) {
        keep -= 1;
    }
    format!(".{}{tail}", &name[..keep.min(name.len())])
}

/// Runs `job` over `hosts` on this thread: every item, in order, to its end; the report says how
/// each ended. Stops between files and between a file's steps when [`Control::cancel`] is set.
pub fn run(job: &Job, hosts: Hosts<'_>, ctl: &mut dyn Control) -> Report {
    let ends = match hosts {
        Hosts::Same(fs) => Ends { a: fs, b: None },
        Hosts::Two { src, dst } => Ends {
            a: src,
            b: Some(dst),
        },
    };
    let cancel = ctl.cancel();
    let mut engine = Engine {
        job,
        ends,
        ctl,
        cancel,
        standing: None,
        report: Report::default(),
        lines: Vec::new(),
        verb: "copying",
        item: 0,
    };
    if let Err(why) = engine.check() {
        return refused(&job.names, why);
    }
    for (i, name) in job.names.iter().enumerate() {
        engine.item = i + 1;
        if engine.stopped() {
            let rest = job.names.len() - i;
            engine.report.notes.push(format!(
                "stopped (Esc) before {name}: {rest} item(s) not reached"
            ));
            engine.not_reached(i);
            break;
        }
        let (outcome, flow) = engine.one(name);
        engine.report.items.push(Item {
            name: name.clone(),
            outcome,
        });
        let lines = std::mem::take(&mut engine.lines);
        engine.report.items.extend(lines);
        let rest = job.names.len() - i - 1;
        let note = match flow {
            Flow::Go => continue,
            Flow::Stop if rest == 0 => None,
            Flow::Stop => Some(format!(
                "stopped (Esc) in {name}: {rest} item(s) after it not reached"
            )),
            Flow::Cancel => Some(format!(
                "cancelled at {name}, a name in the way: {rest} item(s) after it not done"
            )),
        };
        engine.report.notes.extend(note);
        engine.not_reached(i + 1);
        break;
    }
    engine.report
}

/// The connections: one for both sides (`b` empty), or a source and a target.
struct Ends<'a> {
    a: &'a mut dyn FileOps,
    b: Option<&'a mut dyn FileOps>,
}

impl Ends<'_> {
    fn same(&self) -> bool {
        self.b.is_none()
    }

    fn src(&mut self) -> &mut dyn FileOps {
        &mut *self.a
    }

    fn dst(&mut self) -> &mut dyn FileOps {
        match self.b.as_mut() {
            Some(b) => &mut **b,
            None => &mut *self.a,
        }
    }
}

/// A moved source right before its removal.
enum Still {
    Same,
    /// Removed meanwhile: nothing to remove.
    Gone,
    /// Not the entry that was copied: kept, for this reason.
    Changed(String),
}

/// After an item: go on, or end the operation.
enum Flow {
    Go,
    /// Esc.
    Stop,
    /// Cancel at a clash.
    Cancel,
}

/// The name an item is to get at its target, after any clash.
enum Resolved {
    Name(String),
    Skip,
    Cancel,
}

/// How a file, link or folder of an item got to its target.
enum Placed {
    At(Copied),
    Skipped,
    Cancelled,
    /// Esc: what is left.
    Stopped(String),
    Failed(String),
}

/// A file (or link) published at its target: its final name, and its source as it was read —
/// what the removal of a moved source checks again.
struct Copied {
    name: String,
    before: Stat,
    id: Option<(u64, u64)>,
    /// A link's target text, as copied.
    target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Link,
    /// A fifo, socket or device: never acted on.
    Special,
    /// A name that is not UTF-8: never acted on (01 §3a).
    NotUtf8,
    /// The listing could not say; decided by an `lstat` when it is reached.
    Unknown,
}

impl Kind {
    fn of(own: &Stat) -> Self {
        if own.is_symlink {
            Kind::Link
        } else if own.is_dir {
            Kind::Dir
        } else if own.is_file {
            Kind::File
        } else {
            Kind::Special
        }
    }
}

/// An entry below a folder being copied: its path below the folder (`sub/name`).
struct Node {
    rel: String,
    kind: Kind,
}

/// Hashes what passes through a reader (the bytes read, for the check against the bytes
/// written).
struct HashRead<'a> {
    inner: Box<dyn Read + 'a>,
    hasher: Sha256,
}

impl Read for HashRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

/// A hashing sink that fails its writes once Esc is set, which ends the read feeding it.
struct Checked<'a> {
    hasher: Sha256,
    stop: &'a AtomicBool,
}

impl std::io::Write for Checked<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        self.hasher.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Why a file's copy into its temp ended without a temp to publish.
enum StageErr {
    Stopped,
    Failed(String),
}

fn named(name: &str, want: &str) -> ItemOutcome {
    if want == name {
        ItemOutcome::Done
    } else {
        ItemOutcome::Renamed(want.to_string())
    }
}

/// `rel`'s folder below `root` and its name.
fn split_rel<'r>(root: &str, rel: &'r str) -> (String, &'r str) {
    match rel.rfind('/') {
        Some(i) => (join(root, &rel[..i]), &rel[i + 1..]),
        None => (root.to_string(), rel),
    }
}

/// The folders above `rel` (`a/b/c` → `a/b`, `a`, and `""` for the item's own folder).
fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    let cuts = rel.match_indices('/').map(|(i, _)| &rel[..i]);
    std::iter::once("").chain(cuts)
}

struct Engine<'j, 'h, 'c> {
    job: &'j Job,
    ends: Ends<'h>,
    ctl: &'c mut dyn Control,
    cancel: Arc<AtomicBool>,
    /// "The same for the rest": the answer every later clash gets without asking.
    standing: Option<Answer>,
    report: Report,
    /// The current item's lines for its entries (skipped, kept), after its own line.
    lines: Vec<Item>,
    verb: &'static str,
    item: usize,
}

impl Engine<'_, '_, '_> {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn not_reached(&mut self, from: usize) {
        for name in &self.job.names[from..] {
            self.report.items.push(Item {
                name: name.clone(),
                outcome: ItemOutcome::NotReached,
            });
        }
    }

    fn line(&mut self, name: String, outcome: ItemOutcome) {
        self.lines.push(Item { name, outcome });
    }

    /// Refusals before anything happens: names, the target folder, a server that cannot
    /// publish, a folder into itself.
    fn check(&mut self) -> Result<(), String> {
        let job = self.job;
        if job.names.is_empty() {
            return Err("nothing is selected".into());
        }
        for name in &job.names {
            valid_name(name)?;
        }
        match &job.op {
            Op::Rename { to } => {
                valid_name(to)?;
                if job.names.len() != 1 {
                    return Err("a rename is of one entry".into());
                }
            }
            Op::Copy | Op::Move => {
                let dir = &job.dst_dir;
                match self.ends.dst().stat_opt(dir) {
                    Ok(Some(s)) if s.is_dir => {}
                    Ok(Some(_)) => return Err(format!("{dir} is not a folder")),
                    Ok(None) => return Err(format!("{dir}: no such folder")),
                    Err(e) => return Err(format!("{e:#}")),
                }
                if !self.ends.dst().can_link() {
                    return Err(NO_LINK.into());
                }
                if self.ends.same() {
                    self.refuse_into_itself()?;
                }
            }
        }
        Ok(())
    }

    /// A folder copied or moved into itself or below itself (04 §5.2): decided on resolved
    /// paths, and on this machine also by the device and inode of the target and each folder
    /// above it — as given and as resolved — so a link or an alias cannot lead a folder into
    /// itself.
    fn refuse_into_itself(&mut self) -> Result<(), String> {
        let job = self.job;
        let verb = if job.op == Op::Move { "move" } else { "copy" };
        let fs = self.ends.src();
        let err = |e: anyhow::Error| format!("{e:#}");
        for name in &job.names {
            let from = join(&job.src_dir, name);
            let Some(own) = fs.lstat_opt(&from).map_err(err)? else {
                continue;
            };
            if Kind::of(&own) != Kind::Dir {
                continue;
            }
            let refuse = || {
                format!(
                    "cannot {verb} {name} into itself: {} is {name} or inside it",
                    job.dst_dir
                )
            };
            let src_real = fs.realpath(&from).map_err(err)?;
            let dst_real = fs.realpath(&job.dst_dir).map_err(err)?;
            let inside = format!("{}/", src_real.trim_end_matches('/'));
            if dst_real == src_real || dst_real.starts_with(&inside) {
                return Err(refuse());
            }
            let Some(id) = fs.identity(&from, false).map_err(err)? else {
                continue;
            };
            for start in [job.dst_dir.as_str(), dst_real.as_str()] {
                let mut at = start.trim_end_matches('/').to_string();
                while !at.is_empty() {
                    if fs.identity(&at, true).ok().flatten() == Some(id) {
                        return Err(refuse());
                    }
                    at = match at.rfind('/') {
                        Some(i) => at[..i].to_string(),
                        None => String::new(),
                    };
                }
            }
        }
        Ok(())
    }

    fn one(&mut self, name: &str) -> (ItemOutcome, Flow) {
        match &self.job.op {
            Op::Rename { to } => {
                let to = to.clone();
                (self.rename(name, &to), Flow::Go)
            }
            Op::Copy => self.transfer(name, false),
            Op::Move => self.transfer(name, true),
        }
    }

    fn progress(&mut self, name: &str, done: u64, total: u64) {
        let p = Progress {
            verb: self.verb,
            item: self.item,
            items: self.job.names.len(),
            name: name.to_string(),
            done,
            total,
        };
        self.ctl.progress(&p);
    }

    // ---- rename (F2) ----

    /// F2 (04 §5.3): never onto an existing name; a change of case or Unicode form only, which
    /// the filesystem answers with "exists" for the source itself, goes through a free temp name.
    fn rename(&mut self, name: &str, to: &str) -> ItemOutcome {
        self.verb = "renaming";
        if name == to {
            return ItemOutcome::Done;
        }
        let dir = self.job.src_dir.clone();
        let (from, target) = (join(&dir, name), join(&dir, to));
        self.progress(name, 0, 0);
        let fs = self.ends.src();
        let own = match fs.lstat_opt(&from) {
            Ok(Some(own)) => own,
            Ok(None) => return ItemOutcome::Failed(format!("{name}: no such file or folder")),
            Err(e) => return ItemOutcome::Failed(format!("{e:#}")),
        };
        if Kind::of(&own) == Kind::Dir && !fs.can_rename_dir() {
            return ItemOutcome::Failed(REMOTE_FOLDER_RENAME.to_string());
        }
        let e = match fs.rename_new(&from, &target) {
            Ok(notes) => {
                self.report.notes.extend(notes);
                return ItemOutcome::Done;
            }
            Err(e) => e,
        };
        if !is_taken(&e) {
            return ItemOutcome::Failed(format!("{e:#}"));
        }
        match self.is_itself(&dir, name, to, &from, &target) {
            Ok(true) => self.rename_through_temp(&dir, name, to, &from, &target),
            Ok(false) => ItemOutcome::Failed(format!("{to} exists")),
            Err(e) => ItemOutcome::Failed(e),
        }
    }

    /// Is the "exists" of `to` the source itself? Only when the two names differ by case or
    /// Unicode form alone, locally the two stat to one device and inode, and a fresh listing
    /// holds the source's spelling and none spelled like `to` (remotely attributes prove nothing,
    /// so the listing alone).
    fn is_itself(
        &mut self,
        dir: &str,
        name: &str,
        to: &str,
        from: &str,
        target: &str,
    ) -> Result<bool, String> {
        // Only a change of case or of Unicode form can be the source itself; any other name
        // that "exists" is another entry (a second hard link of the same file is one too).
        if name_key(name) != name_key(to) {
            return Ok(false);
        }
        let fs = self.ends.src();
        let err = |e: anyhow::Error| format!("{e:#}");
        if let Some(id) = fs.identity(from, false).map_err(err)? {
            if fs.identity(target, false).map_err(err)? != Some(id) {
                return Ok(false);
            }
        }
        // And the folder holds one entry for both spellings, not two.
        let names = fs.list_dir(dir).map_err(err)?;
        Ok(names.iter().any(|n| n == name) && !names.iter().any(|n| n == to))
    }

    /// Two steps through a free temp name, each never replacing; when the second fails, the temp
    /// goes back to the original name, and when that fails too the report names where it is.
    fn rename_through_temp(
        &mut self,
        dir: &str,
        name: &str,
        to: &str,
        from: &str,
        target: &str,
    ) -> ItemOutcome {
        let fs = self.ends.src();
        let mut temp = None;
        for _ in 0..8 {
            let t = join(dir, &temp_name(name));
            match fs.rename_new(from, &t) {
                Ok(notes) => {
                    self.report.notes.extend(notes);
                    temp = Some(t);
                    break;
                }
                Err(e) if is_taken(&e) => continue,
                Err(e) => return ItemOutcome::Failed(format!("{e:#}")),
            }
        }
        let Some(temp) = temp else {
            return ItemOutcome::Failed(format!("no free temp name for {name} in {dir}"));
        };
        let second = match fs.rename_new(&temp, target) {
            Ok(notes) => {
                self.report.notes.extend(notes);
                return ItemOutcome::Done;
            }
            Err(e) => e,
        };
        match fs.rename_new(&temp, from) {
            Ok(notes) => {
                self.report.notes.extend(notes);
                ItemOutcome::Failed(format!(
                    "cannot rename {name} to {to}: {second:#}; it is back under {name}"
                ))
            }
            Err(back) => ItemOutcome::Failed(format!(
                "cannot rename {name} to {to}: {second:#}; renaming it back failed too \
                 ({back:#}): it is now at {temp}"
            )),
        }
    }

    // ---- copy and move (F5, F6) ----

    fn transfer(&mut self, name: &str, mv: bool) -> (ItemOutcome, Flow) {
        self.verb = if mv { "moving" } else { "copying" };
        let (src_dir, dst_dir) = (self.job.src_dir.clone(), self.job.dst_dir.clone());
        let from = join(&src_dir, name);
        let own = match self.ends.src().lstat_opt(&from) {
            Ok(Some(own)) => own,
            Ok(None) => {
                let why = format!("{name}: no such file or folder");
                return (ItemOutcome::Failed(why), Flow::Go);
            }
            Err(e) => return (ItemOutcome::Failed(format!("{e:#}")), Flow::Go),
        };
        let kind = Kind::of(&own);
        if kind == Kind::Special {
            self.report.notes.push(format!(
                "{name} is not a file, a folder or a link (a fifo, a socket or a device); it was \
                 skipped"
            ));
            return (ItemOutcome::Skipped, Flow::Go);
        }
        let mut want = match self.resolve(&dst_dir, name, true) {
            Ok(Resolved::Name(n)) => n,
            Ok(Resolved::Skip) => return (ItemOutcome::Skipped, Flow::Go),
            Ok(Resolved::Cancel) => return (ItemOutcome::Skipped, Flow::Cancel),
            Err(e) => return (ItemOutcome::Failed(e), Flow::Go),
        };
        let nothing = if mv {
            "nothing of it was moved"
        } else {
            "nothing of it was copied"
        };
        // A move on one host is a rename where one is possible (04 §5.2).
        if mv && self.ends.same() && (kind != Kind::Dir || self.ends.src().can_rename_dir()) {
            loop {
                // A question may have been open meanwhile: Esc then renames nothing.
                if self.stopped() {
                    return (stopped(nothing), Flow::Stop);
                }
                match self.ends.src().rename_new(&from, &join(&dst_dir, &want)) {
                    Ok(notes) => {
                        self.report.notes.extend(notes);
                        return (named(name, &want), Flow::Go);
                    }
                    Err(e) if is_taken(&e) => match self.resolve(&dst_dir, &want, false) {
                        Ok(Resolved::Name(n)) => want = n,
                        Ok(Resolved::Skip) => return (ItemOutcome::Skipped, Flow::Go),
                        Ok(Resolved::Cancel) => return (ItemOutcome::Skipped, Flow::Cancel),
                        Err(e) => return (ItemOutcome::Failed(e), Flow::Go),
                    },
                    Err(e) if is_no_rename(&e) => break,
                    Err(e) => return (ItemOutcome::Failed(format!("{e:#}")), Flow::Go),
                }
            }
        }
        if self.stopped() {
            return (stopped(nothing), Flow::Stop);
        }
        if kind == Kind::Dir {
            return self.folder(name, &from, &dst_dir, want, mv);
        }
        let placed = if kind == Kind::Link {
            self.copy_link(&from, &dst_dir, want, name, true)
        } else {
            self.copy_file(&from, &dst_dir, want, name, true)
        };
        match placed {
            Placed::At(copied) if mv => (self.remove_moved(name, &from, &copied), Flow::Go),
            Placed::At(copied) => (named(name, &copied.name), Flow::Go),
            Placed::Skipped => (ItemOutcome::Skipped, Flow::Go),
            Placed::Cancelled => (ItemOutcome::Skipped, Flow::Cancel),
            Placed::Stopped(what) => (stopped(&what), Flow::Stop),
            Placed::Failed(why) => (ItemOutcome::Failed(why), Flow::Go),
        }
    }

    /// The name the item gets in `dir`: `name` when it is free (with `check`, by `lstat`;
    /// without, the filesystem has just said it is taken), else what the dialog answers — or
    /// "the same for the rest" answers without asking.
    fn resolve(&mut self, dir: &str, name: &str, check: bool) -> Result<Resolved, String> {
        let mut want = name.to_string();
        let mut look = check;
        loop {
            if look {
                match self.ends.dst().lstat_opt(&join(dir, &want)) {
                    Ok(None) => return Ok(Resolved::Name(want)),
                    Ok(Some(_)) => {}
                    Err(e) => return Err(format!("{e:#}")),
                }
            }
            look = true;
            let answer = match self.standing.clone() {
                Some(Answer::Rename(_)) => Answer::Rename(self.next_free(dir, &want)?),
                Some(answer) => answer,
                None => {
                    let free = self.next_free(dir, &want)?;
                    let reply = self.ctl.ask(&Clash {
                        name: want.clone(),
                        free,
                    });
                    if reply.same_for_rest {
                        self.standing = Some(reply.answer.clone());
                    }
                    reply.answer
                }
            };
            match answer {
                Answer::Skip => return Ok(Resolved::Skip),
                Answer::Cancel => return Ok(Resolved::Cancel),
                Answer::Rename(n) => {
                    valid_name(&n)?;
                    want = n;
                }
            }
        }
    }

    /// The first free `<stem>-N<ext>` in `dir`, by `lstat` (the dialog's prefill; the create or
    /// link decides).
    fn next_free(&mut self, dir: &str, name: &str) -> Result<String, String> {
        for n in 1..MAX_SUFFIX {
            let candidate = suffixed(name, n);
            match self.ends.dst().lstat_opt(&join(dir, &candidate)) {
                Ok(None) => return Ok(candidate),
                Ok(Some(_)) => continue,
                Err(e) => return Err(format!("{e:#}")),
            }
        }
        Err(format!("no free name for {name} in {dir}"))
    }

    /// Removes this operation's temp; one that cannot be removed is a note naming it.
    fn drop_temp(&mut self, temp: &str) {
        if let Err(e) = self.ends.dst().remove(temp) {
            self.report
                .notes
                .push(format!("the temp file {temp} could not be removed: {e:#}"));
        }
    }

    /// Copies the regular file `from` into a temp in `dir`, checks it, and publishes it as
    /// `want` without replacing. `top`: the item itself — a name taken at the publish asks the
    /// dialog; inside a folder the copy made, it fails the item. A move's copy that the host made
    /// itself (sopen never saw its bytes) is then read back in full against its source (04
    /// §5.2), whatever its size: a copy that differs goes again and the source stays; Esc while
    /// checking stops with both copies there.
    fn copy_file(
        &mut self,
        from: &str,
        dir: &str,
        mut want: String,
        shown: &str,
        top: bool,
    ) -> Placed {
        let before = match self.ends.src().lstat_opt(from) {
            Ok(Some(s)) if Kind::of(&s) == Kind::File => s,
            Ok(Some(s)) => {
                return Placed::Failed(format!(
                    "{shown} is no longer a regular file ({})",
                    describe_kind(&s)
                ))
            }
            Ok(None) => return Placed::Failed(format!("{shown} is gone")),
            Err(e) => return Placed::Failed(format!("{e:#}")),
        };
        // Locally the inode the removal of a moved source checks again; a source whose identity
        // cannot be read is not copied (fail closed). Over SFTP there is none (`None`).
        let id = match self.ends.src().identity(from, false) {
            Ok(id) => id,
            Err(e) => return Placed::Failed(format!("{e:#}")),
        };
        let temp = join(dir, &temp_name(&want));
        let nothing = || "nothing of it was copied (its temp was removed)".to_string();
        let (staged, read) = match self.stage(from, &temp, before.size, shown) {
            Ok(got) => got,
            Err(StageErr::Stopped) => return Placed::Stopped(nothing()),
            Err(StageErr::Failed(why)) => return Placed::Failed(why),
        };
        if let Err(why) = self.check_copy(from, shown, &before, &staged, read) {
            self.drop_staged(&temp, staged.id);
            return Placed::Failed(why);
        }
        if self.stopped() {
            self.drop_staged(&temp, staged.id);
            return Placed::Stopped(nothing());
        }
        // The name is published only while it is still the file the copy was written through
        // (this machine: its inode, by the descriptor). Anything else there is not sopen's.
        if let Err(why) = self.is_staged(&temp, staged.id) {
            return Placed::Failed(format!("{why}; nothing was published"));
        }
        let verify = self.job.op == Op::Move && staged.sha256.is_none();
        // Over SFTP, with no inode: what gets the final name, by its attributes, to know it again
        // if it must go.
        let published = if verify && staged.id.is_none() {
            self.ends.dst().lstat_opt(&temp).ok().flatten()
        } else {
            None
        };
        let target = loop {
            let target = join(dir, &want);
            match self.ends.dst().rename_new(&temp, &target) {
                Ok(notes) => {
                    // The copy's notes (a mode or times not kept) name the file it now is; the
                    // publish's (its temp left as a second name) name the temp, which is there.
                    let kept = staged.notes.iter().map(|n| n.replace(&temp, &target));
                    self.report.notes.extend(kept);
                    self.report.notes.extend(notes);
                    break target;
                }
                Err(e) if is_taken(&e) && top => match self.resolve(dir, &want, false) {
                    Ok(Resolved::Name(_)) if self.stopped() => {
                        self.drop_staged(&temp, staged.id);
                        return Placed::Stopped(nothing());
                    }
                    Ok(Resolved::Name(n)) => want = n,
                    other => {
                        self.drop_staged(&temp, staged.id);
                        return match other {
                            Ok(Resolved::Skip) => Placed::Skipped,
                            Ok(_) => Placed::Cancelled,
                            Err(why) => Placed::Failed(why),
                        };
                    }
                },
                Err(e) => {
                    self.drop_staged(&temp, staged.id);
                    return Placed::Failed(format!("{e:#}"));
                }
            }
        };
        // Still the copy under its final name? (The name could change hands between the check
        // above and the rename.) If not, nothing is removed and what is there is left.
        if let Err(why) = self.is_staged(&target, staged.id) {
            return Placed::Failed(why);
        }
        let copied = Copied {
            name: want,
            before,
            id,
            target: None,
        };
        if verify {
            match self.verify(from, &target, shown, before.size) {
                // Verified only while the source is still the entry that was read.
                Ok(()) => match self.still(from, &copied) {
                    Still::Same => {}
                    Still::Gone => {
                        return Placed::Failed(format!(
                            "{shown} went before its copy {target} was verified, so this is no \
                             verified move; the copy stays"
                        ))
                    }
                    Still::Changed(how) => {
                        return Placed::Failed(format!(
                            "{shown} changed before its copy {target} was verified ({how}); it \
                             was kept, so both copies exist"
                        ))
                    }
                },
                Err(StageErr::Stopped) => {
                    return Placed::Stopped(format!(
                        "while checking its copy {target}: {shown} was not removed, so both \
                         copies exist"
                    ))
                }
                // A failed read-back is never a move: the source is not removed, whatever is
                // found afterwards. The copy goes only when the source is certainly still the
                // entry that was read (then the copy is what is wrong); otherwise it stays.
                Err(StageErr::Failed(why)) => {
                    let what = match self.still(from, &copied) {
                        Still::Same => {
                            let gone = self.unpublish(&target, staged.id, published);
                            format!("{gone}; {shown} stays")
                        }
                        Still::Gone => format!("{shown} is gone, so the copy {target} stays"),
                        Still::Changed(how) => format!(
                            "{shown} cannot be compared any more ({how}): it was kept, so both \
                             copies exist"
                        ),
                    };
                    return Placed::Failed(format!("{why}; {what}"));
                }
            }
        }
        Placed::At(copied)
    }

    /// Is `path` still the file the copy was written through? On this machine by its inode — a
    /// failure to read it is a no (fail closed). Over SFTP there is no inode (`id` is `None`):
    /// taken to be, as for every SFTP name.
    fn is_staged(&mut self, path: &str, id: Option<(u64, u64)>) -> Result<(), String> {
        let Some(id) = id else {
            return Ok(());
        };
        match self.ends.dst().identity(path, false) {
            Ok(Some(now)) if now == id => Ok(()),
            Ok(_) => Err(format!(
                "{path} is not the copy sopen made (another file has its name now); it is left \
                 as it is"
            )),
            Err(e) => Err(format!(
                "{path} cannot be confirmed as the copy sopen made ({e:#}); it is left as it is"
            )),
        }
    }

    /// Removes the temp of a copy that goes no further — only while it is still that copy.
    fn drop_staged(&mut self, temp: &str, id: Option<(u64, u64)>) {
        match self.is_staged(temp, id) {
            Ok(()) => self.drop_temp(temp),
            Err(why) => self.report.notes.push(why),
        }
    }

    /// Removes a copy just published that proved wrong — only while it is still the file that
    /// got the name: on this machine its inode, by the descriptor it was written through; over
    /// SFTP its type, size and mtime as published. What it says.
    fn unpublish(
        &mut self,
        target: &str,
        id: Option<(u64, u64)>,
        published: Option<Stat>,
    ) -> String {
        let still = match id {
            Some(_) => self.is_staged(target, id).is_ok(),
            None => match (published, self.ends.dst().lstat_opt(target)) {
                (Some(was), Ok(Some(now))) => {
                    Kind::of(&now) == Kind::of(&was)
                        && now.size == was.size
                        && now.mtime == was.mtime
                }
                _ => false,
            },
        };
        if !still {
            return format!("{target} is left as it is (it may not be the copy any more)");
        }
        match self.ends.dst().remove(target) {
            Ok(()) => "its copy was removed".to_string(),
            Err(e) => format!("its copy {target} could not be removed: {e:#}"),
        }
    }

    /// The copy of `from` into `temp`: on this host by [`FileOps::copy_within`], between hosts
    /// streamed through this machine. A failure has removed the temp it created.
    fn stage(
        &mut self,
        from: &str,
        temp: &str,
        total: u64,
        shown: &str,
    ) -> Result<(Staged, Option<[u8; 32]>), StageErr> {
        self.progress(shown, 0, total);
        let (verb, item, items) = (self.verb, self.item, self.job.names.len());
        let Self {
            ends, ctl, cancel, ..
        } = self;
        let mut progress = |done: u64| {
            ctl.progress(&Progress {
                verb,
                item,
                items,
                name: shown.to_string(),
                done,
                total,
            })
        };
        let (result, read) = match ends.b.as_mut() {
            None => (ends.a.copy_within(from, temp, cancel, &mut progress), None),
            Some(dst) => {
                let opened = ends
                    .a
                    .open_read(from)
                    .map_err(|e| StageErr::Failed(format!("{e:#}")))?;
                let mut reader = HashRead {
                    inner: opened.read,
                    hasher: Sha256::new(),
                };
                let r = dst.stage_from(&mut reader, &opened.meta, temp, cancel, &mut progress);
                (r, Some(reader.hasher.finalize().into()))
            }
        };
        let e = match result {
            Ok(staged) => return Ok((staged, read)),
            Err(e) => e,
        };
        let stopped = is_cancelled(&e.error);
        let mut why = format!("{:#}", e.error);
        match e.created {
            Created::No => {}
            Created::Yes => {
                if let Err(re) = self.ends.dst().remove(temp) {
                    let left = format!("the temp file {temp} could not be removed: {re:#}");
                    if stopped {
                        self.report.notes.push(left);
                    } else {
                        why = format!("{why}; {left}");
                    }
                }
            }
            Created::Unknown => {
                let check = format!("creating {temp} got no reply: check whether it is there");
                if stopped {
                    self.report.notes.push(check);
                } else {
                    why = format!("{why}; {check}");
                }
            }
        }
        Err(if stopped {
            StageErr::Stopped
        } else {
            StageErr::Failed(why)
        })
    }

    /// A move's check of a copy the host made (`copy-data`, `copy_file_range`), whatever its
    /// size: the SHA-256 of the source and of the copy, each read through in full,
    /// compared. Shown as `checking`, both reads one bar; Esc stops it.
    fn verify(&mut self, from: &str, copy: &str, shown: &str, size: u64) -> Result<(), StageErr> {
        let (item, items) = (self.item, self.job.names.len());
        let Self {
            ends, ctl, cancel, ..
        } = self;
        let total = size.saturating_mul(2);
        let mut hash =
            |fs: &mut dyn FileOps, path: &str, base: u64| -> Result<[u8; 32], StageErr> {
                if cancel.load(Ordering::Relaxed) {
                    return Err(StageErr::Stopped);
                }
                let mut sink = Checked {
                    hasher: Sha256::new(),
                    stop: cancel,
                };
                let mut progress = |done: u64| {
                    ctl.progress(&Progress {
                        verb: "checking",
                        item,
                        items,
                        name: shown.to_string(),
                        done: base + done,
                        total,
                    })
                };
                match fs.read_to(path, &mut sink, &mut progress) {
                    Ok(_) if cancel.load(Ordering::Relaxed) => Err(StageErr::Stopped),
                    Ok(_) => Ok(sink.hasher.finalize().into()),
                    Err(_) if cancel.load(Ordering::Relaxed) => Err(StageErr::Stopped),
                    Err(e) => Err(StageErr::Failed(format!("{e:#}"))),
                }
            };
        let read = hash(ends.src(), from, 0)?;
        let written = hash(ends.dst(), copy, size)?;
        if read != written {
            return Err(StageErr::Failed(format!(
                "the copy of {shown} does not hold the same bytes as {shown}"
            )));
        }
        Ok(())
    }

    /// The check after a copy (04 §5.1, §5.2): the temp holds as many bytes as the source had;
    /// where the bytes passed through sopen, the hash of those read equals the hash of those
    /// written; and the source is still the regular file of the same size and mtime it was when
    /// its read began (a source changing during its copy fails the item).
    fn check_copy(
        &mut self,
        from: &str,
        shown: &str,
        before: &Stat,
        staged: &Staged,
        read: Option<[u8; 32]>,
    ) -> Result<(), String> {
        let changed = |how: String| {
            format!("{shown} changed while it was copied ({how}); its copy was removed")
        };
        if staged.source.size != before.size {
            let how = format!("size {} → {}", before.size, staged.source.size);
            return Err(changed(how));
        }
        if staged.size != before.size {
            return Err(format!(
                "the copy of {shown} has {} bytes, not {}; it was removed",
                staged.size, before.size
            ));
        }
        if let (Some(written), Some(read)) = (staged.sha256, read) {
            if written != read {
                return Err(format!(
                    "the bytes written for {shown} differ from the bytes read; its copy was removed"
                ));
            }
        }
        match self.ends.src().lstat_opt(from) {
            Ok(Some(now)) if Kind::of(&now) != Kind::File => Err(changed(describe_kind(&now))),
            Ok(Some(now)) if now.size != before.size || now.mtime != before.mtime => {
                Err(changed(describe_change(&before.fingerprint(), &now)))
            }
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(changed("it was removed".into())),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    /// A link made again at the target with the same target text (04 §5.1 "Links"); then the
    /// source is checked again — a link changed while it was copied fails the item and its copy
    /// goes.
    fn copy_link(
        &mut self,
        from: &str,
        dir: &str,
        mut want: String,
        shown: &str,
        top: bool,
    ) -> Placed {
        let before = match self.ends.src().lstat_opt(from) {
            Ok(Some(s)) if s.is_symlink => s,
            Ok(_) => return Placed::Failed(format!("{shown} is no longer a link")),
            Err(e) => return Placed::Failed(format!("{e:#}")),
        };
        // The link's own inode, never followed; fail closed where it cannot be read.
        let id = match self.ends.src().identity(from, false) {
            Ok(id) => id,
            Err(e) => return Placed::Failed(format!("{e:#}")),
        };
        let target = match self.ends.src().read_link(from) {
            Ok(t) => t,
            Err(e) => return Placed::Failed(format!("{e:#}")),
        };
        let made = loop {
            let made = join(dir, &want);
            match self.ends.dst().make_symlink(&target, &made) {
                Ok(()) => break made,
                Err(e) if is_taken(&e) && top => match self.resolve(dir, &want, false) {
                    Ok(Resolved::Name(_)) if self.stopped() => {
                        return Placed::Stopped("nothing of it was copied".into())
                    }
                    Ok(Resolved::Name(n)) => want = n,
                    Ok(Resolved::Skip) => return Placed::Skipped,
                    Ok(_) => return Placed::Cancelled,
                    Err(why) => return Placed::Failed(why),
                },
                Err(e) => return Placed::Failed(format!("{e:#}")),
            }
        };
        let copied = Copied {
            name: want,
            before,
            id,
            target: Some(target.clone()),
        };
        let how = match self.still(from, &copied) {
            Still::Same => return Placed::At(copied),
            Still::Gone => "it was removed".to_string(),
            Still::Changed(how) => how,
        };
        // The copy made here goes — only while it is still that link.
        let ours = matches!(self.ends.dst().read_link(&made), Ok(t) if t == target);
        let gone = if !ours {
            format!("{made} is left as it is")
        } else {
            match self.ends.dst().remove(&made) {
                Ok(()) => "its copy was removed".to_string(),
                Err(e) => format!("its copy {made} could not be removed: {e:#}"),
            }
        };
        Placed::Failed(format!(
            "{shown} changed while it was copied ({how}); {gone}"
        ))
    }

    /// Is a moved source still the entry that was copied (04 §5.1 "Before removing a source")?
    /// The same type, size and mtime, and locally the same inode.
    fn still(&mut self, path: &str, copied: &Copied) -> Still {
        let before = &copied.before;
        let now = match self.ends.src().lstat_opt(path) {
            Ok(Some(now)) => now,
            Ok(None) => return Still::Gone,
            Err(e) => return Still::Changed(format!("{e:#}")),
        };
        if Kind::of(&now) != Kind::of(before) {
            return Still::Changed(format!("it is now {}", describe_kind(&now)));
        }
        if now.size != before.size || now.mtime != before.mtime {
            return Still::Changed(describe_change(&before.fingerprint(), &now));
        }
        if copied.id.is_some() && self.ends.src().identity(path, false).ok().flatten() != copied.id
        {
            return Still::Changed("another file has its name now".into());
        }
        if let Some(target) = &copied.target {
            match self.ends.src().read_link(path) {
                Ok(now) if &now == target => {}
                Ok(now) => return Still::Changed(format!("it now points at {now}, not {target}")),
                Err(e) => return Still::Changed(format!("{e:#}")),
            }
        }
        Still::Same
    }

    /// The removal of a moved file or link once its copy is published and checked.
    fn remove_moved(&mut self, name: &str, from: &str, copied: &Copied) -> ItemOutcome {
        let to = if copied.name == name {
            String::new()
        } else {
            format!(" (copied as {})", copied.name)
        };
        match self.still(from, copied) {
            Still::Same => {}
            // Nothing to remove: the move is done.
            Still::Gone => return named(name, &copied.name),
            Still::Changed(how) => {
                return ItemOutcome::Failed(format!(
                    "{name} changed after it was copied ({how}); it was kept, so both copies \
                     exist{to}"
                ))
            }
        }
        match self.ends.src().remove(from) {
            Ok(()) => named(name, &copied.name),
            Err(e) => ItemOutcome::Failed(format!(
                "{name} could not be removed ({e:#}); both copies exist{to}"
            )),
        }
    }

    /// A folder: made at the target (a name in the way asks), filled from a walk of the source
    /// as it was, its folders given their bits and times after their contents; for a move, the
    /// sources removed bottom-up after everything is copied and checked.
    fn folder(
        &mut self,
        name: &str,
        from: &str,
        dst_dir: &str,
        mut want: String,
        mv: bool,
    ) -> (ItemOutcome, Flow) {
        self.progress(name, 0, 0);
        loop {
            match self.ends.dst().make_dir(&join(dst_dir, &want)) {
                Ok(()) => break,
                Err(e) if is_taken(&e) => match self.resolve(dst_dir, &want, false) {
                    Ok(Resolved::Name(_)) if self.stopped() => {
                        return (stopped("nothing of it was copied"), Flow::Stop)
                    }
                    Ok(Resolved::Name(n)) => want = n,
                    Ok(Resolved::Skip) => return (ItemOutcome::Skipped, Flow::Go),
                    Ok(Resolved::Cancel) => return (ItemOutcome::Skipped, Flow::Cancel),
                    Err(why) => return (ItemOutcome::Failed(why), Flow::Go),
                },
                Err(e) => return (ItemOutcome::Failed(format!("{e:#}")), Flow::Go),
            }
        }
        let root = join(dst_dir, &want);
        let partial = |root: &str| {
            format!("the copy {root} is partial: it holds only what was copied before the end")
        };
        let left = |root: &str| {
            let source = if mv { "; no source was removed" } else { "" };
            format!("the copy {root} is partial{source}")
        };
        let mut nodes = Vec::new();
        if let Err(why) = self.walk(from, "", &mut nodes) {
            // Nothing is in the new folder yet: it goes again, or the report names it.
            if let Err(e) = self.ends.dst().remove_dir(&root) {
                self.report.notes.push(format!(
                    "the new folder {root} is left (empty): it could not be removed: {e:#}"
                ));
            }
            return (ItemOutcome::Failed(why), Flow::Go);
        }
        let mut copied: Vec<Option<Copied>> = Vec::with_capacity(nodes.len());
        for node in &mut nodes {
            if self.stopped() {
                self.report.notes.push(partial(&root));
                return (stopped(&left(&root)), Flow::Stop);
            }
            let source = join(from, &node.rel);
            let shown = format!("{name}/{}", node.rel);
            if node.kind == Kind::Unknown {
                node.kind = match self.ends.src().lstat_opt(&source) {
                    // A folder the walk could not see into stays unknown: skipped, not made
                    // empty.
                    Ok(Some(own)) if Kind::of(&own) != Kind::Dir => Kind::of(&own),
                    Ok(_) => Kind::Unknown,
                    Err(e) => {
                        self.report.notes.push(partial(&root));
                        return (ItemOutcome::Failed(format!("{e:#}")), Flow::Go);
                    }
                };
            }
            let (dir, base) = split_rel(&root, &node.rel);
            let placed = match node.kind {
                Kind::Dir => match self.ends.dst().make_dir(&join(&root, &node.rel)) {
                    Ok(()) => None,
                    Err(e) => Some(Placed::Failed(format!("{e:#}"))),
                },
                Kind::File => Some(self.copy_file(&source, &dir, base.to_string(), &shown, false)),
                Kind::Link => Some(self.copy_link(&source, &dir, base.to_string(), &shown, false)),
                Kind::Special | Kind::NotUtf8 | Kind::Unknown => {
                    let why = match node.kind {
                        Kind::NotUtf8 => "its name is not UTF-8",
                        Kind::Unknown => "it could not be read when its folder was listed",
                        _ => "it is not a file, a folder or a link",
                    };
                    self.report
                        .notes
                        .push(format!("{shown} was skipped: {why}"));
                    self.line(shown, ItemOutcome::Skipped);
                    None
                }
            };
            match placed {
                None => copied.push(None),
                Some(Placed::At(c)) => copied.push(Some(c)),
                Some(Placed::Stopped(_)) => {
                    self.report.notes.push(partial(&root));
                    return (stopped(&left(&root)), Flow::Stop);
                }
                Some(Placed::Failed(why)) => {
                    self.report.notes.push(partial(&root));
                    return (ItemOutcome::Failed(why), Flow::Go);
                }
                Some(Placed::Skipped | Placed::Cancelled) => {
                    unreachable!("a clash below a new folder fails the item instead")
                }
            }
        }
        // Folders get their bits and times after their contents, deepest first.
        let dirs = nodes.iter().rev().filter(|n| n.kind == Kind::Dir);
        let dirs: Vec<String> = dirs.map(|n| n.rel.clone()).chain([String::new()]).collect();
        for rel in dirs {
            let (src, dst) = if rel.is_empty() {
                (from.to_string(), root.clone())
            } else {
                (join(from, &rel), join(&root, &rel))
            };
            match self.ends.src().meta(&src) {
                Ok(meta) => {
                    let notes = self.ends.dst().set_meta(&dst, &meta);
                    self.report.notes.extend(notes);
                }
                Err(e) => self
                    .report
                    .notes
                    .push(format!("the mode and times of {dst} were not set: {e:#}")),
            }
        }
        if !mv {
            return (named(name, &want), Flow::Go);
        }
        self.remove_tree(name, from, &want, &nodes, &copied)
    }

    /// The entries below `top/rel`, folders before their contents, as the listing gives them.
    fn walk(&mut self, top: &str, rel: &str, out: &mut Vec<Node>) -> Result<(), String> {
        let dir = if rel.is_empty() {
            top.to_string()
        } else {
            join(top, rel)
        };
        let entries = self
            .ends
            .src()
            .list_entries(&dir)
            .map_err(|e| format!("{e:#}"))?;
        for e in entries {
            let child = if rel.is_empty() {
                e.name.clone()
            } else {
                format!("{rel}/{}", e.name)
            };
            let kind = if e.lossy {
                Kind::NotUtf8
            } else if e.link {
                Kind::Link
            } else {
                match e.kind {
                    EntryKind::Dir => Kind::Dir,
                    EntryKind::File => Kind::File,
                    EntryKind::Special => Kind::Special,
                    EntryKind::Broken | EntryKind::Unknown => Kind::Unknown,
                }
            };
            out.push(Node {
                rel: child.clone(),
                kind,
            });
            if kind == Kind::Dir {
                self.walk(top, &child, out)?;
            }
        }
        Ok(())
    }

    /// A moved folder's sources, bottom-up: each file and link checked again and removed, each
    /// folder removed once empty. What stays (changed, unremovable, skipped) keeps its folders,
    /// gets a line, and the item says both copies exist.
    fn remove_tree(
        &mut self,
        name: &str,
        from: &str,
        want: &str,
        nodes: &[Node],
        copied: &[Option<Copied>],
    ) -> (ItemOutcome, Flow) {
        let mut blocked: HashSet<String> = HashSet::new();
        let mut kept = 0usize;
        for (node, copied) in nodes.iter().zip(copied).rev() {
            if self.stopped() {
                self.report.notes.push(format!(
                    "stopped while removing the source of {name}: what was not removed yet \
                     exists in both places"
                ));
                return (
                    stopped(
                        "while removing its source: what was not removed yet exists in both places",
                    ),
                    Flow::Stop,
                );
            }
            let path = join(from, &node.rel);
            let stays = match (node.kind, copied) {
                (Kind::Dir, _) if blocked.contains(&node.rel) => Some(None),
                (Kind::Dir, _) => match self.ends.src().remove_dir(&path) {
                    Ok(()) => None,
                    Err(e) => Some(Some(format!("{e:#}"))),
                },
                (Kind::File | Kind::Link, Some(c)) => match self.still(&path, c) {
                    Still::Same => match self.ends.src().remove(&path) {
                        Ok(()) => None,
                        Err(e) => Some(Some(format!("{e:#}"))),
                    },
                    Still::Gone => None,
                    Still::Changed(how) => {
                        Some(Some(format!("changed after it was copied ({how})")))
                    }
                },
                // Skipped entries were never copied: they stay, listed already.
                _ => Some(None),
            };
            let Some(why) = stays else { continue };
            kept += 1;
            for up in ancestors(&node.rel) {
                blocked.insert(up.to_string());
            }
            if let Some(why) = why {
                let line = ItemOutcome::Failed(format!("{why}; both copies exist"));
                self.line(format!("{name}/{}", node.rel), line);
            }
        }
        if !blocked.contains("") {
            if let Err(e) = self.ends.src().remove_dir(from) {
                return (
                    ItemOutcome::Failed(format!(
                        "{name} was copied, but its folder could not be removed ({e:#}); both \
                         copies exist"
                    )),
                    Flow::Go,
                );
            }
        }
        if kept == 0 {
            return (named(name, want), Flow::Go);
        }
        let to = if want == name {
            String::new()
        } else {
            format!(" as {want}")
        };
        (
            ItemOutcome::Failed(format!(
                "{name} was copied{to}, but {kept} of its entries stay where they were (listed \
                 below): what was copied of them exists in both places"
            )),
            Flow::Go,
        )
    }
}

/// A name up to case and Unicode form: NFC of the lower case of the NFC form.
fn name_key(name: &str) -> String {
    let nfc = icu_normalizer::ComposingNormalizerBorrowed::new_nfc();
    nfc.normalize(&nfc.normalize(name).to_lowercase())
        .into_owned()
}

fn describe_kind(s: &Stat) -> String {
    match Kind::of(s) {
        Kind::Dir => "a folder",
        Kind::File => "a file",
        Kind::Link => "a link",
        _ => "not a file, a folder or a link",
    }
    .to_string()
}
