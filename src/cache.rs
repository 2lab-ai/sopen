//! Local paths: the fetch cache `<cache>/<host>/<abs path>`, edits kept aside as
//! `<copy>.conflict-<UTC stamp>`, image and hex backups
//! `<cache>/.backup/<host>/<abs path>.<UTC stamp>`, image and hex edits under `<cache>/.edits`,
//! exports under `<cache>/.exports`, session copies under `<cache>/.sessions` (spec §5), and the
//! state dir (ssh control sockets, `usage.jsonl`). What
//! sopen knows about a copy lives apart from the mirrored tree, under `<cache>/.meta` (see
//! [`Claim`]): the record of what the copy held when fetched or saved, and the session lock. `SOPEN_CACHE_DIR` /
//! `SOPEN_STATE_DIR` override both. What sopen creates under the cache is the owner's only
//! (0700 directories, 0600 files).

use anyhow::{Context, Result};
use std::fs::{DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct Dirs {
    pub cache: PathBuf,
    pub state: PathBuf,
}

/// Host label used for local targets in cache paths.
pub const LOCAL_HOST: &str = "local";

pub fn dirs() -> Result<Dirs> {
    let project = directories::ProjectDirs::from("ai", "2lab", "sopen");
    let cache = match std::env::var_os("SOPEN_CACHE_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => project
            .as_ref()
            .map(|p| p.cache_dir().to_path_buf())
            .context("no home directory to put the cache in (set SOPEN_CACHE_DIR)")?,
    };
    let state = match std::env::var_os("SOPEN_STATE_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        // Linux: $XDG_STATE_HOME/sopen. macOS has no state dir in `directories`, and its
        // Application Support path is too long for an ssh control socket (104-byte sun_path),
        // so both use the XDG layout (~/.local/state/sopen) — as the openssh crate does.
        _ => project
            .as_ref()
            .and_then(|p| p.state_dir().map(Path::to_path_buf))
            .or_else(|| {
                directories::BaseDirs::new().map(|b| b.home_dir().join(".local/state/sopen"))
            })
            .context("no home directory to put state in (set SOPEN_STATE_DIR)")?,
    };
    Ok(Dirs { cache, state })
}

/// `<root>/<host>/<abs path>` with the path's `/`-components kept and anything that could climb
/// out of `<root>` (`..`, `.`) dropped.
fn under(root: &Path, host: &str, abs: &str) -> PathBuf {
    let mut out = root.join(sanitize(host));
    for comp in Path::new(abs).components() {
        if let Component::Normal(part) = comp {
            out.push(part);
        }
    }
    out
}

/// A host label as one path component. Never empty and never starting with `.`: so no
/// mirrored path is `.`/`..`, and none reaches [`META`].
fn sanitize(host: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    if cleaned.is_empty() || cleaned.starts_with('.') {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// `<cache>/.meta`: what sopen knows about its cached copies — `record/<mirrored path>` (the
/// SHA-256 a copy held when sopen last fetched or saved it) and `lock/<mirrored path>` (the
/// session lock). No mirrored path reaches it (every one starts with a [`sanitize`]d host
/// label), and each kind is a tree of its own shaped like the mirror, so a remote file can
/// never be named onto a record or a lock.
const META: &str = ".meta";

/// Where sopen keeps `kind` (`record`, `lock`) for the cached copy `local` (under `cache`).
fn meta_path(cache: &Path, kind: &str, local: &Path) -> Result<PathBuf> {
    let rel = local
        .strip_prefix(cache)
        .with_context(|| format!("{} is not under the cache", local.display()))?;
    Ok(cache.join(META).join(kind).join(rel))
}

/// The cache's fixed roots, beside the mirrored hosts: image and hex backups, staged and kept
/// image and hex edits, local exports. Dot names: no host label reaches them (see [`sanitize`]). Roots an
/// older build made without the dot (`backup`, `edits`, `exports`) are just cache now: never
/// read, never written.
const BACKUP: &str = ".backup";
/// Session copies: `<cache>/.sessions/<host>/<abs path>.<random><ext>` (see [`claim`]).
const SESSIONS: &str = ".sessions";
const EDITS: &str = ".edits";
const EXPORTS: &str = ".exports";
/// The file browser's private preview copies, before every remote preview read through a
/// stream (06 §13 W8b): `<cache>/.preview/<window>/<n>`, with `<window>.lock` held by a live
/// window. Nothing makes it now; [`sweep_legacy_previews`] removes what an older sopen left.
const LEGACY_PREVIEW: &str = ".preview";

pub fn cache_path(cache: &Path, host: &str, abs: &str) -> PathBuf {
    under(cache, host, abs)
}

/// Where image and hex edits are staged and refused ones kept.
pub fn edits_root(cache: &Path) -> PathBuf {
    cache.join(EDITS)
}

/// Where an image's edits are staged and refused ones kept: `<edits root>/<host>/<abs path>`
/// (the files themselves are siblings named after it).
pub fn edits_path(cache: &Path, host: &str, abs: &str) -> PathBuf {
    under(&edits_root(cache), host, abs)
}

/// Where local exports are made before they are published.
pub fn exports_root(cache: &Path) -> PathBuf {
    cache.join(EXPORTS)
}

/// Where image and hex backups go: `<backup root>/<host>/<abs path>.<UTC stamp>`.
pub fn backup_root(cache: &Path) -> PathBuf {
    cache.join(BACKUP)
}

pub fn backup_path(cache: &Path, host: &str, abs: &str, at: SystemTime) -> PathBuf {
    let mut path = under(&backup_root(cache), host, abs);
    let name = format!(
        "{}.{}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        utc_stamp(at)
    );
    path.set_file_name(name);
    path
}

/// A job's lock (a render in `.edits`, a work folder in `.exports`): `<path>.lock`, held
/// while the job runs. The lock file is removed when the job ends; one whose lock nobody holds
/// is a job whose sopen died — [`sweep_stale_jobs`] removes what it left.
pub struct JobLock {
    path: PathBuf,
    _file: File,
}

impl JobLock {
    /// Takes the lock of the job at `job` (its file or folder; the lock is `<job>.lock`).
    pub fn take(job: &Path) -> Result<Self> {
        let path = lock_of(job);
        if let Some(dir) = path.parent() {
            private_dir(dir)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        file.try_lock()
            .map_err(|e| anyhow::anyhow!("cannot lock {}: {e}", path.display()))?;
        Ok(Self { path, _file: file })
    }
}

impl Drop for JobLock {
    /// Removed while still held, so no sweep can take a lock file that is going away.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `path` still names the file `file` is open on (device and inode, the link not followed).
fn same_file(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::symlink_metadata(path)) {
        (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
        _ => false,
    }
}

fn lock_of(job: &Path) -> PathBuf {
    let mut name = job.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    job.with_file_name(name)
}

/// Removes what jobs left whose sopen died (killed during a Save or an Export): every
/// `<job>.lock` under `.exports` and `.edits` that nobody holds, with its job — a work folder
/// in `.exports`, a staged media render (`*.render-*`) in `.edits`. Nothing without a lock file
/// is touched: a kept edit or render (a refused or failed save names it) has none. Returns what
/// was removed.
pub fn sweep_stale_jobs(cache: &Path) -> Vec<PathBuf> {
    sweep_with(cache, &mut |_| {})
}

/// [`sweep_stale_jobs`], calling `between` with each lock file it opened before it locks it
/// (tests pause the sweep there).
fn sweep_with(cache: &Path, between: &mut dyn FnMut(&Path)) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    let mut walk = vec![exports_root(cache), edits_root(cache)];
    while let Some(dir) = walk.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && dir != exports_root(cache) {
                walk.push(path);
                continue;
            }
            let Some(job) = path
                .to_str()
                .and_then(|p| p.strip_suffix(".lock"))
                .map(PathBuf::from)
            else {
                continue;
            };
            let in_edits = job.starts_with(edits_root(cache));
            let name = job.file_name().map(|n| n.to_string_lossy().into_owned());
            if in_edits && !name.is_some_and(|n| n.contains(".render-")) {
                continue;
            }
            let Ok(file) = OpenOptions::new().write(true).open(&path) else {
                continue;
            };
            between(&path);
            if file.try_lock().is_err() {
                continue; // its job is running
            }
            // A job that ends removes its lock file before letting it go: a lock got after
            // that is of a file no longer at this path (the job ended — a kept render — or a
            // new one took the name). Only the file still there is a dead job's.
            if !same_file(&file, &path) {
                continue;
            }
            let gone = if job.is_dir() {
                std::fs::remove_dir_all(&job)
            } else {
                std::fs::remove_file(&job)
            };
            if gone.is_ok() || !job.exists() {
                removed.push(job);
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    removed
}

/// Creates `dir` and its missing parents for the owner only (0700). Directories that already
/// exist keep their mode.
pub fn private_dir(dir: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))
}

/// A new file for the owner only (0600, in [`private_dir`]s), under the first name `name`
/// gives (attempt 0, 1, …) that nothing has yet — never an existing file, never through a
/// symlink. Returns it open for writing.
pub fn create_unique(mut name: impl FnMut(u32) -> PathBuf) -> Result<(PathBuf, File)> {
    const TRIES: u32 = 32;
    for attempt in 0..TRIES {
        let path = name(attempt);
        if let Some(dir) = path.parent() {
            private_dir(dir)?;
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("cannot create {}", path.display()));
            }
        }
    }
    anyhow::bail!("no free name for a new file after {TRIES} tries")
}

/// The cached copy one session works on — its app or editor, its window — with what sopen
/// knows about it. The shared copy is locked for as long as this lives.
pub struct Claim {
    pub local: PathBuf,
    /// The record of what the copy held when sopen last fetched or saved it.
    record: PathBuf,
    /// Why this session works on a copy of its own (`local` is not the shared copy): the one
    /// line to print.
    pub note: Option<String>,
    /// `local` was created empty by this claim (a session copy): nothing in it to keep.
    fresh: bool,
    /// The shared copy's lock; `None` for a session copy (only its own session uses it).
    _lock: Option<File>,
}

/// A copy moved aside before a fetch, and what is known about it.
#[derive(Debug, PartialEq, Eq)]
pub struct Kept {
    pub path: PathBuf,
    /// It differs from what sopen recorded it held: it has unsaved edits. `false`: there is no
    /// record of what was fetched, so it is only kept in case it holds edits.
    pub edited: bool,
}

impl Claim {
    /// Before a fetch writes the copy: a copy that differs from its record, or has none, is
    /// moved aside; returns where it went and which of the two it was.
    pub fn set_aside_unsaved(&self) -> Result<Option<Kept>> {
        if self.fresh || !self.local.is_file() {
            return Ok(None);
        }
        let held = hex(&crate::app::file_hash(&self.local)?);
        let edited = match read_record(&self.record) {
            Some(record) if record == held => return Ok(None),
            Some(_) => true,
            None => false,
        };
        let path = set_aside(&self.local)?;
        Ok(Some(Kept { path, edited }))
    }

    /// Records `sha256` as what the copy holds now (fetched, or saved back).
    pub fn record(&self, sha256: &[u8; 32]) -> Result<()> {
        write_record(&self.record, sha256)
    }
}

/// `name` as (stem, extension with its dot): `a.txt` → (`a`, `.txt`), `README` → (`README`, ``).
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Claims the cached copy of `abs` on `host` for this session. When another sopen session holds
/// it — or it cannot be locked at all — this one gets a copy of its own outside the mirror,
/// `<cache>/.sessions/<host>/<abs path>.<random><ext>`, created exclusively, so neither replaces
/// nor moves aside the copy another session is editing. Session copies stay until the cache is
/// cleared: sopen never deletes them.
pub fn claim(cache: &Path, host: &str, abs: &str) -> Result<Claim> {
    claim_with(
        cache,
        &cache_path(cache, host, abs),
        HELD_RETRY,
        &mut |file| file.try_lock(),
    )
}

/// How long a claim goes on asking for a lock it found held, and how often, before it takes the
/// copy for another session's ([`claim_with`]).
const HELD_RETRY: std::time::Duration = std::time::Duration::from_millis(100);
const HELD_RETRY_EVERY: std::time::Duration = std::time::Duration::from_millis(5);

/// [`claim`] of the cached copy `cached`, with `try_lock` standing in for `File::try_lock`
/// (tests: a filesystem whose locks fail) and `held_retry` for [`HELD_RETRY`] (tests: a window
/// long or short enough that no clock decides them).
///
/// A lock found held is asked for again, every [`HELD_RETRY_EVERY`] for up to `held_retry`,
/// before this session takes a copy of its own. The reason is sopen's own children: on Linux
/// each is started with a `pre_exec` (`PR_SET_PDEATHSIG`, S1-M), so it is forked, and between
/// its fork and its exec it holds a copy of every descriptor of this process — the lock of a
/// claim this process has just let go included (close-on-exec only takes it away at the exec).
/// A claim of the same copy made in that instant would find the lock held by no session at
/// all, and fall back for nothing. Such a lock is free within milliseconds; another session's
/// is held for as long as it lives, so that one costs the wait once and then gets its copy.
fn claim_with(
    cache: &Path,
    cached: &Path,
    held_retry: std::time::Duration,
    try_lock: &mut dyn FnMut(&File) -> std::result::Result<(), std::fs::TryLockError>,
) -> Result<Claim> {
    if let Some(dir) = cached.parent() {
        private_dir(dir)?;
    }
    let (file, lock) = open_lock(cache, cached)?;
    // Held by another session, or not lockable here at all: either way this session must not
    // work on the shared copy.
    let asked = std::time::Instant::now();
    let tried = loop {
        match try_lock(&file) {
            Err(std::fs::TryLockError::WouldBlock) if asked.elapsed() < held_retry => {
                std::thread::sleep(HELD_RETRY_EVERY);
            }
            tried => break tried,
        }
    };
    let why = match tried {
        Ok(()) => {
            return Ok(Claim {
                local: cached.to_path_buf(),
                record: meta_path(cache, "record", cached)?,
                note: None,
                fresh: false,
                _lock: Some(file),
            })
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            format!("another sopen session has {} open", cached.display())
        }
        Err(std::fs::TryLockError::Error(e)) => {
            format!("{} cannot be locked ({e})", lock.display())
        }
    };
    // A copy of its own, outside the mirror (where no remote file can be it or be taken for
    // it), created exclusively — empty, owner-only — for the fetch to fill. Only this session
    // uses it, so it has no lock.
    let rel = cached
        .strip_prefix(cache)
        .with_context(|| format!("{} is not under the cache", cached.display()))?;
    let base = cache.join(SESSIONS).join(rel);
    let name = base.file_name().unwrap_or_default().to_string_lossy();
    let (_, ext) = split_ext(&name);
    let (local, _) = create_unique(|_| {
        let random = crate::remote::random_hex();
        base.with_file_name(format!("{name}.{}{ext}", &random[..8]))
    })?;
    let note = format!(
        "{why}; this session works on its own copy {}",
        local.display()
    );
    Ok(Claim {
        record: meta_path(cache, "record", &local)?,
        local,
        note: Some(note),
        fresh: true,
        _lock: None,
    })
}

/// A remote file fetched into this session's cached copy.
pub struct Fetched {
    /// The copy, locked for as long as this lives: another session gets a copy of its own.
    pub claim: Claim,
    pub at: crate::remote::Stat,
    /// Where earlier copies' unsaved edits went (already printed), for a window to repeat.
    pub notices: Vec<String>,
}

/// Claims the cached copy of `abs` on `host` for this session (a line says when it is a copy
/// of its own), moves an earlier copy's unsaved edits aside (said as known), fetches with
/// `progress` (`(bytes done, total)`), and records what the copy now holds. Returns the fetch
/// and how long it took in milliseconds. Every fetch into the cache goes through here, so no
/// session replaces or moves a copy another one is editing.
pub fn fetch_claimed(
    fs: &mut dyn crate::remote::RemoteFs,
    host: &str,
    abs: &str,
    cache: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(Fetched, u64)> {
    let never = std::sync::atomic::AtomicBool::new(false);
    fetch_claimed_cancellable(fs, host, abs, cache, progress, &never)
}

/// [`fetch_claimed`] that stops when `cancel` is set
/// ([`RemoteFs::fetch_cancellable`](crate::remote::RemoteFs::fetch_cancellable)): the copy is
/// left as it was, the claim released, and the error is
/// [`Cancelled`](crate::remote::Cancelled).
pub fn fetch_claimed_cancellable(
    fs: &mut dyn crate::remote::RemoteFs,
    host: &str,
    abs: &str,
    cache: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(Fetched, u64)> {
    fetch_claimed_said(fs, host, abs, cache, progress, cancel, &mut Vec::new())
}

/// [`fetch_claimed_cancellable`], with the notices also put into `said` as soon as the claim has
/// made them — before the fetch, so a fetch that then fails or is stopped still leaves its
/// caller the place an earlier copy's edits went (on success they are in [`Fetched`] too).
pub fn fetch_claimed_said(
    fs: &mut dyn crate::remote::RemoteFs,
    host: &str,
    abs: &str,
    cache: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &std::sync::atomic::AtomicBool,
    said: &mut Vec<String>,
) -> Result<(Fetched, u64)> {
    let (claim, notices) = claim_for_fetch(cache, host, abs)?;
    said.extend(notices.iter().cloned());
    let started = std::time::Instant::now();
    let at = fs.fetch_cancellable(abs, &claim.local, progress, cancel)?;
    if let Some(sha) = &at.sha256 {
        if let Err(e) = claim.record(sha) {
            crate::debug!("not recorded: {e:#}");
        }
    }
    let ms = started.elapsed().as_millis() as u64;
    Ok((Fetched { claim, at, notices }, ms))
}

/// What every fetch into the cache does before it writes: the claim (a line says when it is a
/// copy of its own) and an earlier copy's unsaved edits moved aside — said as known, printed,
/// and returned for a window to repeat.
fn claim_for_fetch(cache: &Path, host: &str, abs: &str) -> Result<(Claim, Vec<String>)> {
    let claim = claim(cache, host, abs)?;
    if let Some(note) = &claim.note {
        eprintln!("sopen: {note}");
    }
    let mut notices = Vec::new();
    // Said as known: a copy that differs from what was recorded has unsaved edits; one with no
    // record may or may not.
    match claim.set_aside_unsaved()? {
        Some(Kept { path, edited: true }) => notices.push(format!(
            "the previous copy had unsaved edits; kept them at {}",
            path.display()
        )),
        Some(Kept {
            path,
            edited: false,
        }) => notices.push(format!(
            "no record of what was fetched for {}; kept it at {} in case it holds edits",
            claim.local.display(),
            path.display()
        )),
        None => {}
    }
    for line in &notices {
        eprintln!("sopen: {line}");
    }
    Ok((claim, notices))
}

/// Removes the preview copies an older sopen left in `cache` (`<cache>/.preview`, which this
/// one never makes), by that sopen's own rule and only what it made there: a window's folder
/// `<window>` — 12 lowercase hex characters, as it named them — and `<window>.lock`. The folder
/// of every window that is gone (its lock can be locked, or there is none) goes with that lock;
/// a lock with no folder once it is free and [`LONE_LOCK_AGE`] old (an older window makes and
/// locks its lock just before its folder); then the root itself once it is empty. A window whose
/// lock is held (an older sopen still running) keeps its folder, and anything of another name is
/// never touched — so the root stays with them. Only a real folder at exactly that place under
/// the cache is looked into: a link there, or a file, is not sopen's to remove, and nothing a
/// link inside points at is touched.
pub fn sweep_legacy_previews(cache: &Path) {
    sweep_legacy_previews_at(cache, SystemTime::now())
}

/// How old a lock with no folder must be before [`sweep_legacy_previews`] takes it for a
/// crashed window's leftover, not a window being made.
const LONE_LOCK_AGE: std::time::Duration = std::time::Duration::from_secs(60);

/// [`sweep_legacy_previews`] as of `now` (tests: a lock's age without waiting).
fn sweep_legacy_previews_at(cache: &Path, now: SystemTime) {
    let root = cache.join(LEGACY_PREVIEW);
    if !std::fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let lock_of = |dir: &Path| {
        let mut lock = dir.as_os_str().to_owned();
        lock.push(".lock");
        PathBuf::from(lock)
    };
    // `Some(held)` when the window is gone: its lock, taken here and held while its folder
    // goes (`None` inside for no lock at all); `None` when it lives or cannot be told.
    let gone = |lock: &Path| match File::open(lock) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
        Ok(file) => file.try_lock().is_ok().then_some(Some(file)),
    };
    // A window's name as an older sopen made it (`PreviewSpace`: 12 of `random_hex`'s digits).
    let window = |name: &str| {
        name.len() == 12 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    };
    let mut lone = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_str().unwrap_or_default();
        let lock_of_window = name.strip_suffix(".lock").is_some_and(window);
        match entry.file_type() {
            Ok(t) if t.is_dir() && window(name) => {
                let lock = lock_of(&path);
                if let Some(held) = gone(&lock) {
                    // `remove_dir_all` removes a link inside, never what it points at.
                    let _ = std::fs::remove_dir_all(&path);
                    let _ = std::fs::remove_file(&lock);
                    drop(held);
                }
            }
            Ok(t) if t.is_file() && lock_of_window => lone.push(path),
            _ => {}
        }
    }
    for lock in lone {
        let aged = std::fs::metadata(&lock)
            .and_then(|m| m.modified())
            .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age >= LONE_LOCK_AGE));
        if aged && !lock.with_extension("").exists() && gone(&lock).is_some() {
            let _ = std::fs::remove_file(&lock);
        }
    }
    // Removed only empty: a live window's folder keeps it.
    let _ = std::fs::remove_dir(&root);
}

