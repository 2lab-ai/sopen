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
    claim_with(cache, &cache_path(cache, host, abs), &mut |file| {
        file.try_lock()
    })
}

/// [`claim`] of the cached copy `cached`, with `try_lock` standing in for `File::try_lock`
/// (tests: a filesystem whose locks fail).
fn claim_with(
    cache: &Path,
    cached: &Path,
    try_lock: &mut dyn FnMut(&File) -> std::result::Result<(), std::fs::TryLockError>,
) -> Result<Claim> {
    if let Some(dir) = cached.parent() {
        private_dir(dir)?;
    }
    let (file, lock) = open_lock(cache, cached)?;
    // Held by another session, or not lockable here at all: either way this session must not
    // work on the shared copy.
    let why = match try_lock(&file) {
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
    let started = std::time::Instant::now();
    let at = fs.fetch(abs, &claim.local, progress)?;
    if let Some(sha) = &at.sha256 {
        if let Err(e) = claim.record(sha) {
            crate::debug!("not recorded: {e:#}");
        }
    }
    let ms = started.elapsed().as_millis() as u64;
    Ok((Fetched { claim, at, notices }, ms))
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
mod tests {
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
            ".meta",
            ".backup",
            ".edits",
            ".exports",
            ".sessions",
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
        let one = claim_with(tmp.path(), &cached, &mut |_| Err(unsupported())).unwrap();
        let two = claim_with(tmp.path(), &cached, &mut |_| Err(unsupported())).unwrap();
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
