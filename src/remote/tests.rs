use super::*;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, UNIX_EPOCH};

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// `.<name>.sopen-<16 hex>` — the temp names the write algorithms create.
fn is_temp(name: &str) -> bool {
    name.rsplit_once(".sopen-")
        .is_some_and(|(_, tail)| tail.len() == 16 && tail.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Temp files in `dir` — they must never survive a write.
fn temps(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| is_temp(n))
        .collect()
}

fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn sha(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// LocalFs with failures and races injected, to prove what the provided algorithms do.
#[derive(Default)]
struct Hooks {
    fail_rename: bool,
    rename_unknown: bool,
    fail_create_after_bytes: bool,
    /// Someone else already holds the temp name this write picks.
    take_temp_name: bool,
    /// `link_new` fails as "not supported" (the filesystem has no hard links).
    no_links: bool,
    /// `can_link` says no (the server lacks hardlink@openssh.com).
    cannot_link: bool,
    /// `can_replace` says no (the server lacks posix-rename@openssh.com).
    cannot_replace: bool,
    /// Right before the first publish of a final name, another writer creates it.
    competitor: bool,
    fail_remove: bool,
    /// Another writer appends to the target right after our rename lands.
    grow_after_rename: bool,
    /// Temps look owned by another uid; `owner_fix_works` decides whether set_owner helps.
    foreign_temp_owner: bool,
    owner_fix_works: bool,
    /// The server leaves this attribute out of the target's stat.
    missing: Option<&'static str>,
    /// Content checks stop above this size (stands in for 256 MiB).
    hash_limit: Option<u64>,
    /// Right after our rename, another writer installs same-size content with another mtime.
    rewrite_after_rename: bool,
    /// The request reaches the server and takes effect, but its reply is lost (connection
    /// closed): "rename", "link" or "create".
    lost_reply: Option<&'static str>,
}

#[derive(Default)]
struct Hooked {
    inner: LocalFs,
    h: Hooks,
    raced: HashSet<String>,
    owner_fixed: bool,
    renames: usize,
}

impl Hooked {
    fn new(h: Hooks) -> Self {
        Self {
            h,
            ..Self::default()
        }
    }

    fn race(&mut self, path: &str) {
        if self.h.competitor && !is_temp(path) && self.raced.is_empty() {
            self.raced.insert(path.to_string());
            std::fs::write(path, "theirs").unwrap();
        }
    }
}

impl RemoteFs for Hooked {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.inner.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        let mut st = self.inner.lstat_opt(path)?;
        let temp = is_temp(path.rsplit('/').next().unwrap());
        if let Some(s) = st.as_mut() {
            if temp && self.h.foreign_temp_owner && !self.owner_fixed {
                s.uid = s.uid.map(|u| u + 1);
            }
            if !temp && self.h.missing.is_some() {
                s.missing = self.h.missing;
            }
        }
        Ok(st)
    }
    fn realpath(&mut self, path: &str) -> Result<String> {
        self.inner.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> Result<Vec<u8>> {
        self.inner.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> Result<Vec<String>> {
        self.inner.list_dir(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.inner.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        mode: Option<u32>,
    ) -> std::result::Result<Uploaded, CreateError> {
        if self.h.take_temp_name && is_temp(path.rsplit('/').next().unwrap()) {
            std::fs::write(path, "theirs").unwrap();
        }
        self.race(path);
        let up = self.inner.create_from(local, path, mode)?;
        if self.h.lost_reply == Some("create") {
            return Err(CreateError::new(
                Created::Unknown,
                anyhow!("simulated: connection closed before the reply"),
            ));
        }
        if self.h.fail_create_after_bytes {
            return Err(CreateError::new(
                Created::Yes,
                anyhow!("simulated connection loss after upload"),
            ));
        }
        Ok(up)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.inner.set_mode(path, mode)
    }
    fn set_owner(&mut self, path: &str, uid: u32, gid: u32) -> Result<()> {
        if self.h.foreign_temp_owner && !self.h.owner_fix_works {
            bail!("simulated: operation not permitted");
        }
        self.owner_fixed = true;
        self.inner.set_owner(path, uid, gid)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.renames += 1;
        if self.h.fail_rename {
            bail!("simulated rename failure");
        }
        if self.h.rename_unknown {
            return Err(unknown("simulated: rename sent, no reply"));
        }
        self.inner.rename_over(from, to)?;
        if self.h.lost_reply == Some("rename") {
            return Err(unknown(
                "rename: simulated connection closed before the reply",
            ));
        }
        if self.h.rewrite_after_rename {
            let len = std::fs::metadata(to).unwrap().len() as usize;
            std::fs::write(to, "x".repeat(len)).unwrap();
            set_mtime(Path::new(to), 1_700_009_999);
        }
        if self.h.grow_after_rename {
            let mut f = std::fs::OpenOptions::new().append(true).open(to).unwrap();
            f.write_all(b" + another writer").unwrap();
        }
        Ok(())
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        if self.h.no_links {
            bail!("simulated: hard links are not supported here");
        }
        self.race(to);
        self.inner.link_new(from, to)?;
        if self.h.lost_reply == Some("link") {
            return Err(unknown(
                "link: simulated connection closed before the reply",
            ));
        }
        Ok(())
    }
    fn can_link(&self) -> bool {
        !self.h.cannot_link
    }
    fn can_replace(&self) -> bool {
        !self.h.cannot_replace
    }
    fn hash_check_limit(&self) -> u64 {
        self.h.hash_limit.unwrap_or(HASH_CHECK_MAX)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        if self.h.fail_remove {
            bail!("simulated: permission denied");
        }
        self.inner.remove(path)
    }
}

/// A remote file `name` holding `body`, its fetched copy, and the fetch baseline.
fn fetched(dir: &Path, name: &str, body: &str) -> (PathBuf, PathBuf, Stat) {
    let target = dir.join(name);
    std::fs::write(&target, body).unwrap();
    set_mtime(&target, 1_700_000_000);
    let local = dir.join("cache").join(name);
    let at = LocalFs::new()
        .fetch(p(&target), &local, &mut |_, _| {})
        .unwrap();
    (target, local, at)
}

use std::path::PathBuf;

#[test]
fn fetch_then_write_atomic_happy_path_keeps_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote");
    std::fs::create_dir(&remote).unwrap();
    let target = remote.join("a.sh");
    std::fs::write(&target, "echo one\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o750)).unwrap();

    let mut fs = LocalFs::new();
    let local = tmp.path().join("cache/a.sh");
    let mut seen = Vec::new();
    let at = fs
        .fetch(p(&target), &local, &mut |done, total| {
            seen.push((done, total))
        })
        .unwrap();
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "echo one\n");
    assert_eq!(seen.last(), Some(&(9, 9)));
    assert_eq!(at.mode, 0o750);
    assert_eq!(
        at.sha256,
        Some(sha(b"echo one\n")),
        "fetch hashes what it read"
    );

    std::fs::write(&local, "echo two, longer\n").unwrap();
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Written { now, notes } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "echo two, longer\n"
    );
    assert_eq!(now.size, 17);
    assert_eq!(now.sha256, Some(sha(b"echo two, longer\n")), "W3 baseline");
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
        0o750
    );
    assert!(temps(&remote).is_empty(), "{:?}", temps(&remote));

    // The returned stat is the baseline for the next save (content check included).
    std::fs::write(&local, "echo three\n").unwrap();
    let again = fs
        .write_atomic(&local, p(&target), now.fingerprint())
        .unwrap();
    assert!(matches!(again, WriteOutcome::Written { .. }), "{again:?}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "echo three\n");

    // The saved copy is the new baseline: fetching again keeps nothing aside.
    fs.fetch(p(&target), &local, &mut |_, _| {}).unwrap();
    let names: Vec<_> = fs.list_dir(p(local.parent().unwrap())).unwrap();
    assert!(!names.iter().any(|n| n.contains(".conflict-")), "{names:?}");
}

#[test]
fn write_atomic_refuses_when_size_or_mtime_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "notes.txt", "v1");
    std::fs::write(&local, "my edit").unwrap();
    let mut fs = LocalFs::new();

    // Someone else rewrites it: same size, different mtime.
    std::fs::write(&target, "v2").unwrap();
    set_mtime(&target, 1_700_000_100);
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Conflict {
        now: Some(now),
        reason,
        ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(now.mtime, UNIX_EPOCH + Duration::from_secs(1_700_000_100));
    assert!(reason.starts_with("modified "), "{reason}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "my edit");

    // Same mtime, different size.
    std::fs::write(&target, "v2 plus").unwrap();
    set_mtime(&target, 1_700_000_000);
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert!(
        matches!(&outcome, WriteOutcome::Conflict { reason, .. } if reason == "size 2 → 7"),
        "{outcome:?}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2 plus");

    // Deleted since the fetch.
    std::fs::remove_file(&target).unwrap();
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert!(
        matches!(&outcome, WriteOutcome::Conflict { now: None, reason, .. } if reason == "it was deleted"),
        "{outcome:?}"
    );
    assert!(!target.exists());
    assert!(temps(tmp.path()).is_empty());
}

#[test]
fn w2_same_size_same_mtime_rewrite_is_a_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "notes.txt", "v1");
    std::fs::write(&local, "my edit").unwrap();
    // Rewritten within the same second, same length: only the content hash can tell.
    std::fs::write(&target, "v2").unwrap();
    set_mtime(&target, 1_700_000_000);
    let outcome = LocalFs::new()
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Conflict { reason, notes, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        reason,
        "its content changed (same size and modification time)"
    );
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
    assert!(temps(tmp.path()).is_empty(), "{:?}", temps(tmp.path()));

    // A baseline from a plain stat has no hash: size + mtime only (documented).
    let plain = LocalFs::new().stat(p(&target)).unwrap();
    assert_eq!(plain.sha256, None);
}