/// Tests: an earlier session's cached copy of `abs` on `host`, left as a session leaves it — its
/// record says it held `fetched`, and it holds `now` — made without taking its lock (a claim
/// dropped right before another one was seen still holding its lock while the parallel tests
/// spawn processes). Returns the copy's path.
#[cfg(test)]
pub(crate) fn seed_copy(
    cache: &Path,
    host: &str,
    abs: &str,
    fetched: &[u8],
    now: &[u8],
) -> PathBuf {
    let local = cache_path(cache, host, abs);
    private_dir(local.parent().unwrap()).unwrap();
    let record = meta_path(cache, "record", &local).unwrap();
    private_dir(record.parent().unwrap()).unwrap();
    use sha2::Digest as _;
    write_record(&record, &sha2::Sha256::digest(fetched).into()).unwrap();
    std::fs::write(&local, now).unwrap();
    local
}

/// The session lock file of the cached copy `local`, open (created owner-only if new). Never
/// removed: removing it would let two sessions lock two different files.
fn open_lock(cache: &Path, local: &Path) -> Result<(File, PathBuf)> {
    let lock = meta_path(cache, "lock", local)?;
    if let Some(dir) = lock.parent() {
        private_dir(dir)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock)
        .with_context(|| format!("cannot create {}", lock.display()))?;
    Ok((file, lock))
}

