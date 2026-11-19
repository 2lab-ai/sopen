//! The file layer. [`RemoteFs`] has two implementations with the same semantics: [`SftpRemote`]
//! (SFTP over the system `ssh`, spec §4) and [`LocalFs`] (the local disk; used for local targets and
//! by every test). The conflict-safe write algorithm (spec §5) is written once, as provided methods
//! on top of a few primitives, so the tests exercise exactly what runs against a remote.

mod local;
mod sftp;

pub use local::LocalFs;
pub use sftp::{connection_args, SftpRemote};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

/// A replace re-reads the remote and compares its SHA-256 with the fetch's up to this size;
/// above it the check is size + mtime only, and the result says so (spec §5).
pub const HASH_CHECK_MAX: u64 = 256 * 1024 * 1024;

/// Highest `-N` suffix tried for a new sibling.
const MAX_SUFFIX: u32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub is_dir: bool,
    /// A regular file (for `lstat` the entry itself, for `stat` what a link points at).
    pub is_file: bool,
    /// Only `lstat` reports links; `stat` follows them.
    pub is_symlink: bool,
    pub size: u64,
    /// SFTP v3 carries whole seconds; the local disk carries nanoseconds.
    pub mtime: SystemTime,
    /// Permission bits only (`& 0o7777`).
    pub mode: u32,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// The first attribute the server left out (`"size"`, `"mtime"`, `"permissions"`,
    /// `"type"`, `"owner"`); a zero stands in for it. Reads tolerate that; writes refuse.
    pub missing: Option<&'static str>,
    /// SHA-256 of the bytes sopen transferred: set by [`RemoteFs::fetch`] and by a successful
    /// [`RemoteFs::write_atomic`], `None` from a plain stat.
    pub sha256: Option<[u8; 32]>,
}

/// What a write is checked against: the remote `(size, mtime)` and content hash recorded at
/// fetch time. Without a hash (a baseline from a plain stat) only size and mtime are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime: SystemTime,
    pub sha256: Option<[u8; 32]>,
}

impl Stat {
    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            size: self.size,
            mtime: self.mtime,
            sha256: self.sha256,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The target now holds the local content. `now` is the next save's baseline: the target's
    /// stat after the rename, with the size and SHA-256 of the bytes sopen uploaded. `notes` are
    /// things the user must hear (the remote changed right after the save — then the next save
    /// from this baseline is refused; a content check skipped for size).
    Written { now: Stat, notes: Vec<String> },
    /// Nothing was written: the target changed, vanished, or stopped being a regular file since
    /// it was fetched. `reason` says how; `notes` names anything left behind.
    Conflict {
        now: Option<Stat>,
        reason: String,
        notes: Vec<String>,
    },
}

/// What a directory entry is — for a symbolic link, what it points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// A fifo, socket or device: never opened (reading a fifo would block).
    Special,
    /// A symbolic link that points nowhere (or at something this account cannot stat).
    Broken,
    /// The entry itself could not be stat'ed (a folder that may be read but not searched).
    Unknown,
}

/// One directory entry as a listing shows it, from the listing itself: SFTP readdir carries
/// each name's attributes, so a listing costs one round trip, plus one stat per symbolic link
/// (to say what it points at).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name as shown: a name that is not valid UTF-8 has U+FFFD for its bad bytes.
    pub name: String,
    /// The name is not valid UTF-8 (`name` is only how it shows): the file layer takes paths as
    /// UTF-8, so the browser never opens such an entry, and nothing is ever looked up by `name`
    /// — two different names can show alike.
    pub lossy: bool,
    pub kind: EntryKind,
    /// The entry is a symbolic link: `kind`, `size` and `mtime` are its target's (the link's
    /// own when it is broken).
    pub link: bool,
    pub size: u64,
    pub mtime: SystemTime,
}

impl EntryKind {
    fn of(stat: &Stat) -> Self {
        if stat.is_dir {
            EntryKind::Dir
        } else if stat.is_file {
            EntryKind::File
        } else {
            EntryKind::Special
        }
    }
}