#[test]
fn w2_large_files_are_checked_by_stat_only_and_say_so() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("big.bin");
    let f = std::fs::File::create(&target).unwrap();
    f.set_len(HASH_CHECK_MAX + 1).unwrap(); // sparse: no data written
    drop(f);
    let mut fs = LocalFs::new();
    // A baseline carrying a hash that could never match: above the limit it is not consulted.
    let mut at = fs.stat(p(&target)).unwrap();
    at.sha256 = Some([0; 32]);
    let local = tmp.path().join("edit.bin");
    std::fs::write(&local, "small now").unwrap();
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Written { notes, .. } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("over 256 MiB"), "{notes:?}");
}

#[test]
fn w2_missing_attributes_refuse_the_write() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "edit").unwrap();
    for attr in ["size", "mtime", "permissions", "type", "owner"] {
        let mut fs = Hooked::new(Hooks {
            missing: Some(attr),
            ..Hooks::default()
        });
        let err = fs
            .write_atomic(&local, p(&target), at.fingerprint())
            .unwrap_err();
        assert!(
            format!("{err:#}").contains(&format!("does not report the {attr}")),
            "{err:#}"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
        assert!(temps(tmp.path()).is_empty());
    }
}

#[test]
fn w3_a_writer_right_after_the_rename_is_reported_and_poisons_the_baseline() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "mine").unwrap();
    let mut fs = Hooked::new(Hooks {
        grow_after_rename: true,
        ..Hooks::default()
    });
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Written { now, notes } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].starts_with("saved, but"), "{notes:?}");
    assert!(notes[0].contains("another writer?"), "{notes:?}");
    assert_eq!(now.sha256, Some(sha(b"mine")));

    // The next save from that baseline is refused, not silently layered over the other write.
    let mut plain = LocalFs::new();
    std::fs::write(&local, "mine, again").unwrap();
    let next = plain
        .write_atomic(&local, p(&target), now.fingerprint())
        .unwrap();
    assert!(matches!(next, WriteOutcome::Conflict { .. }), "{next:?}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "mine + another writer"
    );
}

