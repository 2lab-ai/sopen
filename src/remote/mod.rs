//! The file layer. [`RemoteFs`] has two implementations with the same semantics: [`SftpRemote`]
//! (SFTP over the system `ssh`, spec §4) and [`LocalFs`] (the local disk; used for local targets and
//! by every test). The conflict-safe write algorithm (spec §5) is written once, as provided methods
//! on top of a few primitives, so the tests exercise exactly what runs against a remote.

mod local;
mod sftp;

pub use local::LocalFs;
pub use sftp::SftpRemote;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::Path;
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

/// A request that changes the remote timed out after it was sent: it may or may not have taken
/// effect. Never report "nothing was written" for it.
#[derive(Debug)]
pub struct OutcomeUnknown(pub String);

impl std::fmt::Display for OutcomeUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OutcomeUnknown {}

pub fn is_outcome_unknown(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<OutcomeUnknown>())
}

/// An [`OutcomeUnknown`] error: a request that changes the remote got no reply in time.
pub fn unknown(what: impl Into<String>) -> anyhow::Error {
    anyhow!(OutcomeUnknown(what.into()))
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
    /// Streams a file's content into `out`; `progress` gets the running byte count.
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64>;
    /// Creates `path` exclusively (fails if anything exists there), sets `mode` first when
    /// given, writes the local file into it and flushes it to disk (`fsync@openssh.com` when
    /// the server offers it). Returns the size and SHA-256 of the bytes written; a failure says
    /// whether `path` had been created ([`CreateError::created`]).
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        mode: Option<u32>,
    ) -> std::result::Result<Uploaded, CreateError>;
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()>;
    fn set_owner(&mut self, path: &str, uid: u32, gid: u32) -> Result<()>;
    /// Renames `from` over `to`, replacing it (`posix-rename@openssh.com` when offered).
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()>;
    /// Publishes `from` as `to` without replacing anything (a hard link; fails if `to` exists).
    fn link_new(&mut self, from: &str, to: &str) -> Result<()>;
    /// Whether [`RemoteFs::link_new`] can work here at all (SFTP: `hardlink@openssh.com`).
    fn can_link(&self) -> bool {
        true
    }
    fn remove(&mut self, path: &str) -> Result<()>;

    fn stat(&mut self, path: &str) -> Result<Stat> {
        self.stat_opt(path)?
            .with_context(|| format!("{path}: no such file or directory"))
    }

    /// Downloads `remote` to `local` (written via a `.part` file, then renamed) and returns the
    /// stat it was fetched at, with the SHA-256 of the fetched bytes. The stat is taken before
    /// and after the transfer; if they differ the file changed mid-transfer and the fetch is
    /// retried once, then refused. A previous copy at `local` that differs from what sopen last
    /// fetched or saved there (unsaved edits) is moved aside first and its new path printed.
    fn fetch(
        &mut self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Stat> {
        if let Some(parent) = local.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }
        if let Some(kept) = crate::cache::set_aside_unsaved(local)? {
            eprintln!(
                "sopen: the previous copy {} had unsaved edits; kept them at {}",
                local.display(),
                kept.display()
            );
        }
        let part = local.with_file_name(format!(
            "{}.part-{}",
            local
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            random_hex()
        ));
        for _ in 0..2 {
            let before = self.stat(remote)?;
            if before.is_dir {
                bail!("{remote} is a directory");
            }
            let result = (|| -> Result<Uploaded> {
                let file = std::fs::File::create(&part)
                    .with_context(|| format!("cannot create {}", part.display()))?;
                let mut out = Hashing::new(std::io::BufWriter::with_capacity(1 << 20, file));
                let mut cb = |done: u64| progress(done, before.size);
                self.read_to(remote, &mut out, &mut cb)?;
                out.flush()?;
                Ok(out.finish())
            })();
            let got = match result {
                Ok(got) => got,
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    return Err(e);
                }
            };
            let after = self.stat_opt(remote)?;
            let whole = before.missing == Some("size") || got.size == before.size;
            if after.is_some_and(|a| same_stat(&a, &before)) && whole {
                std::fs::rename(&part, local)
                    .with_context(|| format!("cannot move into {}", local.display()))?;
                if let Err(e) = crate::cache::write_baseline(local, &got.sha256) {
                    crate::debug!("baseline not recorded: {e:#}");
                }
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
    /// failure, or named in the error/notes when it cannot be.
    fn write_atomic(
        &mut self,
        local: &Path,
        target: &str,
        expected: Fingerprint,
    ) -> Result<WriteOutcome> {
        let mut notes = Vec::new();
        let current = match check_target(self, target, &expected)? {
            Check::Same(s) => s,
            Check::Changed(now, reason) => {
                return Ok(WriteOutcome::Conflict { now, reason, notes });
            }
        };
        let mode = current.mode & 0o7777;
        let (dir, name) = split_parent(target);
        let tmp = join(dir, &format!(".{name}.sopen-{}", random_hex()));
        let uploaded = match self.create_from(local, &tmp, Some(mode)) {
            Ok(u) => u,
            Err(e) => {
                let e = abandon(self, &tmp, e);
                return Err(e.context(format!("{target} was not replaced")));
            }
        };
        // The temp is ours from here: every exit removes it or names it.
        let staged = prepare_swap(self, target, &tmp, &current, &expected, mode, &mut notes);
        match staged {
            Ok(None) => {}
            Ok(Some((now, reason))) => {
                if let Err(e) = self.remove(&tmp) {
                    notes.push(format!("the temp file {tmp} could not be removed: {e:#}"));
                }
                return Ok(WriteOutcome::Conflict { now, reason, notes });
            }
            Err(e) => {
                let e = e.context(format!("{target} was not replaced"));
                return Err(remove_or_name(self, &tmp, e));
            }
        }
        if let Err(e) = self.rename_over(&tmp, target) {
            if is_outcome_unknown(&e) {
                return Err(e.context(format!(
                    "saving {target}: outcome unknown — check the remote ({tmp} may be left)"
                )));
            }
            let e = e.context(format!(
                "cannot rename {tmp} over {target}; {target} was not replaced"
            ));
            return Err(remove_or_name(self, &tmp, e));
        }
        let mut now = self
            .stat(target)
            .with_context(|| format!("replaced {target} but cannot stat it"))?;
        if now.mode & 0o7777 != mode {
            self.set_mode(target, mode)
                .with_context(|| format!("wrote {target} but could not restore mode {mode:o}"))?;
            now = self.stat(target)?;
        }
        if now.size != uploaded.size {
            notes.push(format!(
                "saved, but {target} changed right after ({} bytes written, {} there now; \
                 another writer?) — the next save from this copy will be refused",
                uploaded.size, now.size
            ));
        }
        // The baseline describes what sopen wrote: if the remote does not hold exactly that,
        // the next save sees a size mismatch and refuses.
        now.size = uploaded.size;
        now.sha256 = Some(uploaded.sha256);
        if crate::cache::baseline_record_path(local).exists() {
            if let Err(e) = crate::cache::write_baseline(local, &uploaded.sha256) {
                crate::debug!("baseline not recorded: {e:#}");
            }
        }
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

/// With the temp uploaded: give it the target's owner and mode, compare the target's content
/// with the fetch, and re-check its stat last. `Ok(Some(..))` = the target changed.
fn prepare_swap<F: RemoteFs + ?Sized>(
    fs: &mut F,
    target: &str,
    tmp: &str,
    current: &Stat,
    expected: &Fingerprint,
    mode: u32,
    notes: &mut Vec<String>,
) -> Result<Option<(Option<Stat>, String)>> {
    let staged = fs
        .lstat_opt(tmp)?
        .with_context(|| format!("{tmp} vanished right after it was uploaded"))?;
    if let Some(attr) = staged.missing {
        bail!("the server does not report the {attr} of {tmp}; cannot match the target's owner");
    }
    let want = (current.uid, current.gid);
    let mut staged_owner = (staged.uid, staged.gid);
    if staged_owner != want {
        let (Some(uid), Some(gid)) = want else {
            unreachable!("check_target refuses a target without an owner")
        };
        match fs.set_owner(tmp, uid, gid) {
            Ok(()) => {
                let again = fs
                    .lstat_opt(tmp)?
                    .with_context(|| format!("{tmp} vanished"))?;
                staged_owner = (again.uid, again.gid);
            }
            Err(e) => crate::debug!("set_owner {tmp}: {e:#}"),
        }
        if staged_owner != want {
            bail!(
                "{target} is owned by {} and a file this account creates there is owned by {}; \
                 the replacement cannot keep the original owner",
                owner(want),
                owner(staged_owner)
            );
        }
    }
    // A change of owner can clear setuid/setgid bits.
    let staged = fs
        .lstat_opt(tmp)?
        .with_context(|| format!("{tmp} vanished"))?;
    if staged.mode & 0o7777 != mode {
        fs.set_mode(tmp, mode)?;
    }
    if let Some(want) = expected.sha256 {
        if current.size <= HASH_CHECK_MAX {
            let mut sink = Hashing::new(std::io::sink());
            fs.read_to(target, &mut sink, &mut |_| {})
                .with_context(|| format!("cannot re-read {target} to check it for changes"))?;
            if sink.finish().sha256 != want {
                return Ok(Some((
                    Some(*current),
                    "its content changed (same size and modification time)".to_string(),
                )));
            }
        } else {
            notes.push(format!(
                "{target} is over 256 MiB: checked its size and modification time, not its content"
            ));
        }
    }
    match check_target(fs, target, expected)? {
        Check::Same(_) => Ok(None),
        Check::Changed(now, reason) => Ok(Some((now, reason))),
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
        Created::Unknown => e
            .error
            .context(format!("{path} may have been created — check the remote")),
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
                return Err(e.context(format!("publishing {path}: outcome unknown — check it")));
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