/// The entry `name` from its own attributes (`own`, as `lstat` gives them) and, for a symbolic
/// link, its target's (`None`: the link is broken).
fn entry(name: &OsStr, own: &Stat, target: Option<Stat>) -> Entry {
    let (kind, at) = match (own.is_symlink, target) {
        (true, Some(t)) => (EntryKind::of(&t), t),
        (true, None) => (EntryKind::Broken, *own),
        (false, _) => (EntryKind::of(own), *own),
    };
    Entry {
        name: name.to_string_lossy().into_owned(),
        lossy: name.to_str().is_none(),
        kind,
        link: own.is_symlink,
        size: at.size,
        mtime: at.mtime,
    }
}

/// An entry whose own attributes could not be read.
fn unknown_entry(name: &OsStr) -> Entry {
    Entry {
        name: name.to_string_lossy().into_owned(),
        lossy: name.to_str().is_none(),
        kind: EntryKind::Unknown,
        link: false,
        size: 0,
        mtime: std::time::UNIX_EPOCH,
    }
}

/// A directory's names as the listing gave them (their bytes) with the attributes it gave for
/// each (`None`: none).
pub(crate) type Listed = Vec<(OsString, Option<Stat>)>;

/// The entries of a listing, sorted by name. `lookup(name, follow)` stats the entry by its own
/// bytes — never by the name as shown, which two names can share: `follow` = false gives the
/// entry's own attributes, asked only when the listing left them or its type out; `true` what a
/// link points at, asked only for an entry its own attributes say is a link.
pub(crate) fn entries_from(
    listed: Listed,
    lookup: &mut dyn FnMut(&OsStr, bool) -> Result<Option<Stat>>,
) -> Vec<Entry> {
    let untyped = |s: &Stat| !s.is_dir && !s.is_file && !s.is_symlink && s.missing.is_some();
    let mut out = Vec::with_capacity(listed.len());
    for (raw, given) in listed {
        let own = match given {
            Some(own) if !untyped(&own) => own,
            // The listing left the attributes or the type out: the entry's own, by `lstat`.
            _ => match lookup(&raw, false) {
                Ok(Some(own)) if !untyped(&own) => own,
                // Gone since the listing.
                Ok(None) => continue,
                Ok(Some(_)) | Err(_) => {
                    out.push(unknown_entry(&raw));
                    continue;
                }
            },
        };
        let target = if own.is_symlink {
            lookup(&raw, true).ok().flatten()
        } else {
            None
        };
        out.push(entry(&raw, &own, target));
    }
    // By name as shown; of two names that show alike, the one that is valid UTF-8 first.
    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.lossy.cmp(&b.lossy)));
    out
}

/// What a temp standing in for an existing file takes from it: its owner and permission bits,
/// given through the temp's open handle by [`RemoteFs::create_from`] before any content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Like {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

/// What [`RemoteFs::create_from`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uploaded {
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Whether a failed [`RemoteFs::create_from`] had created its path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    /// The exclusive create failed: the path is not ours (it may be someone else's file).
    No,
    /// The path is ours and may hold partial content; the caller removes it.
    Yes,
    /// The create request timed out: the path may or may not exist now.
    Unknown,
}

#[derive(Debug)]
pub struct CreateError {
    pub created: Created,
    pub error: anyhow::Error,
}

impl CreateError {
    pub fn new(created: Created, error: anyhow::Error) -> Self {
        Self { created, error }
    }
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

impl std::error::Error for CreateError {}

/// A request that changes the remote was sent and no status reply came back (timeout, connection
/// closed, broken pipe, a garbled reply): it may or may not have taken effect. Only an SFTP status
/// reply proves a request did not happen. Never report "nothing was written" for it.
#[derive(Debug)]
pub struct OutcomeUnknown(pub String);

impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OutcomeUnknown {}

pub fn is_outcome_unknown(e: &anyhow::Error) -> bool {
    outcome_unknown(e).is_some()
}

/// The outermost [`OutcomeUnknown`] in `e` (as a context layer or in the chain).
pub fn outcome_unknown(e: &anyhow::Error) -> Option<&OutcomeUnknown> {
    e.downcast_ref::<OutcomeUnknown>()
        .or_else(|| e.chain().find_map(|c| c.downcast_ref::<OutcomeUnknown>()))
}

/// `e` under an [`OutcomeUnknown`] that reads "<what>: outcome unknown — check <path>".
fn unknown_at(e: anyhow::Error, what: &str, path: &str) -> anyhow::Error {
    e.context(OutcomeUnknown(format!(
        "{what}: outcome unknown — check {path}"
    )))
}

/// An [`OutcomeUnknown`] error: a request that changes the remote got no reply in time.
pub fn unknown(what: impl Into<String>) -> anyhow::Error {
    anyhow!(OutcomeUnknown(what.into()))
}

/// A fetch its caller stopped ([`RemoteFs::fetch_cancellable`]); nothing was kept of it.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Whether `e` is a fetch its caller stopped.
pub fn is_cancelled(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Cancelled>())
}