#[test]
fn w4_owner_mismatch_is_fixed_or_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "edit").unwrap();

    let mut refuse = Hooked::new(Hooks {
        foreign_temp_owner: true,
        ..Hooks::default()
    });
    let err = refuse
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("is owned by uid"), "{msg}");
    assert!(msg.contains("was not replaced"), "{msg}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
    assert!(temps(tmp.path()).is_empty(), "{:?}", temps(tmp.path()));
    assert_eq!(refuse.renames, 0);

    let mut fixed = Hooked::new(Hooks {
        foreign_temp_owner: true,
        owner_fix_works: true,
        ..Hooks::default()
    });
    let outcome = fixed
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert!(
        matches!(outcome, WriteOutcome::Written { .. }),
        "{outcome:?}"
    );
    assert!(fixed.owner_fixed);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "edit");
}

#[test]
fn w5_a_target_that_became_a_symlink_or_directory_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "my edit").unwrap();
    // Same size and mtime behind the link: only lstat can tell.
    let elsewhere = tmp.path().join("elsewhere.txt");
    std::fs::rename(&target, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
    let outcome = LocalFs::new()
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert!(
        matches!(&outcome, WriteOutcome::Conflict { reason, .. } if reason == "it is now a symbolic link"),
        "{outcome:?}"
    );
    assert!(std::fs::symlink_metadata(&target)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "v1");

    std::fs::remove_file(&target).unwrap();
    std::fs::create_dir(&target).unwrap();
    let outcome = LocalFs::new()
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert!(
        matches!(&outcome, WriteOutcome::Conflict { reason, .. } if reason == "it is now a directory"),
        "{outcome:?}"
    );
    assert!(temps(tmp.path()).is_empty());
}