/// A sibling of `edited` that keeps a refused edit out of the way of the next fetch:
/// `<file>.conflict-<UTC stamp>`.
pub fn conflict_copy_path(edited: &Path, at: SystemTime) -> PathBuf {
    let mut name = edited.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".conflict-{}", utc_stamp(at)));
    edited.with_file_name(name)
}

/// Moves `edited` to its conflict-copy path (`-N` added when that name is taken) and returns
/// where it went. Used whenever an edit cannot be uploaded, so no later fetch can overwrite it.
pub fn set_aside(edited: &Path) -> Result<PathBuf> {
    set_aside_with(edited, &mut |_| {}, &mut |_| {})
}

/// [`set_aside`], with `before_move` called on the chosen name right before the move (tests: a
/// racing process). The name is first created exclusively (an empty owner-only placeholder),
/// so the move can only ever replace that placeholder — never a file someone else made.
fn set_aside_with(
    edited: &Path,
    before_move: &mut dyn FnMut(&Path),
    after_move: &mut dyn FnMut(&Path),
) -> Result<PathBuf> {
    let first = conflict_copy_path(edited, SystemTime::now());
    let (keep, _) = create_unique(|n| {
        if n == 0 {
            return first.clone();
        }
        let mut name = first.file_name().unwrap_or_default().to_os_string();
        name.push(format!("-{n}"));
        first.with_file_name(name)
    })
    .with_context(|| format!("no free conflict-copy name next to {}", edited.display()))?;
    before_move(&keep);
    if let Err(e) = std::fs::rename(edited, &keep) {
        let _ = std::fs::remove_file(&keep);
        return Err(e).with_context(|| {
            format!(
                "cannot move {} aside to {}",
                edited.display(),
                keep.display()
            )
        });
    }
    after_move(&keep);
    // An editor may have rewritten the copy with the umask's mode: the edit is private.
    if let Err(e) = make_private(&keep) {
        crate::debug!("{} not made owner-only: {e}", keep.display());
    }
    Ok(keep)
}