/// Writes through to `inner` until `stop` is set; from then on every write fails, which ends
/// the transfer feeding it. (Not `ErrorKind::Interrupted`: `write_all` retries that.)
struct Stoppable<'a, W> {
    inner: W,
    stop: &'a AtomicBool,
}

impl<W: Write> Write for Stoppable<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("cancelled"));
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Counts and hashes what passes through to `inner`.
pub struct Hashing<W> {
    inner: W,
    hasher: Sha256,
    len: u64,
}

impl<W: Write> Hashing<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            len: 0,
        }
    }

    pub fn finish(self) -> Uploaded {
        Uploaded {
            size: self.len,
            sha256: self.hasher.finalize().into(),
        }
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// File operations on one host. Paths are absolute (or home-relative for SFTP) unix paths.
pub trait RemoteFs {
    /// `stat` following symlinks; `Ok(None)` when the path does not exist.
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>>;
    /// `lstat`: like [`RemoteFs::stat_opt`] but a final symlink is reported as itself.
    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>>;
    /// Absolute, symlink-resolved path. Relative paths resolve against the remote home (SFTP) or
    /// the working directory (local); `~` and `~/…` expand to the home.
    fn realpath(&mut self, path: &str) -> Result<String>;
    /// Up to `max` bytes from the start of a file (for the text sniff).
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>>;
    /// Entry names of a directory, sorted (detection only — sopen never syncs directories).
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>>;
    /// The entries of a directory with their kind, size and modification time, sorted by name
    /// (the file browser's listing), from the listing's own attributes ([`entries_from`]): one
    /// round trip over SFTP, plus a stat per link.
    fn list_entries(&mut self, path: &str) -> Result<Vec<Entry>>;
    /// Bytes free for this account on the filesystem holding `path`, when that costs one
    /// request (`None` otherwise, or when it fails).
    fn free_space(&mut self, _path: &str) -> Option<u64> {
        None
    }
    /// Streams a file's content into `out`; `progress` gets the running byte count.
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64>;
    /// Creates `path` exclusively (fails if anything exists there), writes the local file into
    /// it and flushes it to disk (`fsync@openssh.com` when the server offers it). With `like`,
    /// through handle-based metadata updates on the open file, never by path (so a name swapped
    /// meanwhile is never changed through): that owner (where the server lets it; the caller
    /// checks) and the permission bits without set-id bits before the content, then the full
    /// mode restored after the content. Returns the size and SHA-256 of the
    /// bytes written; a failure says whether `path` had been created ([`CreateError::created`]).
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError>;
    /// Sets `path`'s permission bits by path (setstat). The write algorithms never use it —
    /// they set modes through the open handle; it is for setting up tests and tools.
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()>;
    /// Renames `from` over `to`, replacing it (`posix-rename@openssh.com` when offered).
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()>;
    /// Publishes `from` as `to` without replacing anything (a hard link; fails if `to` exists).
    fn link_new(&mut self, from: &str, to: &str) -> Result<()>;
    /// Whether [`RemoteFs::link_new`] can work here at all (SFTP: `hardlink@openssh.com`).
    fn can_link(&self) -> bool {
        true
    }
    /// Whether [`RemoteFs::rename_over`] can replace an existing file (SFTP:
    /// `posix-rename@openssh.com`; plain SFTP rename refuses an existing target).
    fn can_replace(&self) -> bool {
        true
    }
    fn remove(&mut self, path: &str) -> Result<()>;
    /// Largest target whose content is re-read and hashed before a replace (tests lower it).
    fn hash_check_limit(&self) -> u64 {
        HASH_CHECK_MAX
    }

    fn stat(&mut self, path: &str) -> Result<Stat> {
        self.stat_opt(path)?
            .with_context(|| format!("{path}: no such file or directory"))
    }

    /// Downloads `remote` to `local` (written via a `.part` file, then renamed) and returns the
    /// stat it was fetched at, with the SHA-256 of the fetched bytes. The stat is taken before
    /// and after the transfer; if they differ the file changed mid-transfer and the fetch is
    /// retried once, then refused. Whatever is at `local` is replaced: the caller owns it (a
    /// [`crate::cache::Claim`] moves unsaved edits aside first and records what was fetched).
    fn fetch(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Stat> {
        self.fetch_cancellable(remote, local, progress, &AtomicBool::new(false))
    }

    /// [`RemoteFs::fetch`] that stops when `cancel` is set — checked before each attempt, at
    /// every write into the copy, and once more before the complete copy is published (a cancel
    /// after the last write, or of a file with no bytes): the partial copy is removed, `local`
    /// is left as it was, and the error is [`Cancelled`].
    fn fetch_cancellable(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<Stat> {
        // The copy is the owner's only: 0700 directories, a 0600 file.
        if let Some(parent) = local.parent() {
            crate::cache::private_dir(parent)?;
        }
        let part = local.with_file_name(format!(
            "{}.part-{}",
            local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            random_hex()
        ));
        let stopped = || cancel.load(Ordering::Relaxed);
        for _ in 0..2 {
            if stopped() {
                return Err(anyhow!(Cancelled));
            }
            let before = self.stat(remote)?;
            if before.is_dir {
                bail!("{remote} is a directory");
            }
            let result = (|| -> Result<Uploaded> {
                let file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&part)
                    .with_context(|| format!("cannot create {}", part.display()))?;
                let mut out = Hashing::new(Stoppable {
                    inner: std::io::BufWriter::with_capacity(1 << 20, file),
                    stop: cancel,
                });
                let mut cb = |done: u64| progress(done, before.size);
                self.read_to(remote, &mut out, &mut cb)?;
                out.flush()?;
                Ok(out.finish())
            })();
            let got = match result {
                Ok(got) if !stopped() => got,
                outcome => {
                    let _ = std::fs::remove_file(&part);
                    return match outcome {
                        Err(e) if !stopped() => Err(e),
                        _ => Err(anyhow!(Cancelled)),
                    };
                }
            };
            let after = self.stat_opt(remote)?;
            let whole = before.missing == Some("size") || got.size == before.size;
            if stopped() {
                // Cancelled during the check: the copy is complete, but not wanted.
                let _ = std::fs::remove_file(&part);
                return Err(anyhow!(Cancelled));
            }
            if after.is_some_and(|a| same_stat(&a, &before)) && whole {
                std::fs::rename(&part, local)
                    .with_context(|| format!("cannot move into {}", local.display()))?;
                return Ok(Stat {
                    sha256: Some(got.sha256),
                    ..before
                });
            }
            let _ = std::fs::remove_file(&part);
        }
        bail!("{remote} kept changing while it was fetched; try again when it is quiet")
    }

    /// Replaces `target` with the local file — only if it is still the regular file that was
    /// fetched: same `(size, mtime)`, and (up to [`HASH_CHECK_MAX`], when `expected` has a hash)
    /// the same content. Upload goes to `.<name>.sopen-<random>` in the same directory, created
    /// exclusively with the target's permission bits and given the target's owner; the target is
    /// re-checked, then the temp is renamed over it. A temp this call created is removed on every
    /// failure, or named in the error/notes when it cannot be. The returned baseline is the
    /// staged temp as stat'ed before the rename, never the target's stat after it.
    fn write_atomic(
        &mut self,
        local: &Path,
        target: &str,
        expected: Fingerprint,
    ) -> Result<WriteOutcome> {
        if !self.can_replace() {
            bail!(
                "this server cannot replace a file in one step (it does not offer \
                 posix-rename@openssh.com), so {target} was not touched; use Save copy to write \
                 the edit next to it instead"
            );
        }
        let mut notes = Vec::new();
        let current = match check_target(self, target, &expected)? {
            Check::Same(s) => s,
            Check::Changed(now, reason) => {
                return Ok(WriteOutcome::Conflict { now, reason, notes });
            }
        };
        let mode = current.mode & 0o7777;
        let (Some(uid), Some(gid)) = (current.uid, current.gid) else {
            unreachable!("check_target refuses a target without an owner")
        };
        let (dir, name) = split_parent(target);
        let tmp = join(dir, &format!(".{name}.sopen-{}", random_hex()));
        let like = Like { mode, uid, gid };
        let uploaded = match self.create_from(local, &tmp, Some(like)) {
            Ok(u) => u,
            Err(e) if e.created == Created::Unknown => {
                let what = format!("{target} was not replaced, but creating {tmp} got no reply");
                return Err(unknown_at(e.error, &what, &tmp));
            }
            Err(e) => {
                let e = abandon(self, &tmp, e);
                return Err(e.context(format!("{target} was not replaced")));
            }
        };
        // The temp is ours from here: every exit removes it or names it.
        let swap = prepare_swap(
            self, target, &tmp, &current, &expected, &uploaded, mode, &mut notes,
        );
        let staged = match swap {
            Ok(Swap::Ready(staged)) => staged,
            Ok(Swap::Changed(now, reason)) => {
                if let Err(e) = self.remove(&tmp) {
                    notes.push(format!("the temp file {tmp} could not be removed: {e:#}"));
                }
                return Ok(WriteOutcome::Conflict { now, reason, notes });
            }
            Err(e) => {
                let e = e.context(format!("{target} was not replaced"));
                return Err(remove_or_name(self, &tmp, e));
            }
        };
        if let Err(e) = self.rename_over(&tmp, target) {
            if is_outcome_unknown(&e) {
                // It may have landed: the temp is not removed, and both paths are named.
                let what = format!("the rename of {tmp} over {target} got no reply");
                return Err(unknown_at(e, &what, target));
            }
            let e = e.context(format!(
                "cannot rename {tmp} over {target}; {target} was not replaced"
            ));
            return Err(remove_or_name(self, &tmp, e));
        }
        // Replaced. Is the target still the object sopen staged?
        let after = self
            .lstat_opt(target)
            .with_context(|| format!("replaced {target} but cannot stat it"))?;
        match after {
            Some(now) if now.is_file && !now.is_symlink && same_stat(&now, &staged) => {
                // The staged temp had the mode (checked right before the rename); a rename does
                // not change it, so a difference is someone else's chmod. Said, never undone by
                // path.
                if now.mode & 0o7777 != mode {
                    notes.push(format!(
                        "saved, but {target} now has mode {:o}, not {mode:o} (changed right \
                         after the save?)",
                        now.mode & 0o7777
                    ));
                }
            }
            other => notes.push(format!(
                "saved, but the remote changed right after: {target} is {} — sopen wrote {} \
                 (another writer?); the next save from this copy will be refused",
                other.map_or("gone".to_string(), |s| describe_stat(&s)),
                describe_stat(&staged)
            )),
        }
        // The baseline is what sopen staged; if the remote holds anything else, the next save
        // sees a different (size, mtime) and refuses.
        let now = Stat {
            mode,
            sha256: Some(uploaded.sha256),
            ..staged
        };
        Ok(WriteOutcome::Written { now, notes })
    }

    /// Uploads the local file into `dir` as `wanted`, or `<stem>-N.<ext>` when that name is
    /// taken; never replaces an existing file. Returns the final name. Publishing is a hard
    /// link from a temp (the file appears complete); where links are unavailable the file is
    /// written straight into a name created exclusively, so nothing is replaced either way.
    fn write_new_sibling(&mut self, local: &Path, dir: &str, wanted: &str) -> Result<String> {
        if wanted.is_empty() || wanted.contains('/') || wanted == "." || wanted == ".." {
            bail!("invalid file name '{wanted}'");
        }
        if !self.can_link() {
            return publish_direct(self, local, dir, wanted, 0);
        }
        let tmp = join(dir, &format!(".{wanted}.sopen-{}", random_hex()));
        if let Err(e) = self.create_from(local, &tmp, None) {
            return Err(abandon(self, &tmp, e));
        }
        let linked = publish_linked(self, &tmp, dir, wanted);
        let removed = self.remove(&tmp);
        match (linked, removed) {
            (Ok(Linked::As(name)), Ok(())) => Ok(name),
            (Ok(Linked::As(name)), Err(re)) => bail!(
                "published {}, but the temp file {tmp} could not be removed: {re:#}",
                join(dir, &name)
            ),
            (Ok(Linked::Unsupported(n)), Ok(())) => publish_direct(self, local, dir, wanted, n),
            (Ok(Linked::Unsupported(_)), Err(re)) => bail!(
                "nothing published: hard links failed in {dir} and the temp file {tmp} could \
                 not be removed: {re:#}"
            ),
            (Err(e), Ok(())) => Err(e),
            (Err(e), Err(re)) => {
                Err(e.context(format!("the temp file {tmp} could not be removed ({re:#})")))
            }
        }
    }
}

fn same_stat(a: &Stat, b: &Stat) -> bool {
    a.size == b.size && a.mtime == b.mtime
}

fn describe_stat(s: &Stat) -> String {
    let kind = if s.is_symlink {
        "a symbolic link, "
    } else if !s.is_file {
        "not a regular file, "
    } else {
        ""
    };
    format!(
        "{kind}{} bytes modified {}",
        s.size,
        crate::cache::rfc3339(s.mtime)
    )
}

/// How `now` differs from the fetch baseline, for a conflict message.
pub fn describe_change(at: &Fingerprint, now: &Stat) -> String {
    let mut parts = Vec::new();
    if now.size != at.size {
        parts.push(format!("size {} → {}", at.size, now.size));
    }
    if now.mtime != at.mtime {
        parts.push(format!(
            "modified {} → {}",
            crate::cache::rfc3339(at.mtime),
            crate::cache::rfc3339(now.mtime)
        ));
    }
    if parts.is_empty() {
        "its content changed (same size and modification time)".to_string()
    } else {
        parts.join(", ")
    }
}

enum Check {
    Same(Stat),
    Changed(Option<Stat>, String),
}

/// `lstat` of the target against the baseline: gone, a symlink, not a regular file, or a
/// different `(size, mtime)` is a change. Attributes the server leaves out refuse the write.
fn check_target<F: RemoteFs + ?Sized>(
    fs: &mut F,
    target: &str,
    expected: &Fingerprint,
) -> Result<Check> {
    let Some(now) = fs.lstat_opt(target)? else {
        return Ok(Check::Changed(None, "it was deleted".to_string()));
    };
    if let Some(attr) = now.missing {
        bail!(
            "the server does not report the {attr} of {target}; sopen cannot check it for \
             changes, so it will not write it"
        );
    }
    let changed = |reason: &str| Ok(Check::Changed(Some(now), reason.to_string()));
    if now.is_symlink {
        return changed("it is now a symbolic link");
    }
    if now.is_dir {
        return changed("it is now a directory");
    }
    if !now.is_file {
        return changed("it is no longer a regular file");
    }
    if now.size != expected.size || now.mtime != expected.mtime {
        return Ok(Check::Changed(Some(now), describe_change(expected, &now)));
    }
    Ok(Check::Same(now))
}

enum Swap {
    /// The temp as it will be renamed: its `(size, mtime)` is the next baseline.
    Ready(Stat),
    Changed(Option<Stat>, String),
}

/// With the temp uploaded: stat it (the next baseline) and check it took the target's owner
/// and mode, compare the target's content with the fetch, and re-check the target's stat last.
#[allow(clippy::too_many_arguments)]
fn prepare_swap<F: RemoteFs + ?Sized>(
    fs: &mut F,
    target: &str,
    tmp: &str,
    current: &Stat,
    expected: &Fingerprint,
    uploaded: &Uploaded,
    mode: u32,
    notes: &mut Vec<String>,
) -> Result<Swap> {
    // The temp as it will be renamed: what the target will be right after the rename. Owner
    // and mode were given through its handle (create_from); here they are only checked, never
    // set by path — the name may no longer be the file just written.
    let staged = fs
        .lstat_opt(tmp)?
        .with_context(|| format!("{tmp} vanished right after it was uploaded"))?;
    if let Some(attr) = staged.missing {
        bail!("the server does not report the {attr} of {tmp}; cannot match the target's owner");
    }
    if !staged.is_file || staged.size != uploaded.size {
        bail!(
            "{tmp} is not the {} bytes just uploaded ({})",
            uploaded.size,
            describe_stat(&staged)
        );
    }
    let want = (current.uid, current.gid);
    let staged_owner = (staged.uid, staged.gid);
    if staged_owner != want {
        bail!(
            "{target} is owned by {} and a file this account creates there is owned by {}; \
             the replacement cannot keep the original owner",
            owner(want),
            owner(staged_owner)
        );
    }
    if staged.mode & 0o7777 != mode {
        bail!(
            "{tmp} did not take mode {mode:o} ({}); the replacement cannot keep the original \
             permissions",
            describe_stat(&staged)
        );
    }
    if let Some(want) = expected.sha256 {
        if current.size <= fs.hash_check_limit() {
            let mut sink = Hashing::new(std::io::sink());
            fs.read_to(target, &mut sink, &mut |_| {})
                .with_context(|| format!("cannot re-read {target} to check it for changes"))?;
            if sink.finish().sha256 != want {
                return Ok(Swap::Changed(
                    Some(*current),
                    "its content changed (same size and modification time)".to_string(),
                ));
            }
        } else {
            notes.push(format!(
                "{target} is over 256 MiB: checked its size and modification time, not its content"
            ));
        }
    }
    match check_target(fs, target, expected)? {
        Check::Same(_) => Ok(Swap::Ready(staged)),
        Check::Changed(now, reason) => Ok(Swap::Changed(now, reason)),
    }
}

fn owner((uid, gid): (Option<u32>, Option<u32>)) -> String {
    let id = |v: Option<u32>| v.map_or("?".to_string(), |v| v.to_string());
    format!("uid {} gid {}", id(uid), id(gid))
}

/// After a failed exclusive create: remove `path` only if this operation created it.
fn abandon<F: RemoteFs + ?Sized>(fs: &mut F, path: &str, e: CreateError) -> anyhow::Error {
    match e.created {
        Created::No => e.error,
        Created::Yes => remove_or_name(fs, path, e.error),
        Created::Unknown => unknown_at(e.error, &format!("creating {path} got no reply"), path),
    }
}

/// Removes this operation's own `path`; when that fails too, the error says it is left.
fn remove_or_name<F: RemoteFs + ?Sized>(fs: &mut F, path: &str, e: anyhow::Error) -> anyhow::Error {
    match fs.remove(path) {
        Ok(()) => e,
        Err(re) => e.context(format!("{path} is left behind ({re:#})")),
    }
}

fn candidate(wanted: &str, n: u32) -> String {
    if n == 0 {
        wanted.to_string()
    } else {
        suffixed(wanted, n)
    }
}

enum Linked {
    As(String),
    /// Links fail here for a reason other than "exists"; continue without them from this `-N`.
    Unsupported(u32),
}

fn publish_linked<F: RemoteFs + ?Sized>(
    fs: &mut F,
    tmp: &str,
    dir: &str,
    wanted: &str,
) -> Result<Linked> {
    for n in 0..MAX_SUFFIX {
        let name = candidate(wanted, n);
        let path = join(dir, &name);
        if fs.lstat_opt(&path)?.is_some() {
            continue;
        }
        match fs.link_new(tmp, &path) {
            Ok(()) => return Ok(Linked::As(name)),
            Err(e) if is_outcome_unknown(&e) => {
                let what = format!("linking {tmp} to {path} got no reply");
                return Err(unknown_at(e, &what, &path));
            }
            Err(_) if fs.lstat_opt(&path)?.is_some() => continue,
            Err(e) => {
                crate::debug!("link {tmp} -> {path} failed ({e:#}); writing names directly");
                return Ok(Linked::Unsupported(n));
            }
        }
    }
    bail!("no free name for {wanted} in {dir}")
}

/// No links: write the content into each candidate name created exclusively; "exists" moves on.
fn publish_direct<F: RemoteFs + ?Sized>(
    fs: &mut F,
    local: &Path,
    dir: &str,
    wanted: &str,
    from: u32,
) -> Result<String> {
    for n in from..MAX_SUFFIX {
        let name = candidate(wanted, n);
        let path = join(dir, &name);
        if fs.lstat_opt(&path)?.is_some() {
            continue;
        }
        match fs.create_from(local, &path, None) {
            Ok(_) => return Ok(name),
            Err(e) if e.created == Created::No && fs.lstat_opt(&path)?.is_some() => continue,
            Err(e) => return Err(abandon(fs, &path, e)),
        }
    }
    bail!("no free name for {wanted} in {dir}")
}

/// `("/a/b", "c.txt")` for `/a/b/c.txt`; `("/", "c")` for `/c`; `(".", "c")` for `c`.
pub fn split_parent(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => ("/", &trimmed[1..]),
        Some(i) => (&trimmed[..i], &trimmed[i + 1..]),
        None => (".", trimmed),
    }
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// `t.cut-1-3.mp3` → `t.cut-1-3-2.mp3` for n = 2; names without an extension get `-N` appended.
pub fn suffixed(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}-{n}{}", &name[..i], &name[i..]),
        _ => format!("{name}-{n}"),
    }
}

/// 16 hex chars, unique per call (per-process random keys + time + counter).
pub fn random_hex() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests;