#[test]
fn write_atomic_removes_temp_on_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("a.txt");
    std::fs::write(&target, "original").unwrap();
    let local = tmp.path().join("edit.txt");
    std::fs::write(&local, "edited").unwrap();

    for (fail_rename, fail_create) in [(true, false), (false, true)] {
        let mut fs = Hooked::new(Hooks {
            fail_rename,
            fail_create_after_bytes: fail_create,
            ..Hooks::default()
        });
        let at = fs.stat(p(&target)).unwrap();
        let err = fs
            .write_atomic(&local, p(&target), at.fingerprint())
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("simulated"), "{msg}");
        assert!(msg.contains("was not replaced"), "{msg}");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert!(temps(tmp.path()).is_empty(), "{:?}", temps(tmp.path()));
    }

    // A local source that cannot be read: nothing staged, nothing left.
    let mut fs = LocalFs::new();
    let at = fs.stat(p(&target)).unwrap();
    let missing = tmp.path().join("missing.txt");
    assert!(fs
        .write_atomic(&missing, p(&target), at.fingerprint())
        .is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
    assert!(temps(tmp.path()).is_empty());
}

#[test]
fn w6_never_removes_a_temp_name_it_did_not_create() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("a.txt");
    std::fs::write(&target, "original").unwrap();
    let local = tmp.path().join("edit.txt");
    std::fs::write(&local, "edited").unwrap();
    let mut fs = Hooked::new(Hooks {
        take_temp_name: true,
        ..Hooks::default()
    });
    let at = fs.stat(p(&target)).unwrap();
    assert!(fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .is_err());
    let left = temps(tmp.path());
    assert_eq!(left.len(), 1, "someone else's file was removed");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(&left[0])).unwrap(),
        "theirs"
    );

    let dir = tempfile::tempdir().unwrap();
    let err = fs
        .write_new_sibling(&local, p(dir.path()), "out.mp3")
        .unwrap_err();
    assert!(format!("{err:#}").contains("cannot create"), "{err:#}");
    assert_eq!(
        temps(dir.path()).len(),
        1,
        "someone else's file was removed"
    );
}