/// Makes the regular file at `path` owner-only (0600) through its own descriptor: opened
/// without following a symlink (one fails the open and is left as it is) and without blocking
/// on a FIFO, checked to be a regular file, then chmodded via that descriptor — never through a
/// path that may by then name something else.
fn make_private(path: &Path) -> std::io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(std::io::Error::other("not a regular file; left as it is"));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `sha256` as the record at `record` — owner-only (0600), like the copy. Written to a
/// new file next to it and renamed onto the record's name: the rename replaces whatever entry
/// is there and never writes through it.
fn write_record(record: &Path, sha256: &[u8; 32]) -> Result<()> {
    let (temp, mut file) = create_unique(|_| {
        let mut name = record.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".tmp-{}", &crate::remote::random_hex()[..8]));
        record.with_file_name(name)
    })?;
    let written = std::io::Write::write_all(&mut file, format!("{}\n", hex(sha256)).as_bytes())
        .and_then(|()| std::fs::rename(&temp, record));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("cannot write {}", record.display()));
    }
    Ok(())
}

fn read_record(record: &Path) -> Option<String> {
    let text = std::fs::read_to_string(record).ok()?;
    Some(text.trim().to_string())
}

/// Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn parts(at: SystemTime) -> (i64, u32, u32, u64, u64, u64) {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil((secs / 86_400) as i64);
    let rem = secs % 86_400;
    (y, m, d, rem / 3_600, rem % 3_600 / 60, rem % 60)
}

