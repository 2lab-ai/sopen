//! Local paths: the fetch cache `<cache>/<host>/<abs path>` (each copy with a
//! `.<name>.sopen-baseline` record of what it held when fetched or saved), edits kept aside as
//! `<copy>.conflict-<UTC stamp>`, image backups `<cache>/backup/<host>/<abs path>.<UTC stamp>`
//! (spec §5), and the state dir (ssh control sockets, `usage.jsonl`). `SOPEN_CACHE_DIR` /
//! `SOPEN_STATE_DIR` override both.

use anyhow::{Context, Result};
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
    let first = conflict_copy_path(edited, SystemTime::now());
    for n in 0..1000u32 {
        let keep = if n == 0 {
            first.clone()
        } else {
            let mut name = first.file_name().unwrap_or_default().to_os_string();
            name.push(format!("-{n}"));
            first.with_file_name(name)
        };
        if keep.symlink_metadata().is_ok() {
            continue;
        }
        std::fs::rename(edited, &keep).with_context(|| {
            format!(
                "cannot move {} aside to {}",
                edited.display(),
                keep.display()
            )
        })?;
        return Ok(keep);
    }
    anyhow::bail!("no free conflict-copy name next to {}", edited.display())
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

/// Records `sha256` as what the fetched copy `local` holds.
pub fn write_baseline(local: &Path, sha256: &[u8; 32]) -> Result<()> {
    let record = baseline_record_path(local);
    std::fs::write(&record, format!("{}\n", hex(sha256)))
        .with_context(|| format!("cannot write {}", record.display()))
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