#[test]
fn w6_a_temp_that_cannot_be_removed_is_named() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "edit").unwrap();

    // Conflict found after the upload (content changed): the leftover temp is in the notes.
    std::fs::write(&target, "v2").unwrap();
    set_mtime(&target, 1_700_000_000);
    let mut fs = Hooked::new(Hooks {
        fail_remove: true,
        ..Hooks::default()
    });
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Conflict { notes, .. } = outcome else {
        panic!("{outcome:?}")
    };
    let left = temps(tmp.path());
    assert_eq!(left.len(), 1);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains(&left[0]), "{notes:?}");
    std::fs::remove_file(tmp.path().join(&left[0])).unwrap();

    // A failed rename: the error names the temp it could not remove.
    let at = LocalFs::new().stat(p(&target)).unwrap();
    let mut fs = Hooked::new(Hooks {
        fail_remove: true,
        fail_rename: true,
        ..Hooks::default()
    });
    let err = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    let left = temps(tmp.path());
    assert_eq!(left.len(), 1);
    assert!(
        format!("{err:#}").contains(&format!("{} is left behind", left[0]))
            || format!("{err:#}").contains("is left behind"),
        "{err:#}"
    );
    assert!(format!("{err:#}").contains(&left[0]), "{err:#}");
    std::fs::remove_file(tmp.path().join(&left[0])).unwrap();

    // A published sibling whose temp stays behind is not reported as a clean success.
    let dir = tempfile::tempdir().unwrap();
    let err = fs
        .write_new_sibling(&local, p(dir.path()), "out.mp3")
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.starts_with("published "), "{msg}");
    assert!(msg.contains("could not be removed"), "{msg}");
    assert!(dir.path().join("out.mp3").exists());
}

#[test]
fn w7_a_rename_without_a_reply_is_outcome_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "edit").unwrap();
    let mut fs = Hooked::new(Hooks {
        rename_unknown: true,
        ..Hooks::default()
    });
    let err = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    assert!(is_outcome_unknown(&err));
    let msg = format!("{err:#}");
    assert!(msg.contains("outcome unknown"), "{msg}");
    assert!(!msg.contains("was not replaced"), "{msg}");
}

#[test]
fn write_new_sibling_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let local = dir.join("export.bin");
    std::fs::write(&local, "new").unwrap();
    std::fs::write(dir.join("t.cut-1-3.mp3"), "first").unwrap();
    std::fs::write(dir.join("t.cut-1-3-1.mp3"), "second").unwrap();
    let mut fs = LocalFs::new();

    let name = fs
        .write_new_sibling(&local, p(dir), "t.cut-1-3.mp3")
        .unwrap();
    assert_eq!(name, "t.cut-1-3-2.mp3");
    assert_eq!(std::fs::read_to_string(dir.join(&name)).unwrap(), "new");
    assert_eq!(
        std::fs::read_to_string(dir.join("t.cut-1-3.mp3")).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("t.cut-1-3-1.mp3")).unwrap(),
        "second"
    );

    let fresh = fs.write_new_sibling(&local, p(dir), "other.wav").unwrap();
    assert_eq!(fresh, "other.wav");
    assert!(temps(dir).is_empty(), "{:?}", temps(dir));
    assert!(fs.write_new_sibling(&local, p(dir), "a/b").is_err());
    assert!(fs.write_new_sibling(&local, p(dir), "..").is_err());
}