/// `20261111T093005Z` — sorts by time, no characters a file name would trip on.
pub fn utc_stamp(at: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = parts(at);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

/// `2026-11-11T09:30:05Z`.
pub fn rfc3339(at: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = parts(at);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod job_tests {
    use super::*;

    /// The sweep removes what a killed sopen's jobs left — a work folder, a staged render whose
    /// lock nobody holds — and nothing else: a running job (its lock held), a folder or render
    /// without a lock (a kept render a failed save named), an image's staged edit.
    #[test]
    fn the_sweep_removes_only_dead_jobs() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path();
        let exports = exports_root(cache);
        let edits = edits_path(cache, "box", "/srv/clip.mp4");
        let made = |p: &Path, dir: bool| {
            std::fs::create_dir_all(if dir { p } else { p.parent().unwrap() }).unwrap();
            if !dir {
                std::fs::write(p, b"x").unwrap();
            }
        };
        let lock = |job: &Path| std::fs::write(lock_of(job), b"").unwrap();
        let dead_work = exports.join("aaaa");
        made(&dead_work, true);
        std::fs::write(dead_work.join("partial.mp4"), b"x").unwrap();
        lock(&dead_work);
        let live_work = exports.join("bbbb");
        let _held = JobLock::take(&live_work).unwrap();
        made(&live_work, true);
        let old_work = exports.join("cccc");
        made(&old_work, true);
        let dead_render = edits.with_file_name("clip.render-20261120T000000Z-1a2b.mp4");
        made(&dead_render, false);
        lock(&dead_render);
        let kept_render = edits.with_file_name("clip.render-20261120T000000Z-3c4d.mp4");
        made(&kept_render, false);
        let image_edit = edits.with_file_name("shot.edited-20261120T000000Z-5e6f.png");
        made(&image_edit, false);
        lock(&image_edit);
        let mut gone = sweep_stale_jobs(cache);
        gone.sort();
        let mut want = vec![dead_work.clone(), dead_render.clone()];
        want.sort();
        assert_eq!(gone, want);
        assert!(!dead_work.exists() && !lock_of(&dead_work).exists());
        assert!(!dead_render.exists() && !lock_of(&dead_render).exists());
        for kept in [&live_work, &old_work, &kept_render, &image_edit] {
            assert!(kept.exists(), "{} kept", kept.display());
        }
        assert!(lock_of(&live_work).exists(), "a running job's lock stays");
    }

    /// A sweep that opened a job's lock and then paused: meanwhile the job ended and kept its
    /// render (a failed Save names it for the owner: its lock removed, then let go), or a new
    /// job took a new lock of the same name. The sweep then gets the lock it opened — a file no
    /// longer at that path — and leaves the job alone.
    #[test]
    fn a_sweep_never_removes_a_job_whose_lock_went_while_it_paused() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path();
        let edits = edits_path(cache, "box", "/srv/clip.mp4");
        let kept = edits.with_file_name("clip.render-20261120T000000Z-1a2b.mp4");
        let again = edits.with_file_name("clip.render-20261120T000000Z-3c4d.mp4");
        let mut held = Vec::new();
        for render in [&kept, &again] {
            held.push(Some(JobLock::take(render).unwrap()));
            std::fs::write(render, b"the render").unwrap();
        }
        let mut retaken = None;
        let gone = sweep_with(cache, &mut |lock: &Path| {
            // Between the sweep's open and its lock.
            if lock == lock_of(&kept) {
                held[0].take();
            } else if lock == lock_of(&again) {
                held[1].take();
                retaken = Some(JobLock::take(&again).unwrap());
            }
        });
        assert!(gone.is_empty(), "{gone:?}");
        assert_eq!(std::fs::read(&kept).unwrap(), b"the render", "kept");
        assert!(!lock_of(&kept).exists());
        assert!(again.exists() && lock_of(&again).exists(), "running again");
        drop(retaken);
    }
}

