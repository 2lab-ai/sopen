//! Local paths: the fetch cache `<cache>/<host>/<abs path>` (each copy with a
//! `.<name>.sopen-baseline` record of what it held when fetched or saved, and the session lock
//! `.<name>.sopen-lock`), edits kept aside as `<copy>.conflict-<UTC stamp>`, image backups
//! `<cache>/backup/<host>/<abs path>.<UTC stamp>` (spec §5), and the state dir (ssh control
//! sockets, `usage.jsonl`). `SOPEN_CACHE_DIR` / `SOPEN_STATE_DIR` override both. What sopen
//! creates under the cache is the owner's only (0700 directories, 0600 files).

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

fn sanitize(host: &str) -> String {
    let cleaned: String = host
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    match cleaned.as_str() {
        "" | "." | ".." => "_".to_string(),
        _ => cleaned,
    }
}

pub fn cache_path(cache: &Path, host: &str, abs: &str) -> PathBuf {
    under(cache, host, abs)
}

pub fn backup_path(cache: &Path, host: &str, abs: &str, at: SystemTime) -> PathBuf {
    let mut path = under(&cache.join("backup"), host, abs);
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

/// `<dir>/.<name>.sopen-lock` next to a cached copy: the advisory lock a session holds on the
/// copy (hidden, like the baseline record, so a remote file named `<name>.lock` is not it).
/// Never removed: removing it would let two sessions lock two different files.
pub fn lock_path(local: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(local.file_name().unwrap_or_default());
    name.push(".sopen-lock");
    local.with_file_name(name)
}

/// The cached copy one session works on — its app or editor, its window — locked for as long
/// as this lives.
pub struct Claim {
    pub local: PathBuf,
    /// Why this session works on a copy of its own (`local` is not the shared copy): the one
    /// line to print.
    pub note: Option<String>,
    /// `None` only for a session copy whose own lock failed (its name is still its own).
    _lock: Option<File>,
}

/// Claims `cached` for this session. When another sopen session holds it — or it cannot be
/// locked at all — this one gets a sibling copy of its own, `<stem>.session-<random>.<ext>`
/// (created exclusively), so neither replaces nor moves aside the copy another is editing.
pub fn claim(cached: &Path) -> Result<Claim> {
    claim_with(cached, &mut |file| file.try_lock())
}

/// [`claim`], with `try_lock` standing in for `File::try_lock` (tests: a filesystem whose
/// locks fail).
fn claim_with(
    cached: &Path,
    try_lock: &mut dyn FnMut(&File) -> std::result::Result<(), std::fs::TryLockError>,
) -> Result<Claim> {
    if let Some(dir) = cached.parent() {
        private_dir(dir)?;
    }
    let path = lock_path(cached);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    // Held by another session, or not lockable here at all: either way this session must not
    // work on the shared copy.
    let why = match try_lock(&file) {
        Ok(()) => {
            return Ok(Claim {
                local: cached.to_path_buf(),
                note: None,
                _lock: Some(file),
            })
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            format!("another sopen session has {} open", cached.display())
        }
        Err(std::fs::TryLockError::Error(e)) => {
            format!("{} cannot be locked ({e})", cached.display())
        }
    };
    let name = cached.file_name().unwrap_or_default().to_string_lossy();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (&name[..], ""),
    };
    // The sibling's own lock file, created exclusively, reserves its name.
    let mut local = PathBuf::new();
    let (lock, file) = create_unique(|_| {
        let random = crate::remote::random_hex();
        local = cached.with_file_name(format!("{stem}.session-{}{ext}", &random[..8]));
        lock_path(&local)
    })?;
    let lock_held = match try_lock(&file) {
        Ok(()) => true,
        Err(e) => {
            crate::debug!("{} not locked ({e})", lock.display());
            false
        }
    };
    let note = format!(
        "{why}; this session works on its own copy {}",
        local.display()
    );
    Ok(Claim {
        local,
        note: Some(note),
        _lock: lock_held.then_some(file),
    })
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
    set_aside_with(edited, &mut |_| {})
}