#[test]
fn w1_without_links_a_racing_creator_is_never_replaced() {
    for hooks in [
        Hooks {
            no_links: true,
            competitor: true,
            ..Hooks::default()
        },
        Hooks {
            cannot_link: true,
            competitor: true,
            ..Hooks::default()
        },
        Hooks {
            competitor: true,
            ..Hooks::default()
        },
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("export.bin");
        std::fs::write(&local, "mine").unwrap();
        let mut fs = Hooked::new(hooks);
        let name = fs
            .write_new_sibling(&local, p(tmp.path()), "t.cut-1-3.mp3")
            .unwrap();
        assert_eq!(name, "t.cut-1-3-1.mp3");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("t.cut-1-3.mp3")).unwrap(),
            "theirs",
            "a competitor's file was replaced"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(&name)).unwrap(),
            "mine"
        );
        assert_eq!(fs.renames, 0, "publishing never uses a replacing rename");
        assert!(temps(tmp.path()).is_empty(), "{:?}", temps(tmp.path()));
    }
}

#[test]
fn w1_concurrent_publishers_never_lose_a_file() {
    const THREADS: usize = 6;
    const EACH: usize = 8;
    for no_links in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let dir = dir.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut fs = Hooked::new(Hooks {
                        no_links,
                        ..Hooks::default()
                    });
                    let mut mine = Vec::new();
                    barrier.wait();
                    for i in 0..EACH {
                        let body = format!("writer {t} export {i}");
                        let local = dir.join(format!("src-{t}-{i}"));
                        std::fs::write(&local, &body).unwrap();
                        let name = fs
                            .write_new_sibling(&local, p(&dir), "clip.cut-1-3.mp4")
                            .unwrap();
                        mine.push((name, body));
                    }
                    mine
                })
            })
            .collect();
        let all: Vec<(String, String)> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let names: HashSet<&String> = all.iter().map(|(n, _)| n).collect();
        assert_eq!(
            names.len(),
            THREADS * EACH,
            "no_links={no_links}: a name was reused"
        );
        for (name, body) in &all {
            assert_eq!(
                &std::fs::read_to_string(dir.join(name)).unwrap(),
                body,
                "no_links={no_links}: {name} lost its content"
            );
        }
        assert!(temps(&dir).is_empty(), "{:?}", temps(&dir));
    }
}

#[test]
fn fetch_keeps_unsaved_edits_of_the_previous_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, _) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "unsaved edit").unwrap();
    std::fs::write(&target, "v2").unwrap();
    let at = LocalFs::new()
        .fetch(p(&target), &local, &mut |_, _| {})
        .unwrap();
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "v2");
    assert_eq!(at.sha256, Some(sha(b"v2")));
    let kept: Vec<_> = std::fs::read_dir(local.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains(".conflict-"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), "unsaved edit");
}

/// A fetched copy and the cache directories made for it are the owner's only (0600 / 0700),
/// whatever the umask lets through; so is an edit moved aside, even one an editor rewrote with
/// the umask's mode.
#[test]
fn fetched_copies_and_kept_edits_are_owner_only() {
    if crate::testutil::rerun_under_umask_022(
        "remote::tests::fetched_copies_and_kept_edits_are_owner_only",
    ) {
        return;
    }
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("a.txt");
    std::fs::write(&target, "v1").unwrap();
    let cache = tmp.path().join("cache");
    let local = cache.join("box/srv/a.txt");
    LocalFs::new()
        .fetch(p(&target), &local, &mut |_, _| {})
        .unwrap();
    assert_eq!(mode(&local), 0o600);
    for dir in local.parent().unwrap().ancestors().take(3) {
        assert_eq!(mode(dir), 0o700, "{dir:?}");
    }
    // The editor replaced the copy with a file of the umask's mode.
    std::fs::remove_file(&local).unwrap();
    std::fs::write(&local, "edit").unwrap();
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o644)).unwrap();
    let kept = crate::cache::set_aside(&local).unwrap();
    assert_eq!(mode(&kept), 0o600);
}

#[test]
fn fetch_of_a_directory_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fs = LocalFs::new();
    let err = fs
        .fetch(p(tmp.path()), &tmp.path().join("x"), &mut |_, _| {})
        .unwrap_err();
    assert!(format!("{err:#}").contains("is a directory"));
}