#[cfg(test)]
mod tests {
    /// A lock found held is asked for again for a moment before the claim takes the copy for
    /// another session's: one let go within that moment (a child of sopen's, between its fork
    /// and its exec, holds a copy of a lock this process has just released) gives the shared
    /// copy, with no session copy made; one held past it is another session's, as before.
    /// No clock decides either half: the first has a window far longer than the test and lets
    /// the lock go only once the claim has found it held; the second holds it for good.
    #[test]
    fn a_claim_waits_a_moment_for_a_lock_it_found_held() {
        use std::time::Duration;
        let tmp = tempfile::tempdir().unwrap();
        let cached = cache_path(tmp.path(), "box", "/data/a.bin");
        let (held, _) = open_lock(tmp.path(), &cached).unwrap();
        held.try_lock().unwrap();
        // Let go when the claim says it found the lock held — not after some time.
        let (found_held, told) = std::sync::mpsc::channel::<()>();
        let release = std::thread::spawn(move || {
            told.recv().expect("the claim found the lock held");
            drop(held);
        });
        let mut found = 0;
        let claim = claim_with(tmp.path(), &cached, Duration::from_secs(30), &mut |file| {
            let tried = file.try_lock();
            if matches!(tried, Err(std::fs::TryLockError::WouldBlock)) {
                found += 1;
                if found == 1 {
                    found_held.send(()).unwrap();
                }
            }
            tried
        })
        .unwrap();
        release.join().unwrap();
        assert!(found >= 1, "it was held when the claim first asked");
        assert_eq!(claim.note, None, "the shared copy, not a session copy");
        assert_eq!(claim.local, cached);
        assert!(
            !tmp.path().join(SESSIONS).exists(),
            "no session copy was made"
        );
        // `claim` holds it now, for as long as it lives: another session. A short window, and
        // the whole of it is waited before the fallback.
        let window = Duration::from_millis(50);
        let asked = std::time::Instant::now();
        let second = claim_with(tmp.path(), &cached, window, &mut |file| file.try_lock()).unwrap();
        assert!(asked.elapsed() >= window, "it asked for the whole window");
        assert!(second
            .note
            .is_some_and(|n| n.contains("another sopen session has")));
        assert!(second.local.starts_with(tmp.path().join(SESSIONS)));
    }

    use super::*;
    use std::time::Duration;