/// [`set_aside`], with `before_move` called on the chosen name right before the move (tests: a
/// racing process). The name is first created exclusively (an empty owner-only placeholder),
/// so the move can only ever replace that placeholder — never a file someone else made.
fn set_aside_with(edited: &Path, before_move: &mut dyn FnMut(&Path)) -> Result<PathBuf> {
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
    // What is at the copy's name, not what it may point at: only a regular file is the edit
    // itself; a symlink is moved aside as the link, and nothing is done through it.
    let regular = std::fs::symlink_metadata(edited).is_ok_and(|m| m.file_type().is_file());
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
    // An editor may have rewritten the copy with the umask's mode: the edit is private.
    if !regular {
        crate::debug!(
            "{} is not a regular file; moved aside as is",
            keep.display()
        );
    } else if let Err(e) = std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(0o600)) {
        crate::debug!("{} not made owner-only: {e}", keep.display());
    }
    Ok(keep)
}

/// `<dir>/.<name>.sopen-baseline` next to a fetched copy: the hex SHA-256 the copy held when
/// sopen last fetched or saved it. A copy that no longer matches holds unsaved edits.
pub fn baseline_record_path(local: &Path) -> PathBuf {
    let mut name = std::ffi::OsString::from(".");
    name.push(local.file_name().unwrap_or_default());
    name.push(".sopen-baseline");
    local.with_file_name(name)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Records `sha256` as what the fetched copy `local` holds — owner-only (0600), like the copy.
/// Written to a new file next to it and renamed onto the record's name: the rename replaces
/// whatever entry is there (an older sopen's 0644 record, a symlink) and never writes through
/// it.
pub fn write_baseline(local: &Path, sha256: &[u8; 32]) -> Result<()> {
    let record = baseline_record_path(local);
    let (temp, mut file) = create_unique(|_| {
        let mut name = record.file_name().unwrap_or_default().to_os_string();
        name.push(format!("-{}", &crate::remote::random_hex()[..8]));
        record.with_file_name(name)
    })?;
    let written = std::io::Write::write_all(&mut file, format!("{}\n", hex(sha256)).as_bytes())
        .and_then(|()| std::fs::rename(&temp, &record));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("cannot write {}", record.display()));
    }
    Ok(())
}

fn read_baseline(local: &Path) -> Option<String> {
    let text = std::fs::read_to_string(baseline_record_path(local)).ok()?;
    Some(text.trim().to_string())
}