#[test]
fn local_detection_helpers() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("b"), "0123456789").unwrap();
    std::fs::write(tmp.path().join("a"), "").unwrap();
    std::os::unix::fs::symlink(tmp.path().join("b"), tmp.path().join("c")).unwrap();
    let mut fs = LocalFs::new();
    assert_eq!(fs.list_dir(p(tmp.path())).unwrap(), ["a", "b", "c"]);
    assert_eq!(fs.read_head(p(&tmp.path().join("b")), 4).unwrap(), b"0123");
    let real = fs.realpath(p(&tmp.path().join("b"))).unwrap();
    assert!(real.ends_with("/b"), "{real}");
    assert!(fs.stat_opt(p(&tmp.path().join("nope"))).unwrap().is_none());
    let link = fs.lstat_opt(p(&tmp.path().join("c"))).unwrap().unwrap();
    assert!(link.is_symlink && !link.is_file);
    let through = fs.stat_opt(p(&tmp.path().join("c"))).unwrap().unwrap();
    assert!(through.is_file && !through.is_symlink && through.uid.is_some());
}

#[test]
fn change_description() {
    let at = Fingerprint {
        size: 3,
        mtime: UNIX_EPOCH,
        sha256: None,
    };
    let now = Stat {
        is_dir: false,
        is_file: true,
        is_symlink: false,
        size: 5,
        mtime: UNIX_EPOCH,
        mode: 0o644,
        uid: Some(1),
        gid: Some(1),
        missing: None,
        sha256: None,
    };
    assert_eq!(describe_change(&at, &now), "size 3 → 5");
    let later = Stat {
        size: 3,
        mtime: UNIX_EPOCH + Duration::from_secs(60),
        ..now
    };
    assert_eq!(
        describe_change(&at, &later),
        "modified 1970-01-01T00:00:00Z → 1970-01-01T00:01:00Z"
    );
}

#[test]
fn path_helpers() {
    assert_eq!(split_parent("/a/b/c.txt"), ("/a/b", "c.txt"));
    assert_eq!(split_parent("/c"), ("/", "c"));
    assert_eq!(split_parent("c"), (".", "c"));
    assert_eq!(join("/", "c"), "/c");
    assert_eq!(join("/a", "c"), "/a/c");
    assert_eq!(suffixed("t.cut-1-3.mp3", 2), "t.cut-1-3-2.mp3");
    assert_eq!(suffixed("README", 1), "README-1");
    assert_eq!(suffixed(".bashrc", 1), ".bashrc-1");
    assert_ne!(random_hex(), random_hex());
    assert_eq!(random_hex().len(), 16);
    assert!(is_temp(".a.txt.sopen-0123456789abcdef"));
    assert!(!is_temp(".a.txt.sopen-baseline"));
}

/// Two saves above the content-check limit: another writer puts same-size bytes with another
/// mtime in place right after our rename. The baseline must be our staged temp, so the second
/// save is refused instead of layering over the other write.
#[test]
fn f2_large_file_baseline_comes_from_the_staged_temp() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "big.bin", "0123456789");
    std::fs::write(&local, "abcdefghij").unwrap();
    let mut first = Hooked::new(Hooks {
        hash_limit: Some(4),
        rewrite_after_rename: true,
        ..Hooks::default()
    });
    let outcome = first
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Written { now, notes } = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("saved, but the remote changed right after")),
        "{notes:?}"
    );
    std::fs::write(&local, "klmnopqrst").unwrap();
    let mut second = Hooked::new(Hooks {
        hash_limit: Some(4),
        ..Hooks::default()
    });
    let next = second
        .write_atomic(&local, p(&target), now.fingerprint())
        .unwrap();
    assert!(matches!(next, WriteOutcome::Conflict { .. }), "{next:?}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "xxxxxxxxxx");
}