    #[test]
    fn layout() {
        let root = Path::new("/c");
        assert_eq!(
            cache_path(root, "box", "/tmp/a.mp3"),
            PathBuf::from("/c/box/tmp/a.mp3")
        );
        assert_eq!(
            cache_path(root, "me@[::1]", "/x/../../etc/y"),
            PathBuf::from("/c/me@[::1]/x/etc/y")
        );
        assert_eq!(cache_path(root, "..", "/a"), PathBuf::from("/c/_../a"));
        // No host label reaches what sopen keeps about its copies.
        assert_eq!(
            cache_path(root, ".meta", "/a"),
            PathBuf::from("/c/_.meta/a")
        );
        assert_eq!(cache_path(root, "", "/a"), PathBuf::from("/c/_/a"));
        assert_eq!(
            meta_path(root, "record", Path::new("/c/box/tmp/a.mp3")).unwrap(),
            PathBuf::from("/c/.meta/record/box/tmp/a.mp3")
        );
        // No mirrored host lands in a fixed root: not even hosts named like one.
        let fixed = [
            backup_root(root),
            edits_root(root),
            exports_root(root),
            root.join(META),
            root.join(SESSIONS),
        ];
        for host in [
            "backup",
            "edits",
            "exports",
            "sessions",
            "preview",
            ".meta",
            ".backup",
            ".edits",
            ".exports",
            ".sessions",
            ".preview",
        ] {
            let mirrored = cache_path(root, host, "/x");
            for r in &fixed {
                assert!(
                    !mirrored.starts_with(r),
                    "host {host:?}: {mirrored:?} is under {r:?}"
                );
            }
        }
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_389_076);
        assert_eq!(
            backup_path(root, "box", "/tmp/shot.png", at),
            PathBuf::from("/c/.backup/box/tmp/shot.png.20261111T092436Z")
        );
        assert_eq!(
            conflict_copy_path(Path::new("/c/box/a.txt"), at),
            PathBuf::from("/c/box/a.txt.conflict-20261111T092436Z")
        );
    }

    #[test]
    fn unsaved_copies_are_set_aside_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let claim = claim(tmp.path(), "box", "/a.txt").unwrap();
        let copy = claim.local.clone();
        assert_eq!(copy, tmp.path().join("box/a.txt"));
        assert_eq!(
            claim.set_aside_unsaved().unwrap(),
            None,
            "nothing cached yet"
        );

        std::fs::write(&copy, "fetched").unwrap();
        claim
            .record(&crate::app::file_hash(&copy).unwrap())
            .unwrap();
        assert_eq!(claim.set_aside_unsaved().unwrap(), None, "clean copy stays");
        assert!(copy.exists());

        std::fs::write(&copy, "my edit").unwrap();
        let kept = claim
            .set_aside_unsaved()
            .unwrap()
            .expect("edited copy moved");
        assert!(kept.edited, "it differs from its record");
        let kept = kept.path;
        assert!(!copy.exists());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "my edit");
        let name = kept.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("a.txt.conflict-"), "{name}");

        // A copy with no record is kept in case it holds edits (and said so: not "edited"); a
        // second set-aside in the same second gets its own name instead of replacing the first.
        std::fs::write(&copy, "second edit").unwrap();
        std::fs::remove_file(&claim.record).unwrap();
        let again = claim
            .set_aside_unsaved()
            .unwrap()
            .expect("unrecorded copy moved");
        assert!(!again.edited, "no record: not known to be edited");
        let again = again.path;
        assert_ne!(again, kept);
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "my edit");
        assert_eq!(std::fs::read_to_string(&again).unwrap(), "second edit");
    }

    /// A session copy (the shared one is held) lives outside the mirror, under
    /// `<cache>/.sessions/<host>/<abs path>.<8 hex><ext>`: created by the claim itself — empty,
    /// owner-only, under a name no other claim gets — holding nothing to move aside, and with no
    /// lock (only its own session uses it).
    #[test]
    fn a_session_copy_lands_under_sessions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let one = claim(tmp.path(), "box", "/d/a.txt").unwrap();
        let two = claim(tmp.path(), "box", "/d/a.txt").unwrap();
        let three = claim(tmp.path(), "box", "/d/a.txt").unwrap();
        assert!(one.note.is_none() && !one.local.exists());
        let dir = tmp.path().join(".sessions/box/d");
        for c in [&two, &three] {
            assert_eq!(c.local.parent(), Some(dir.as_path()), "{:?}", c.local);
            let name = c.local.file_name().unwrap().to_string_lossy().into_owned();
            let hex = name
                .strip_prefix("a.txt.")
                .and_then(|r| r.strip_suffix(".txt"));
            assert!(
                hex.is_some_and(|h| h.len() == 8 && h.bytes().all(|b| b.is_ascii_hexdigit())),
                "{name}"
            );
            let meta = std::fs::symlink_metadata(&c.local).expect("created by the claim");
            assert!(
                meta.file_type().is_file() && meta.len() == 0,
                "{:?}",
                c.local
            );
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
            assert_eq!(c.set_aside_unsaved().unwrap(), None);
            assert!(
                !meta_path(tmp.path(), "lock", &c.local).unwrap().exists(),
                "no lock"
            );
            assert!(c
                .note
                .as_deref()
                .unwrap()
                .ends_with(&*c.local.to_string_lossy()));
        }
        assert_ne!(two.local, three.local);
        assert_ne!(two.record, three.record);
    }

    /// `create_unique` skips a name that is taken — by a file or by a symlink pointing
    /// elsewhere — without touching either, and makes an owner-only file under the next one.
    #[test]
    fn create_unique_never_opens_an_existing_name() {
        if crate::testutil::rerun_under_umask_022(
            "cache::tests::create_unique_never_opens_an_existing_name",
        ) {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let taken = tmp.path().join("d/taken");
        std::fs::create_dir_all(taken.parent().unwrap()).unwrap();
        std::fs::write(&taken, "someone's").unwrap();
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, "elsewhere").unwrap();
        let link = tmp.path().join("d/link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let dangling = tmp.path().join("d/dangling");
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &dangling).unwrap();
        let names = [&taken, &link, &dangling, &tmp.path().join("d/e/free")];
        let mut tried = Vec::new();
        let (path, mut file) = create_unique(|n| {
            tried.push(n);
            names[n as usize].to_path_buf()
        })
        .unwrap();
        std::io::Write::write_all(&mut file, b"mine").unwrap();
        assert_eq!(tried, [0, 1, 2, 3]);
        assert_eq!(path, *names[3]);
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "someone's");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "elsewhere");
        assert!(
            !tmp.path().join("nowhere").exists(),
            "not created through the symlink"
        );
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    /// Another process that creates a file under the chosen name just before the move (the
    /// way a second sopen would, exclusively) never has it replaced: the edit lands under a
    /// name of its own and the other file is intact.
    #[test]
    fn set_aside_never_replaces_a_file_created_under_its_name() {
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("a.txt");
        std::fs::write(&copy, "my edit").unwrap();
        let mut theirs = None;
        let kept = set_aside_with(
            &copy,
            &mut |name| {
                let created = OpenOptions::new().write(true).create_new(true).open(name);
                if let Ok(mut file) = created {
                    std::io::Write::write_all(&mut file, b"someone else's").unwrap();
                    theirs = Some(name.to_path_buf());
                }
            },
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "my edit");
        if let Some(theirs) = theirs {
            assert_ne!(theirs, kept);
            assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "someone else's");
        }
        assert!(!copy.exists());
    }

    /// The record and the lock are the owner's only, like the copy they describe, in 0700
    /// directories — also a record left with the umask's mode.
    #[test]
    fn records_and_locks_are_owner_only() {
        if crate::testutil::rerun_under_umask_022("cache::tests::records_and_locks_are_owner_only")
        {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let (one, two) = (
            claim(tmp.path(), "box", "/d/a.txt").unwrap(),
            claim(tmp.path(), "box", "/d/b.txt").unwrap(),
        );
        private_dir(two.record.parent().unwrap()).unwrap();
        std::fs::write(&two.record, "stale\n").unwrap();
        std::fs::set_permissions(&two.record, std::fs::Permissions::from_mode(0o644)).unwrap();
        for c in [&one, &two] {
            c.record(&[7; 32]).unwrap();
            assert_eq!(mode(&c.record), 0o600, "{:?}", c.record);
            assert_eq!(read_record(&c.record), Some(hex(&[7; 32])));
            let lock = meta_path(tmp.path(), "lock", &c.local).unwrap();
            assert_eq!(mode(&lock), 0o600, "{lock:?}");
            for dir in [c.record.parent().unwrap(), lock.parent().unwrap()] {
                for d in dir
                    .ancestors()
                    .take_while(|d| d.starts_with(tmp.path().join(META)))
                {
                    assert_eq!(mode(d), 0o700, "{d:?}");
                }
            }
        }
    }

    /// A symlink at the record's name is replaced by the record, never written through: the
    /// file it pointed at keeps its content and mode.
    #[test]
    fn a_record_is_never_written_through_a_symlink() {
        if crate::testutil::rerun_under_umask_022(
            "cache::tests::a_record_is_never_written_through_a_symlink",
        ) {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let unrelated = tmp.path().join("unrelated");
        std::fs::write(&unrelated, "unrelated").unwrap();
        std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o644)).unwrap();
        let claim = claim(tmp.path(), "box", "/a.txt").unwrap();
        private_dir(claim.record.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&unrelated, &claim.record).unwrap();
        claim.record(&[7; 32]).unwrap();
        assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "unrelated");
        let mode = std::fs::metadata(&unrelated).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let record = std::fs::symlink_metadata(&claim.record).unwrap();
        assert!(record.file_type().is_file(), "the record replaced the link");
        assert_eq!(record.permissions().mode() & 0o777, 0o600);
        assert_eq!(read_record(&claim.record), Some(hex(&[7; 32])));
        let names: Vec<_> = std::fs::read_dir(claim.record.parent().unwrap())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(names.len(), 1, "no temp left behind: {names:?}");
    }

    /// A copy that is a symlink is moved aside as the link itself: what it points at keeps its
    /// content and mode.
    #[test]
    fn set_aside_moves_a_symlink_without_touching_what_it_points_at() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let referent = tmp.path().join("referent");
        std::fs::write(&referent, "referent").unwrap();
        std::fs::set_permissions(&referent, std::fs::Permissions::from_mode(0o644)).unwrap();
        let copy = tmp.path().join("a.txt");
        std::os::unix::fs::symlink(&referent, &copy).unwrap();
        let kept = set_aside(&copy).unwrap();
        assert!(std::fs::symlink_metadata(&kept)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_link(&kept).unwrap(), referent);
        assert_eq!(std::fs::read_to_string(&referent).unwrap(), "referent");
        let mode = std::fs::metadata(&referent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert!(
            copy.symlink_metadata().is_err(),
            "out of the way of the next fetch"
        );
    }

    /// Where the copy cannot be locked at all (a filesystem without working locks), a session
    /// never works on the shared copy unlocked: it gets an exclusively created copy of its own,
    /// as when another session holds the lock.
    #[test]
    fn a_copy_that_cannot_be_locked_is_not_shared() {
        let tmp = tempfile::tempdir().unwrap();
        let cached = cache_path(tmp.path(), "box", "/a.txt");
        let unsupported =
            || std::fs::TryLockError::Error(std::io::Error::from(std::io::ErrorKind::Unsupported));
        let one = claim_with(tmp.path(), &cached, HELD_RETRY, &mut |_| Err(unsupported())).unwrap();
        let two = claim_with(tmp.path(), &cached, HELD_RETRY, &mut |_| Err(unsupported())).unwrap();
        for c in [&one, &two] {
            assert_ne!(c.local, cached);
            let name = c.local.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with("a.txt.") && name.ends_with(".txt"),
                "{name}"
            );
        }
        assert_ne!(one.local, two.local);
        let note = one.note.as_deref().unwrap();
        assert!(note.contains("cannot be locked"), "{note}");
        assert!(note.ends_with(&*one.local.to_string_lossy()), "{note}");
    }

    /// The owner-only chmod after the move goes through the moved entry itself, never through a
    /// path that may by then name something else: here the kept name is swapped for a symlink
    /// right after the move, and what it points at keeps its mode.
    #[test]
    fn set_aside_never_chmods_through_a_name_swapped_after_the_move() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let referent = tmp.path().join("referent");
        std::fs::write(&referent, "referent").unwrap();
        std::fs::set_permissions(&referent, std::fs::Permissions::from_mode(0o644)).unwrap();
        let copy = tmp.path().join("a.txt");
        std::fs::write(&copy, "my edit").unwrap();
        let kept = set_aside_with(&copy, &mut |_| {}, &mut |kept| {
            std::fs::rename(kept, tmp.path().join("moved-on")).unwrap();
            std::os::unix::fs::symlink(&referent, kept).unwrap();
        })
        .unwrap();
        assert!(std::fs::symlink_metadata(&kept)
            .unwrap()
            .file_type()
            .is_symlink());
        let mode = std::fs::metadata(&referent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert_eq!(std::fs::read_to_string(&referent).unwrap(), "referent");
    }

    /// W8b: the preview root an older sopen left is swept by its own rule, and only what that
    /// sopen made there — a window's folder named by 12 hex characters, and `<that>.lock`: a
    /// gone window's folder and lock, a folder with no lock, an old lock without its folder.
    /// A live window (its lock held) keeps its folder, a fresh lock without one (a window
    /// being made) stays, and anything else — a folder `keep/` with files, a `foo.lock` — is
    /// never touched, so the root stays with it; a link at the root's place is left, and what
    /// it points at.
    #[test]
    fn the_legacy_preview_root_is_swept_and_a_live_windows_folder_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(LEGACY_PREVIEW);
        let (dead, orphan, lonely, fresh, live) = (
            "0a1b2c3d4e01",
            "0a1b2c3d4e02",
            "0a1b2c3d4e03",
            "0a1b2c3d4e04",
            "0a1b2c3d4e05",
        );
        std::fs::create_dir_all(root.join(dead)).unwrap();
        std::fs::write(root.join(dead).join("0.png"), "left behind").unwrap();
        std::fs::write(root.join(format!("{dead}.lock")), "").unwrap();
        std::fs::create_dir(root.join(orphan)).unwrap();
        std::fs::write(root.join(format!("{lonely}.lock")), "").unwrap();
        std::fs::write(root.join(format!("{fresh}.lock")), "").unwrap();
        std::fs::create_dir(root.join(live)).unwrap();
        std::fs::write(root.join(live).join("0.txt"), "in use").unwrap();
        let held = File::create(root.join(format!("{live}.lock"))).unwrap();
        held.try_lock().unwrap();
        // Not of that grammar: a folder with files and no lock, and an old free lock.
        std::fs::create_dir(root.join("keep")).unwrap();
        std::fs::write(root.join("keep/mine.txt"), "not sopen's").unwrap();
        std::fs::write(root.join("foo.lock"), "").unwrap();
        let old = SystemTime::now() - 2 * LONE_LOCK_AGE;
        for aged in [format!("{lonely}.lock"), "foo.lock".to_string()] {
            File::options()
                .write(true)
                .open(root.join(aged))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let left = || {
            let mut left: Vec<String> = std::fs::read_dir(&root)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            left.sort();
            left
        };
        sweep_legacy_previews(tmp.path());
        let live_lock = format!("{live}.lock");
        let fresh_lock = format!("{fresh}.lock");
        assert_eq!(
            left(),
            [&fresh_lock, live, &live_lock, "foo.lock", "keep"],
            "a live window, a window being made and what is not sopen's stay"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(live).join("0.txt")).unwrap(),
            "in use"
        );
        drop(held);
        // Two minutes on, the fresh lock is old. A lock just let go may still be held for an
        // instant — a process another test forks has a copy of its descriptor until its exec
        // (the note on `claim`) — so the sweep is asked again until the window is gone.
        let later = SystemTime::now() + 2 * LONE_LOCK_AGE;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while {
            sweep_legacy_previews_at(tmp.path(), later);
            left() != ["foo.lock", "keep"]
        } {
            assert!(
                std::time::Instant::now() < deadline,
                "the last window stayed: {:?}",
                left()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            std::fs::read_to_string(root.join("keep/mine.txt")).unwrap(),
            "not sopen's"
        );
        // A link where the root is: not sopen's folder.
        let other = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::create_dir(elsewhere.path().join(dead)).unwrap();
        let linked = other.path().join(LEGACY_PREVIEW);
        std::os::unix::fs::symlink(elsewhere.path(), &linked).unwrap();
        sweep_legacy_previews(other.path());
        assert!(std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(elsewhere.path().join(dead).exists());
    }

    #[test]
    fn stamps() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(utc_stamp(epoch), "19700101T000000Z");
        // 2024-02-29 23:59:59 UTC (leap day).
        let leap = epoch + Duration::from_secs(1_709_251_199);
        assert_eq!(rfc3339(leap), "2024-02-29T23:59:59Z");
        let y2k = epoch + Duration::from_secs(951_782_400);
        assert_eq!(rfc3339(y2k), "2000-02-29T00:00:00Z");
    }
}