/// Before a fetch overwrites `local`: if it exists and differs from its recorded baseline (or
/// has no record), it may hold unsaved edits — move it aside and return where it went.
pub fn set_aside_unsaved(local: &Path) -> Result<Option<PathBuf>> {
    if !local.is_file() {
        return Ok(None);
    }
    let held = hex(&crate::app::file_hash(local)?);
    if read_baseline(local).as_deref() == Some(held.as_str()) {
        return Ok(None);
    }
    set_aside(local).map(Some)
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
        assert_eq!(cache_path(root, "..", "/a"), PathBuf::from("/c/_/a"));
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_389_076);
        assert_eq!(
            backup_path(root, "box", "/tmp/shot.png", at),
            PathBuf::from("/c/backup/box/tmp/shot.png.20261111T092436Z")
        );
        assert_eq!(
            conflict_copy_path(Path::new("/c/box/a.txt"), at),
            PathBuf::from("/c/box/a.txt.conflict-20261111T092436Z")
        );
    }

    #[test]
    fn unsaved_copies_are_set_aside_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let copy = tmp.path().join("a.txt");
        assert_eq!(
            set_aside_unsaved(&copy).unwrap(),
            None,
            "nothing cached yet"
        );

        std::fs::write(&copy, "fetched").unwrap();
        write_baseline(&copy, &crate::app::file_hash(&copy).unwrap()).unwrap();
        assert_eq!(set_aside_unsaved(&copy).unwrap(), None, "clean copy stays");
        assert!(copy.exists());

        std::fs::write(&copy, "my edit").unwrap();
        let kept = set_aside_unsaved(&copy)
            .unwrap()
            .expect("edited copy moved");
        assert!(!copy.exists());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "my edit");
        let name = kept.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("a.txt.conflict-"), "{name}");

        // A copy with no record is treated as possibly edited; a second set-aside in the same
        // second gets its own name instead of replacing the first.
        std::fs::write(&copy, "second edit").unwrap();
        std::fs::remove_file(baseline_record_path(&copy)).unwrap();
        let again = set_aside_unsaved(&copy)
            .unwrap()
            .expect("unrecorded copy moved");
        assert_ne!(again, kept);
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "my edit");
        assert_eq!(std::fs::read_to_string(&again).unwrap(), "second edit");
        assert_eq!(
            baseline_record_path(Path::new("/c/h/x.bin")),
            PathBuf::from("/c/h/.x.bin.sopen-baseline")
        );
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
        let kept = set_aside_with(&copy, &mut |name| {
            let created = OpenOptions::new().write(true).create_new(true).open(name);
            if let Ok(mut file) = created {
                std::io::Write::write_all(&mut file, b"someone else's").unwrap();
                theirs = Some(name.to_path_buf());
            }
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "my edit");
        if let Some(theirs) = theirs {
            assert_ne!(theirs, kept);
            assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "someone else's");
        }
        assert!(!copy.exists());
    }

    /// The baseline record is the owner's only, like the copy it describes — also one an older
    /// sopen left with the umask's mode.
    #[test]
    fn baseline_records_are_owner_only() {
        if crate::testutil::rerun_under_umask_022("cache::tests::baseline_records_are_owner_only") {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let (fresh, old) = (tmp.path().join("a.txt"), tmp.path().join("b.txt"));
        std::fs::write(baseline_record_path(&old), "stale\n").unwrap();
        std::fs::set_permissions(
            baseline_record_path(&old),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        for copy in [&fresh, &old] {
            write_baseline(copy, &[7; 32]).unwrap();
            let record = baseline_record_path(copy);
            assert_eq!(mode(&record), 0o600, "{record:?}");
            assert_eq!(read_baseline(copy), Some(hex(&[7; 32])));
        }
    }

    /// A symlink at the record's name is replaced by the record, never written through: the
    /// file it pointed at keeps its content and mode.
    #[test]
    fn write_baseline_never_writes_through_a_symlink() {
        if crate::testutil::rerun_under_umask_022(
            "cache::tests::write_baseline_never_writes_through_a_symlink",
        ) {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let unrelated = tmp.path().join("unrelated");
        std::fs::write(&unrelated, "unrelated").unwrap();
        std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o644)).unwrap();
        let copy = tmp.path().join("a.txt");
        std::os::unix::fs::symlink(&unrelated, baseline_record_path(&copy)).unwrap();
        write_baseline(&copy, &[7; 32]).unwrap();
        assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "unrelated");
        let mode = std::fs::metadata(&unrelated).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        let record = std::fs::symlink_metadata(baseline_record_path(&copy)).unwrap();
        assert!(record.file_type().is_file(), "the record replaced the link");
        assert_eq!(record.permissions().mode() & 0o777, 0o600);
        assert_eq!(read_baseline(&copy), Some(hex(&[7; 32])));
        let names: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
        assert_eq!(names.len(), 2, "no temp left behind: {names:?}");
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
        let cached = tmp.path().join("box/a.txt");
        let unsupported =
            || std::fs::TryLockError::Error(std::io::Error::from(std::io::ErrorKind::Unsupported));
        let one = claim_with(&cached, &mut |_| Err(unsupported())).unwrap();
        let two = claim_with(&cached, &mut |_| Err(unsupported())).unwrap();
        for c in [&one, &two] {
            assert_ne!(c.local, cached);
            let name = c.local.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with("a.session-") && name.ends_with(".txt"),
                "{name}"
            );
        }
        assert_ne!(one.local, two.local);
        let note = one.note.as_deref().unwrap();
        assert!(note.contains("cannot be locked"), "{note}");
        assert!(note.ends_with(&*one.local.to_string_lossy()), "{note}");
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