/// The create reached the server (the temp exists) but its reply was lost.
#[test]
fn f2_a_lost_create_reply_is_named_not_forgotten() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "edit").unwrap();
    let mut fs = Hooked::new(Hooks {
        lost_reply: Some("create"),
        ..Hooks::default()
    });
    let err = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    assert!(is_outcome_unknown(&err), "{err:#}");
    let left = temps(tmp.path());
    assert_eq!(left.len(), 1, "the maybe-created temp is not removed");
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&format!(
            "outcome unknown — check {}",
            tmp.path().join(&left[0]).display()
        )),
        "{msg}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
}

/// The rename reached the server and landed, but its reply was lost.
#[test]
fn f2_a_lost_rename_reply_is_outcome_unknown_not_not_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "edit").unwrap();
    let mut fs = Hooked::new(Hooks {
        lost_reply: Some("rename"),
        ..Hooks::default()
    });
    let err = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    assert!(is_outcome_unknown(&err), "{err:#}");
    let top = outcome_unknown(&err).unwrap().to_string();
    assert!(
        top.ends_with(&format!("outcome unknown — check {}", p(&target))),
        "{top}"
    );
    assert!(!format!("{err:#}").contains("was not replaced"), "{err:#}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "edit",
        "it did land"
    );
}

/// The link reached the server and landed, but its reply was lost: no second copy under `-1`.
#[test]
fn f2_a_lost_link_reply_is_named_and_not_published_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("export.bin");
    std::fs::write(&local, "mine").unwrap();
    let dir = tmp.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let mut fs = Hooked::new(Hooks {
        lost_reply: Some("link"),
        ..Hooks::default()
    });
    let err = fs
        .write_new_sibling(&local, p(&dir), "clip.mp4")
        .unwrap_err();
    assert!(is_outcome_unknown(&err), "{err:#}");
    let top = outcome_unknown(&err).unwrap().to_string();
    assert!(
        top.ends_with(&format!("outcome unknown — check {}/clip.mp4", p(&dir))),
        "{top}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("clip.mp4")).unwrap(),
        "mine"
    );
    assert!(!dir.join("clip-1.mp4").exists());
    assert!(temps(&dir).is_empty(), "{:?}", temps(&dir));
}

/// Without links the export is created directly; a lost create reply names that file and
/// leaves it (it may be ours or a racing writer's).
#[test]
fn f2_a_lost_sibling_create_reply_names_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("export.bin");
    std::fs::write(&local, "mine").unwrap();
    let dir = tmp.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let mut fs = Hooked::new(Hooks {
        cannot_link: true,
        lost_reply: Some("create"),
        ..Hooks::default()
    });
    let err = fs
        .write_new_sibling(&local, p(&dir), "clip.mp4")
        .unwrap_err();
    assert!(is_outcome_unknown(&err), "{err:#}");
    let top = outcome_unknown(&err).unwrap().to_string();
    assert!(
        top.ends_with(&format!("outcome unknown — check {}/clip.mp4", p(&dir))),
        "{top}"
    );
    assert!(
        dir.join("clip.mp4").exists(),
        "not removed: it may not be ours"
    );
}

/// No posix-rename: a replace is impossible, so nothing is uploaded and Save copy is offered.
#[test]
fn f2_no_atomic_replace_refuses_before_uploading() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::write(&local, "edit").unwrap();
    let mut fs = Hooked::new(Hooks {
        cannot_replace: true,
        ..Hooks::default()
    });
    let err = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("posix-rename") && msg.contains("Save copy"),
        "{msg}"
    );
    assert_eq!(fs.renames, 0);
    assert!(temps(tmp.path()).is_empty(), "nothing was uploaded");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
    // Save copy (a new sibling) needs no replace.
    let name = fs
        .write_new_sibling(&local, p(tmp.path()), "a.edited.txt")
        .unwrap();
    assert_eq!(name, "a.edited.txt");
}
