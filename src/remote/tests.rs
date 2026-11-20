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
    /// Temps look owned by another uid; `owner_fix_works` decides whether giving them the
    /// target's owner (through the handle, in `create_from`) helps.
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
    /// Right after the temp is created and filled, someone with write access to the
    /// directory replaces it with a symlink to this file.
    temp_swapped_for_link_to: Option<PathBuf>,
    /// The target looks owned by another uid, so the backend is asked for an owner it cannot
    /// give (unless running as root): its real `fchown` is refused.
    foreign_target_owner: bool,
    /// The backend is asked to give the temp root's owner — refused (unless running as root)
    /// while the temp's owner already is the target's.
    owner_asked_as_root: bool,
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
            if !temp && self.h.foreign_target_owner {
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
    fn list_entries(&mut self, path: &str) -> Result<Vec<Entry>> {
        self.inner.list_entries(path)
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
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        if like.is_some() && self.h.foreign_temp_owner && self.h.owner_fix_works {
            self.owner_fixed = true;
        }
        if self.h.take_temp_name && is_temp(path.rsplit('/').next().unwrap()) {
            std::fs::write(path, "theirs").unwrap();
        }
        self.race(path);
        let like = like.map(|l| match self.h.owner_asked_as_root {
            true => Like {
                uid: 0,
                gid: 0,
                ..l
            },
            false => l,
        });
        let up = self.inner.create_from(local, path, like)?;
        if let Some(victim) = &self.h.temp_swapped_for_link_to {
            std::fs::remove_file(path).unwrap();
            std::os::unix::fs::symlink(victim, path).unwrap();
        }
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
    // (That the saved copy is then recorded, so the next fetch keeps nothing aside, is the
    // cache's part: `main::tests::a_saved_copy_is_recorded_so_the_next_fetch_keeps_nothing_aside`.)
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

/// Writing to a file clears its set-user-ID bit (for a non-root writer): the mode is finalized
/// through the still-open handle after the content is written, so a setuid target keeps it.
#[test]
fn a_set_id_bit_cleared_by_writing_is_set_again_through_the_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "tool", "v1");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o4755)).unwrap();
    std::fs::write(&local, "v2").unwrap();
    let mut fs = LocalFs::new();
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Written { now, notes } = outcome else {
        panic!("expected Written, got {outcome:?}")
    };
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(now.mode, 0o4755);
    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o4755);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
}

/// Backend level (LocalFs's real `fchown`): an owner change the system refuses leaves the temp
/// owned by this account while the target is someone else's — the save is refused, nothing
/// replaced. (Root's chown is never refused, so there this case cannot be made.)
#[test]
fn a_refused_owner_change_that_leaves_a_mismatch_refuses_the_save() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    if std::fs::metadata(&target).unwrap().uid() == 0 {
        eprintln!("skipping: root's chown is never refused");
        return;
    }
    std::fs::write(&local, "edit").unwrap();
    let mut fs = Hooked::new(Hooks {
        foreign_target_owner: true,
        ..Hooks::default()
    });
    let err = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("is owned by uid"), "{msg}");
    assert!(msg.contains("was not replaced"), "{msg}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
    assert!(temps(tmp.path()).is_empty(), "{:?}", temps(tmp.path()));
    assert_eq!(fs.renames, 0);
}

/// Backend level: an owner change that fails while the temp's owner already is the target's
/// (here: asked for root's, refused for a non-root account) does not stop the save.
#[test]
fn a_failed_owner_change_with_the_owner_already_right_lets_the_save_proceed() {
    use std::os::unix::fs::MetadataExt;
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    if std::fs::metadata(&target).unwrap().uid() == 0 {
        eprintln!("skipping: root's chown is never refused");
        return;
    }
    std::fs::write(&local, "edit").unwrap();
    let mut fs = Hooked::new(Hooks {
        owner_asked_as_root: true,
        ..Hooks::default()
    });
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert!(
        matches!(outcome, WriteOutcome::Written { .. }),
        "{outcome:?}"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "edit");
}

/// Owner and mode are given to the temp through its open handle, never by path afterwards: a
/// temp swapped for a symlink right after it was written is refused, and the file the link
/// points at keeps its mode.
#[test]
fn a_temp_swapped_for_a_link_is_never_chmodded_through() {
    let tmp = tempfile::tempdir().unwrap();
    let (target, local, at) = fetched(tmp.path(), "a.txt", "v1");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    let at = LocalFs::new()
        .stat(p(&target))
        .map(|s| Stat {
            sha256: at.sha256,
            ..s
        })
        .unwrap();
    std::fs::write(&local, "edit").unwrap();
    let victim = tmp.path().join("victim");
    std::fs::write(&victim, "someone's").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut fs = Hooked::new(Hooks {
        temp_swapped_for_link_to: Some(victim.clone()),
        ..Hooks::default()
    });
    let err = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("was not replaced"), "{msg}");
    let mode = std::fs::metadata(&victim).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "chmodded through the link");
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "someone's");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1");
    assert_eq!(fs.renames, 0);
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

/// A folder with one entry of each kind a listing must tell apart: a folder, a file, a hidden
/// file, links to a folder and to a file, a broken link and a fifo. Whole-second mtimes (SFTP
/// v3 carries seconds).
fn listing_fixture(dir: &Path) {
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "abc").unwrap();
    set_mtime(&dir.join("a.txt"), 1_700_000_000);
    std::fs::write(dir.join(".hidden"), "12345").unwrap();
    set_mtime(&dir.join(".hidden"), 1_700_000_100);
    std::os::unix::fs::symlink("sub", dir.join("to-sub")).unwrap();
    std::os::unix::fs::symlink("a.txt", dir.join("to-a")).unwrap();
    std::os::unix::fs::symlink("nowhere", dir.join("broken")).unwrap();
    let fifo = std::ffi::CString::new(p(&dir.join("pipe"))).unwrap();
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
}

/// What the browser lists for [`listing_fixture`]: each entry's kind (a link's target's), its
/// size and mtime (a link's target's too), and whether it is a link.
fn check_listing(fs: &mut dyn RemoteFs, dir: &Path) {
    let entries = fs.list_entries(p(dir)).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [".hidden", "a.txt", "broken", "pipe", "sub", "to-a", "to-sub"]
    );
    let by = |name: &str| entries.iter().find(|e| e.name == name).unwrap().clone();
    let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
    let a = by("a.txt");
    assert_eq!(
        (a.kind, a.link, a.size, a.mtime),
        (EntryKind::File, false, 3, at(1_700_000_000))
    );
    let hidden = by(".hidden");
    assert_eq!(
        (hidden.kind, hidden.size, hidden.mtime),
        (EntryKind::File, 5, at(1_700_000_100))
    );
    assert_eq!((by("sub").kind, by("sub").link), (EntryKind::Dir, false));
    assert_eq!(
        (by("to-sub").kind, by("to-sub").link),
        (EntryKind::Dir, true)
    );
    let to_a = by("to-a");
    assert_eq!(
        (to_a.kind, to_a.link, to_a.size, to_a.mtime),
        (EntryKind::File, true, 3, at(1_700_000_000)),
        "a link shows what it points at"
    );
    assert_eq!(
        (by("broken").kind, by("broken").link),
        (EntryKind::Broken, true)
    );
    assert_eq!(
        (by("pipe").kind, by("pipe").link),
        (EntryKind::Special, false)
    );
    let free = fs.free_space(p(dir));
    assert!(free.is_some_and(|b| b > 0), "{free:?}");
}

#[test]
fn local_listing_carries_kind_size_and_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    listing_fixture(tmp.path());
    check_listing(&mut LocalFs::new(), tmp.path());
}

/// Over a real SFTP server (OpenSSH's, as a local child): the attributes come with the readdir
/// reply — one stat request per symbolic link (what it points at), none per name (counted where
/// sopen sends them).
#[test]
fn sftp_listing_carries_the_attributes_without_a_stat_per_name() {
    let Some(mut fs) = SftpRemote::local_server() else {
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    listing_fixture(tmp.path());
    check_listing(&mut fs, tmp.path());
    assert_eq!(
        fs.stats(),
        3,
        "one stat per link (to-sub, to-a, broken), none per name"
    );
}

/// A folder that may be read but not searched: its names list, their attributes cannot be
/// read — the listing still comes back, each such entry marked unknown.
#[test]
fn a_folder_read_but_not_searched_still_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("locked");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("x"), "1").unwrap();
    use std::os::unix::fs::MetadataExt;
    if std::fs::metadata(&dir).unwrap().uid() == 0 {
        eprintln!("skipping: root ignores directory permissions");
        return;
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o444)).unwrap();
    let listed = LocalFs::new().list_entries(p(&dir));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let listed = listed.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].name.as_str(), listed[0].kind),
        ("x", EntryKind::Unknown)
    );
}

/// A fetch stopped mid-transfer (the browser's Esc): it ends at the next write with
/// [`Cancelled`], leaves no partial copy, and the copy already at `local` is as it was.
#[test]
fn a_cancelled_fetch_keeps_nothing_and_leaves_the_copy_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("big.bin");
    std::fs::write(&remote, vec![7u8; 3 << 20]).unwrap();
    let local = tmp.path().join("cache/big.bin");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, "the earlier copy").unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut calls = 0;
    let err = LocalFs::new()
        .fetch_cancellable(
            p(&remote),
            &local,
            &mut |_, _| {
                calls += 1;
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            },
            &cancel,
        )
        .unwrap_err();
    assert!(is_cancelled(&err), "{err:#}");
    assert_eq!(calls, 1, "it stopped at the first write after the flag");
    assert_eq!(std::fs::read(&local).unwrap(), b"the earlier copy");
    let left: Vec<_> = std::fs::read_dir(local.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["big.bin"], "no partial copy left");
}

/// What a cancelled fetch must leave: the earlier copy as it was, and nothing else in its
/// folder.
fn earlier_copy_untouched(local: &Path) {
    assert_eq!(std::fs::read(local).unwrap(), b"the earlier copy");
    let left: Vec<_> = std::fs::read_dir(local.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "no partial copy left: {left:?}");
}

/// A fetch cancelled before it starts, of a file with no bytes (so no write ever sees the
/// flag): nothing is published over the earlier copy.
#[test]
fn a_fetch_cancelled_before_it_starts_publishes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("empty.txt");
    std::fs::write(&remote, b"").unwrap();
    let local = tmp.path().join("cache/empty.txt");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, "the earlier copy").unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let err = LocalFs::new()
        .fetch_cancellable(p(&remote), &local, &mut |_, _| {}, &cancel)
        .unwrap_err();
    assert!(is_cancelled(&err), "{err:#}");
    earlier_copy_untouched(&local);
}

/// A cancel that lands after the last write (here from the progress report of the only chunk):
/// the complete copy is not published either.
#[test]
fn a_fetch_cancelled_after_its_last_write_publishes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("small.txt");
    std::fs::write(&remote, b"one chunk of text").unwrap();
    let local = tmp.path().join("cache/small.txt");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, "the earlier copy").unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let err = LocalFs::new()
        .fetch_cancellable(
            p(&remote),
            &local,
            &mut |done, total| {
                if done == total {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
            &cancel,
        )
        .unwrap_err();
    assert!(is_cancelled(&err), "{err:#}");
    earlier_copy_untouched(&local);
}

/// Attributes for the listing seam's tests: a regular file, a folder, a link, and an entry
/// whose type the server left out.
fn attrs(kind: &str, size: u64) -> Stat {
    Stat {
        is_dir: kind == "dir",
        is_file: kind == "file",
        is_symlink: kind == "link",
        size,
        mtime: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        mode: 0o644,
        uid: Some(1),
        gid: Some(1),
        missing: (kind == "untyped").then_some("type"),
        sha256: None,
    }
}

/// A name that is not UTF-8 and a name that shows the same (a literal U+FFFD): each entry's
/// attributes come from its own bytes, never from the name as shown — and the first is marked
/// lossy (the browser does not open it).
#[test]
fn names_that_show_alike_keep_their_own_attributes() {
    use std::os::unix::ffi::OsStrExt;
    let bad = OsStr::from_bytes(b"a\xff").to_os_string();
    let alike = OsString::from("a\u{FFFD}");
    let listed: Listed = vec![
        (bad.clone(), Some(attrs("link", 1))),
        (alike.clone(), Some(attrs("link", 1))),
    ];
    let mut asked = Vec::new();
    let entries = entries_from(listed, &mut |name, follow| {
        asked.push((name.to_os_string(), follow));
        Ok(Some(if name == bad.as_os_str() {
            attrs("file", 111)
        } else if name == alike.as_os_str() {
            attrs("dir", 222)
        } else {
            panic!("looked up by a name no entry has: {name:?}")
        }))
    });
    let of = |lossy: bool| entries.iter().find(|e| e.lossy == lossy).unwrap();
    assert_eq!(of(true).name, "a\u{FFFD}");
    assert_eq!((of(true).kind, of(true).size), (EntryKind::File, 111));
    assert_eq!((of(false).kind, of(false).size), (EntryKind::Dir, 222));
    assert_eq!(asked, [(bad, true), (alike, true)]);
}

/// Entries whose type the server left out: their own type comes from an `lstat` (a link stays
/// a link: to a file, to a folder, or broken), and only a confirmed link is followed — two stats
/// for a link, one for anything else.
#[test]
fn untyped_entries_are_lstat_first_and_followed_only_when_links() {
    let listed: Listed = ["to-file", "to-dir", "dangling", "plain"]
        .iter()
        .map(|n| (OsString::from(n), Some(attrs("untyped", 0))))
        .collect();
    let mut asked = Vec::new();
    let entries = entries_from(listed, &mut |name, follow| {
        asked.push((name.to_string_lossy().into_owned(), follow));
        Ok(match (name.to_str().unwrap(), follow) {
            ("plain", false) => Some(attrs("file", 5)),
            (_, false) => Some(attrs("link", 9)),
            ("to-file", true) => Some(attrs("file", 7)),
            ("to-dir", true) => Some(attrs("dir", 64)),
            ("dangling", true) => None,
            (other, true) => panic!("{other} is not a link: never followed"),
        })
    });
    let by = |n: &str| entries.iter().find(|e| e.name == n).unwrap().clone();
    assert_eq!(
        (by("to-file").kind, by("to-file").link, by("to-file").size),
        (EntryKind::File, true, 7)
    );
    assert_eq!(
        (by("to-dir").kind, by("to-dir").link),
        (EntryKind::Dir, true)
    );
    assert_eq!(
        (by("dangling").kind, by("dangling").link),
        (EntryKind::Broken, true)
    );
    assert_eq!(
        (by("plain").kind, by("plain").link, by("plain").size),
        (EntryKind::File, false, 5)
    );
    let follows = asked.iter().filter(|(_, f)| *f).count();
    assert_eq!((asked.len(), follows), (7, 3), "{asked:?}");
}

/// On a filesystem that takes any bytes (Linux; macOS refuses names that are not UTF-8): a
/// file named `a\xff` and one named `a\u{FFFD}` list as two entries with their own sizes.
#[test]
fn local_names_that_are_not_utf8_list_with_their_own_attributes() {
    use std::os::unix::ffi::OsStrExt;
    let tmp = tempfile::tempdir().unwrap();
    let bad = tmp.path().join(OsStr::from_bytes(b"a\xff"));
    if let Err(e) = std::fs::write(&bad, "1") {
        eprintln!("skipping: this filesystem refuses a name that is not UTF-8 ({e})");
        return;
    }
    std::fs::write(tmp.path().join("a\u{FFFD}"), "22").unwrap();
    let entries = LocalFs::new().list_entries(p(tmp.path())).unwrap();
    assert_eq!(entries.len(), 2);
    let of = |lossy: bool| entries.iter().find(|e| e.lossy == lossy).unwrap();
    assert_eq!((of(true).name.as_str(), of(true).size), ("a\u{FFFD}", 1));
    assert_eq!((of(false).name.as_str(), of(false).size), ("a\u{FFFD}", 2));
}

// ── Streaming transport (06 §2) ─────────────────────────────────────────────────────────────

use super::ranged::{Pipeline, PIPE_IN_FLIGHT, PIPE_REQUEST};
use super::sftp::{Proxy, SftpRanged, Timing, Wired};
use bytes::Bytes;

/// `len` bytes in which every 8-byte word holds its own offset (the Fake's pattern), so a read
/// placed at the wrong offset, or bytes skipped, cannot match.
fn patterned(len: usize) -> Vec<u8> {
    let bytes = pattern(0, len);
    debug_assert!(bytes
        .iter()
        .take(4096)
        .enumerate()
        .all(|(i, b)| *b == pattern_byte(i as u64)));
    bytes
}

/// The clipping proxy of 06 §2: every DATA reply cut to 20 000 bytes.
fn clipping() -> Proxy {
    Proxy {
        clip: Some(20_000),
        ..Default::default()
    }
}

/// A 4 MiB patterned file in a new temp dir.
fn four_mib() -> (tempfile::TempDir, PathBuf, Vec<u8>) {
    let tmp = tempfile::tempdir().unwrap();
    let body = patterned(4 << 20);
    let path = tmp.path().join("four.bin");
    std::fs::write(&path, &body).unwrap();
    (tmp, path, body)
}

/// The next completion, which must come.
fn next(r: &mut dyn Ranged) -> Completion {
    r.next(Some(Duration::from_secs(10))).expect("a completion")
}

fn stat_of(c: &Completion) -> Stat {
    match &c.reply {
        Ok(Reply::Stat(Some(s))) => *s,
        other => panic!("not a stat: {other:?}"),
    }
}

/// What every whole-file read must give for a 4 MiB file: the file's own bytes. Run over the
/// plain local server (the control) and through the clipping proxy (V5).
fn whole_reads_match(fs: &mut SftpRemote, dir: &Path, body: &[u8]) {
    let path = dir.join("four.bin");
    let mut got = Vec::new();
    let n = fs.read_to(p(&path), &mut got, &mut |_| {}).unwrap();
    assert_eq!(
        (n as usize, got.len()),
        (body.len(), body.len()),
        "read_to's length"
    );
    assert!(got == body, "read_to's bytes");
    let head = fs.read_head(p(&path), body.len()).unwrap();
    assert_eq!(head.len(), body.len(), "read_head's length");
    assert!(head == body, "read_head's bytes");
    let part = fs.read_head(p(&path), 100_000).unwrap();
    assert!(part == body[..100_000], "read_head's first 100 000 bytes");
    // The kind sniff: 8 KiB, under the proxy's 20 000 — it fits in one reply either way.
    let sniff = fs.read_head(p(&path), crate::kind::SNIFF_BYTES).unwrap();
    assert!(
        sniff == body[..crate::kind::SNIFF_BYTES],
        "the sniff's bytes"
    );
    let local = dir.join("cache/four.bin");
    let at = fs.fetch(p(&path), &local, &mut |_, _| {}).unwrap();
    assert_eq!(at.sha256, Some(sha(body)), "fetch's hash");
    assert!(std::fs::read(&local).unwrap() == body, "fetch's copy");
    std::fs::remove_file(&local).unwrap();
}

/// V5: a server that answers a read with fewer bytes than asked (the proxy cuts every DATA to
/// 20 000) still gives every whole-file read the file's exact bytes: each read carries its own
/// offset and a short reply is followed by a read of the rest. 3a426b6's loop, which moved the
/// crate's cursor by the length asked, gets 16 × 20 000 bytes + the tail there.
#[test]
fn v5_whole_reads_through_the_clipping_proxy_give_the_files_bytes() {
    let Some(mut plain) = SftpRemote::local_server() else {
        return;
    };
    let (tmp, path, body) = four_mib();
    whole_reads_match(&mut plain, tmp.path(), &body);
    assert!(plain.read_through_cursor(p(&path)).unwrap() == body);
    let mut clipped = SftpRemote::local_server_with(Some(clipping())).unwrap();
    whole_reads_match(&mut clipped, tmp.path(), &body);
    let old = clipped.read_through_cursor(p(&path)).unwrap();
    assert_eq!(old.len(), 16 * 20_000 + 16_384, "3a426b6's read skips");
}

/// V5, the transport: reads queued together, each through its own clone of the handle at its own
/// offset, come back with the bytes of that offset — 20 000 of them through the proxy — and a
/// read of the rest at its own offset completes the block. They leave in the order queued.
#[test]
fn v5_ranged_reads_carry_their_own_offsets_through_the_clipping_proxy() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, path, body) = four_mib();
    let proxy = clipping();
    let fs = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
    let mut r = Box::new(fs).into_ranged(p(&path)).unwrap();
    assert_eq!(r.size(), body.len() as u64);
    proxy.wire.lock().unwrap().clear();
    let size = body.len() as u64;
    let offsets = [3 << 20, 0, (1 << 20) + 7, 0, size - 100];
    let block = 64 * 1024u32;
    let tickets: Vec<Ticket> = offsets.iter().map(|&o| r.read(o, block)).collect();
    for (&off, ticket) in offsets.iter().zip(&tickets) {
        let c = next(r.as_mut());
        assert_eq!(c.ticket, *ticket, "completions come in the order queued");
        let want = (block as u64).min(size - off).min(20_000) as usize;
        let got = c.data().unwrap();
        assert!(
            got[..] == body[off as usize..off as usize + want],
            "bytes at {off}"
        );
        assert!(!c.early_end());
    }
    let sent: Vec<u64> = proxy
        .wire
        .lock()
        .unwrap()
        .iter()
        .filter_map(|w| w.read.map(|(off, _)| off))
        .collect();
    assert_eq!(sent, offsets, "on the wire in the order queued");
    // A block read whole by following each short reply with the rest, at its own offset.
    let off = (2 << 20) + 3;
    let mut whole = Vec::new();
    while whole.len() < block as usize {
        let at = off + whole.len() as u64;
        r.read(at, block - whole.len() as u32);
        let c = next(r.as_mut());
        let got = c.data().unwrap();
        assert!(!got.is_empty());
        whole.extend_from_slice(got);
    }
    assert!(whole[..] == body[off as usize..off as usize + block as usize]);
}

/// V5, the early end on the real server: a reply short of the block, the file cut in the middle
/// of the block, then the rest: what lies before the cut comes, and a read past it gets nothing
/// (`SSH_FX_EOF`) — an early end, which the stream turns into `Changed`. The handle's `fstat` then
/// shows the new size.
#[test]
fn v5_a_short_reply_then_a_cut_ends_the_block_early_on_the_local_server() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, path, body) = four_mib();
    let fs = SftpRemote::local_server_with(Some(clipping())).unwrap();
    let mut r = Box::new(fs).into_ranged(p(&path)).unwrap();
    r.read(0, 65_536);
    assert_eq!(next(r.as_mut()).data().unwrap().len(), 20_000);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(30_000)
        .unwrap();
    r.read(20_000, 45_536);
    let c = next(r.as_mut());
    assert!(
        c.data().unwrap()[..] == body[20_000..30_000],
        "the rest up to the cut"
    );
    r.read(30_000, 35_536);
    let c = next(r.as_mut());
    assert!(c.early_end(), "{c:?}");
    assert_eq!(
        c.asked,
        Asked::Read {
            off: 30_000,
            len: 35_536
        }
    );
    r.fstat();
    assert_eq!(stat_of(&next(r.as_mut())).size, 30_000);
}

/// V5 on the in-order Fake: the same short reply and early end, with the file changed in place.
#[test]
fn v5_on_the_fake_a_short_reply_then_a_cut_ends_the_block_early() {
    let link = Link {
        clip: Some(20_000),
        ..Link::gate()
    };
    let (mut r, control) = fake("/m.bin", FakeFile::pattern(1 << 20), link);
    let before = r.opened();
    r.read(0, 65_536);
    let c = next(r.as_mut());
    assert!(c.data().unwrap()[..] == patterned(20_000)[..]);
    control.change(Change::Truncate(30_000));
    r.read(20_000, 45_536);
    assert_eq!(next(r.as_mut()).data().unwrap().len(), 10_000);
    r.read(30_000, 35_536);
    assert!(next(r.as_mut()).early_end());
    r.fstat();
    let now = stat_of(&next(r.as_mut()));
    assert_eq!(now.size, 30_000);
    assert!(now.mtime > before.mtime, "the cut moved the mtime");
}

/// 06 §2 (V5): on the in-order Fake every validation `fstat` arrives after each read queued
/// before it: requests reach the server in the order queued and come back in that order.
#[test]
fn v5_on_the_fake_every_fence_arrives_after_the_reads_it_validates() {
    let (mut r, control) = fake("/m.bin", FakeFile::pattern(8 << 20), Link::gate());
    let mut queued = Vec::new();
    for batch in [8u64, 3] {
        for i in 0..batch {
            queued.push(r.read(i * 65_536, 65_536));
        }
        queued.push(r.fstat());
    }
    let sent: Vec<Ticket> = control.sent().iter().map(|s| s.ticket).collect();
    assert_eq!(sent, queued, "received in the order queued");
    let mut reads_done = 0;
    for (i, t) in queued.iter().enumerate() {
        let c = next(r.as_mut());
        assert_eq!(c.ticket, *t);
        match c.asked {
            Asked::Read { .. } => reads_done += 1,
            Asked::Fstat => {
                let reads_before = queued[..i].len() - (i >= 9) as usize;
                assert_eq!(reads_done, reads_before, "the fence after its reads");
            }
            Asked::StatPath => unreachable!(),
        }
    }
    assert_eq!(r.pending(), 0);
}

/// 06 §2: over SFTP the transport writes every request from one task, in the order queued — the
/// wire, as the proxy saw it, has the reads at their offsets, then the fence, then the stat.
#[test]
fn sftp_requests_leave_in_the_order_queued() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, path, _) = four_mib();
    let proxy = Proxy::default();
    let fs = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
    let mut r = Box::new(fs).into_ranged(p(&path)).unwrap();
    proxy.wire.lock().unwrap().clear();
    let offsets = [7 << 18, 1 << 20, 0, 3 << 20, 5 << 16, 2 << 20, 9, 6 << 18];
    for o in offsets {
        r.read(o, 65_536);
    }
    r.fstat();
    r.stat_path();
    for _ in 0..offsets.len() + 2 {
        next(r.as_mut()).reply.unwrap();
    }
    let wire: Vec<(u8, Option<u64>)> = proxy
        .wire
        .lock()
        .unwrap()
        .iter()
        .filter(|w| !w.reply)
        .map(|w| (w.kind, w.read.map(|(o, _)| o)))
        .collect();
    let mut want: Vec<(u8, Option<u64>)> =
        offsets.iter().map(|&o| (Wired::READ, Some(o))).collect();
    want.extend([(Wired::FSTAT, None), (Wired::STAT, None)]);
    assert_eq!(wire, want);
}

/// V6: on the Fake (20 ms, 5 MB/s, simulated time), `read_to`'s plan reads 64 MiB in at most ⅔
/// of the time the same requests take one at a time. Its window starts at one request, grows by
/// one per reply, and never has more than 4 MiB out — and reaches it.
#[test]
fn v6_pipelined_whole_read_on_the_fake() {
    let size = 64u64 << 20;
    let (mut r, control): (Box<dyn Ranged>, FakeControl) =
        fake("/big.bin", FakeFile::pattern(size), Link::gate());
    let mut out = Hashing::new(std::io::sink());
    assert_eq!(read_to_over(r.as_mut(), &mut out).unwrap(), size);
    let pipelined = control.elapsed();
    assert_eq!(out.finish().sha256, sha(&patterned(size as usize)));
    let sent: Vec<SentRecord> = control.sent();
    let mut most = 0;
    for (n, s) in sent.iter().enumerate() {
        let Asked::Read { len, .. } = s.asked else {
            panic!()
        };
        most = most.max(s.ahead + len as u64);
        assert!(
            s.ahead / PIPE_REQUEST as u64 <= n as u64 / 2,
            "request {n} went out with {} bytes ahead: the window grows by one per reply",
            s.ahead
        );
    }
    assert_eq!(
        sent[1].ahead, 0,
        "the second request waits for the first reply"
    );
    assert_eq!(most, PIPE_IN_FLIGHT, "up to 4 MiB in flight, and no more");

    let (mut one, control) = fake("/big.bin", FakeFile::pattern(size), Link::gate());
    for off in (0..size).step_by(PIPE_REQUEST as usize) {
        one.read(off, PIPE_REQUEST);
        next(one.as_mut()).reply.unwrap();
    }
    let sequential = control.elapsed();
    assert!(
        pipelined.as_secs_f64() <= sequential.as_secs_f64() * 2.0 / 3.0,
        "pipelined {pipelined:?} against one at a time {sequential:?}"
    );
}

/// 06 §2 over SFTP: `read_to`'s requests, as the proxy saw them, grow from one in flight by one
/// per reply and never have more than 4 MiB out.
#[test]
fn sftp_read_to_is_pipelined_within_its_bound() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("eight.bin");
    std::fs::write(&path, patterned(8 << 20)).unwrap();
    let proxy = Proxy::default();
    let mut fs = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
    proxy.wire.lock().unwrap().clear();
    let mut out = Hashing::new(std::io::sink());
    fs.read_to(p(&path), &mut out, &mut |_| {}).unwrap();
    assert_eq!(out.finish().size, 8 << 20);
    let wire = proxy.wire.lock().unwrap().clone();
    let first = wire.iter().position(|w| w.kind == Wired::READ).unwrap();
    let (mut reads, mut answered, mut most) = (0u64, 0u64, 0u64);
    for w in &wire[first..] {
        match (w.reply, w.kind) {
            (false, Wired::READ) => {
                let out = reads - answered;
                assert!(out <= reads / 2, "read {reads} went out with {out} ahead");
                reads += 1;
                most = most.max((reads - answered) * PIPE_REQUEST as u64);
            }
            (true, Wired::DATA | Wired::STATUS) => answered += 1,
            _ => {}
        }
    }
    assert!(most <= PIPE_IN_FLIGHT, "{most}");
    assert!(
        most > PIPE_REQUEST as u64,
        "more than one request in flight"
    );
}

/// The plan alone: the window opens one request per reply; a short reply's rest goes next and
/// is delivered in order; a server's limit is learned; an empty reply ends the file; a reply
/// longer than asked, or for nothing asked, is an error.
#[test]
fn the_pipeline_plan() {
    let block = PIPE_REQUEST;
    let mut plan = Pipeline::new(None);
    assert_eq!(plan.send(), Some((0, block)));
    assert_eq!(plan.send(), None, "one request until the first reply");
    plan.reply(0, Some(patterned(block as usize).into()))
        .unwrap();
    assert_eq!(plan.take().map(|b| b.len()), Some(block as usize));
    assert_eq!(plan.send(), Some((block as u64, block)));
    assert_eq!(plan.send(), Some((2 * block as u64, block)));
    assert_eq!(plan.send(), None, "two after one reply");
    // A short reply: its rest is asked next and delivered before the bytes after it.
    plan.reply(2 * block as u64, Some(vec![2; 1000].into()))
        .unwrap();
    plan.reply(block as u64, Some(vec![1; 20_000].into()))
        .unwrap();
    assert_eq!(plan.take().map(|b| b.len()), Some(20_000));
    assert_eq!(plan.take(), None, "the rest of the short reply comes first");
    assert_eq!(plan.send(), Some((block as u64 + 20_000, block - 20_000)));
    // 20 000 bytes mid-file is the server's limit from now on.
    assert_eq!(plan.send(), Some((2 * block as u64 + 1000, block - 1000)));
    assert_eq!(plan.send(), Some((3 * block as u64, 20_000)));
    plan.reply(
        block as u64 + 20_000,
        Some(vec![1; (block - 20_000) as usize].into()),
    )
    .unwrap();
    assert_eq!(
        plan.take().map(|b| b.len()),
        Some((block - 20_000) as usize)
    );
    assert_eq!(plan.take().map(|b| b.len()), Some(1000));
    plan.reply(2 * block as u64 + 1000, Some(Bytes::new()))
        .unwrap();
    assert!(plan.done(), "an empty reply ends the file");
    assert_eq!(plan.send(), None);
    assert!(plan
        .reply(3 * block as u64, Some(vec![0; 30_000].into()))
        .is_err());
    assert!(plan.reply(99, Some(Bytes::new())).is_err());

    // A read of at most `end` bytes asks exactly up to it.
    let mut head = Pipeline::new(Some(70_000));
    assert_eq!(head.send(), Some((0, block)));
    head.reply(0, Some(vec![0; block as usize].into())).unwrap();
    assert_eq!(head.send(), Some((block as u64, 70_000 - block)));
    assert_eq!(head.send(), None);
    head.take();
    head.reply(
        block as u64,
        Some(vec![0; (70_000 - block) as usize].into()),
    )
    .unwrap();
    head.take();
    assert!(head.done());
}

/// `into_ranged` gives the connection back when the open fails: a missing file, a folder, and a
/// connection that does not stream (the provided default). The connection works afterwards.
#[test]
fn into_ranged_gives_the_connection_back_on_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, "abc").unwrap();
    let missing = tmp.path().join("missing");
    let works = |conn: &mut Box<dyn RemoteFs + Send>| {
        assert_eq!(conn.stat(p(&file)).unwrap().size, 3);
        let mut out = Vec::new();
        conn.read_to(p(&file), &mut out, &mut |_| {}).unwrap();
        assert_eq!(out, b"abc");
    };
    let refused = |conn: Box<dyn RemoteFs + Send>, path: &Path, why: &str| {
        let e = conn.into_ranged(p(path)).unwrap_err();
        assert!(format!("{:#}", e.error).contains(why), "{:#}", e.error);
        e.conn
    };
    let hooked: Box<dyn RemoteFs + Send> = Box::new(Hooked::new(Hooks::default()));
    works(&mut refused(hooked, &file, "does not stream"));
    let local: Box<dyn RemoteFs + Send> = Box::new(LocalFs::new());
    works(&mut refused(local, &missing, "cannot open"));
    let Some(sftp) = SftpRemote::local_server() else {
        return;
    };
    let mut conn = refused(Box::new(sftp), &missing, "cannot open");
    works(&mut conn);
    let mut conn = refused(conn, tmp.path(), "not a regular file");
    works(&mut conn);
    let mut r = conn.into_ranged(p(&file)).unwrap();
    assert_eq!(r.opened().size, 3);
    r.read(0, 10);
    assert_eq!(&next(r.as_mut()).data().unwrap()[..], b"abc");
}

/// `reopen` closes the handle and opens another file on the same session; `into_connection`
/// closes the handle and gives the session back, which works as before. Each close is sent.
#[test]
fn reopen_and_into_connection_keep_the_session() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (tmp, a, body) = four_mib();
    let b = tmp.path().join("b.txt");
    std::fs::write(&b, "bee").unwrap();
    let proxy = Proxy::default();
    let fs = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
    let mut r = Box::new(fs).into_ranged(p(&a)).unwrap();
    r.read(1000, 10);
    assert!(next(r.as_mut()).data().unwrap()[..] == body[1000..1010]);
    let closes = || {
        let wire = proxy.wire.lock().unwrap();
        wire.iter()
            .filter(|w| !w.reply && w.kind == Wired::CLOSE)
            .count()
    };
    let before = closes();
    let mut r = r.reopen(p(&b)).unwrap();
    assert_eq!(closes(), before + 1, "the first handle closed");
    assert_eq!((r.path(), r.size()), (p(&b), 3));
    r.read(0, 64);
    assert_eq!(&next(r.as_mut()).data().unwrap()[..], b"bee");
    let mut conn = r.into_connection();
    assert_eq!(closes(), before + 2, "the second handle closed");
    let mut out = Vec::new();
    conn.read_to(p(&a), &mut out, &mut |_| {}).unwrap();
    assert!(out == body);
    // A failed reopen gives the session back too.
    let r = conn.into_ranged(p(&b)).unwrap();
    let e = r.reopen(p(&tmp.path().join("gone"))).unwrap_err();
    assert_eq!(e.conn.into_ranged(p(&b)).unwrap().size(), 3);
}

/// 06 §2: closing with requests out, on a server that has stopped answering, ends within the
/// close's bound — every clone of the handle is dropped first, and `Sftp::close` is bounded —
/// and neither `close` nor a drop waits for it. Giving the connection back is bounded the same
/// way. A request with no reply fails once the stall bound passes.
#[test]
fn closing_with_requests_out_is_bounded() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, path, _) = four_mib();
    let timing = Timing {
        close: Duration::from_millis(300),
        stall: Duration::from_millis(400),
    };
    let stalled = || {
        let proxy = Proxy::default();
        let fs = SftpRemote::local_server_with(Some(proxy.clone())).unwrap();
        let mut r: SftpRanged = Box::new(fs).open_ranged(p(&path), timing).unwrap();
        proxy
            .stall
            .store(true, std::sync::atomic::Ordering::Relaxed);
        for i in 0..16 {
            r.read(i * 65_536, 65_536);
        }
        r
    };
    let started = std::time::Instant::now();
    stalled().close_and_wait();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );

    let started = std::time::Instant::now();
    let r: Box<dyn Ranged> = Box::new(stalled());
    r.close();
    drop(Box::new(stalled()) as Box<dyn Ranged>);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "close and drop do not wait"
    );

    let started = std::time::Instant::now();
    let conn = (Box::new(stalled()) as Box<dyn Ranged>).into_connection();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    drop(conn);

    let mut r = stalled();
    let c = r
        .next(Some(Duration::from_secs(10)))
        .expect("the stall bound fails it");
    let e = format!("{:#}", c.reply.unwrap_err());
    assert!(e.contains("no reply from the server"), "{e}");
    r.close_and_wait();
}

/// A transport whose thread dies (a panic) never leaves `next` waiting: every request still
/// queued, and every one queued later, fails in its turn.
#[test]
fn a_dead_transport_fails_what_is_queued() {
    let Some(fs) = SftpRemote::local_server() else {
        return;
    };
    let (_tmp, path, _) = four_mib();
    let mut r = Box::new(fs)
        .open_ranged(p(&path), Timing::default())
        .unwrap();
    r.read(0, 10);
    next(&mut r).reply.unwrap();
    r.kill_transport();
    r.read(0, 10);
    r.fstat();
    assert_eq!(r.pending(), 2);
    for _ in 0..2 {
        let c = r.next(None).expect("failed, not waiting forever");
        let e = format!("{:#}", c.reply.unwrap_err());
        assert!(e.contains("transport stopped"), "{e}");
    }
    assert!(r.next(None).is_none());
    assert_eq!(r.pending(), 0);
}

/// 06 §2: the close drops every clone of the handle before `Sftp::close`, which waits while any
/// lives. With 64 reads out and a close bound of 30 s, a server that answers lets the close end
/// at once: the session closes cleanly, not at its bound.
#[test]
fn closing_drops_every_clone_before_the_session_closes() {
    let Some(fs) = SftpRemote::local_server() else {
        return;
    };
    let (_tmp, path, _) = four_mib();
    let timing = Timing {
        close: Duration::from_secs(30),
        ..Timing::default()
    };
    let mut r = Box::new(fs).open_ranged(p(&path), timing).unwrap();
    for i in 0..64 {
        r.read(i * 65_536, 65_536);
    }
    let started = std::time::Instant::now();
    r.close_and_wait();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

/// The pattern a word at a time is the pattern byte by byte, from any offset.
#[test]
fn the_pattern_from_any_offset() {
    for off in [0u64, 1, 7, 8, 13, 4093] {
        for len in [0usize, 1, 5, 8, 9, 100] {
            let want: Vec<u8> = (off..off + len as u64).map(pattern_byte).collect();
            assert_eq!(pattern(off, len), want, "{off} {len}");
        }
    }
}

/// The Fake's link (06 §2): replies come in order, each `latency` after its request and then at
/// `rate`, in simulated time; each request records the reply bytes ahead of it; a reply is cut
/// to `clip`; a rate drop applies to later requests; with the real clock `next` sleeps.
#[test]
fn the_fakes_link() {
    let (mut r, control) = fake("/m.bin", FakeFile::pattern(1 << 20), Link::gate());
    let start = r.now();
    for i in 0..3 {
        r.read(i * 65_536, 65_536);
    }
    let per = Duration::from_secs_f64(65_536.0 / 5e6);
    let ms20 = Duration::from_millis(20);
    let c = next(r.as_mut());
    assert_eq!(c.took, ms20 + per);
    assert_eq!(c.at - start, ms20 + per);
    assert_eq!(
        next(r.as_mut()).at - start,
        ms20 + per * 2,
        "behind the reply before"
    );
    let aheads: Vec<u64> = control.sent().iter().map(|s| s.ahead).collect();
    assert_eq!(aheads, [0, 65_536, 131_072]);
    assert_eq!(r.pending(), 1);
    next(r.as_mut());
    assert_eq!(r.pending(), 0);
    // Nothing out: a wait passes simulated time and returns nothing.
    let before = control.elapsed();
    assert!(r.next(Some(Duration::from_secs(2))).is_none());
    assert_eq!(control.elapsed() - before, Duration::from_secs(2));
    // A read past the size at open sends nothing.
    r.read(5 << 20, 10);
    assert_eq!(
        next(r.as_mut()).asked,
        Asked::Read {
            off: 5 << 20,
            len: 0
        }
    );
    assert_eq!(control.sent().len(), 3);
    control.set_link(|l| {
        l.rate = 500_000;
        l.clip = Some(1000);
    });
    r.read(0, 65_536);
    let c = next(r.as_mut());
    assert_eq!(c.data().unwrap().len(), 1000);
    assert_eq!(c.took, ms20 + Duration::from_secs_f64(1000.0 / 5e5));

    let real = Link {
        latency: Duration::from_millis(50),
        clock: Clock::Real,
        ..Link::gate()
    };
    let (mut r, _) = fake("/m.bin", FakeFile::pattern(1 << 20), real);
    let started = std::time::Instant::now();
    r.read(0, 100);
    assert!(
        r.next(Some(Duration::from_millis(1))).is_none(),
        "not due yet"
    );
    next(r.as_mut());
    assert!(started.elapsed() >= Duration::from_millis(50));
}

/// A wake from another thread ends a `next` that waits without a timeout.
#[test]
fn a_wake_ends_a_wait() {
    let (mut r, _) = fake("/m.bin", FakeFile::pattern(100), Link::gate());
    let wake = r.waker();
    let waker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        wake.wake();
    });
    assert!(r.next(None).is_none());
    waker.join().unwrap();
    r.read(0, 10);
    assert_eq!(
        next(r.as_mut()).data().unwrap().len(),
        10,
        "and nothing is lost"
    );
}

/// The Fake's file (06 §2): written in place (the mtime moves, or not within the same second),
/// cut, replaced under the path (the open handle keeps the old file) and removed; a change can
/// land right before a given request.
#[test]
fn the_fakes_file_changes() {
    let (mut r, control) = fake("/m.bin", FakeFile::pattern(1000), Link::gate());
    let t0 = r.opened().mtime;
    control.change(Change::Write {
        off: 10,
        bytes: vec![0xAA; 4],
        same_second: false,
    });
    r.read(8, 8);
    r.fstat();
    let mut want = patterned(16)[8..].to_vec();
    want[2..6].fill(0xAA);
    assert_eq!(next(r.as_mut()).data().unwrap()[..], want[..]);
    assert_eq!(
        stat_of(&next(r.as_mut())).mtime,
        t0 + Duration::from_secs(1)
    );
    control.change(Change::Write {
        off: 0,
        bytes: vec![1],
        same_second: true,
    });
    r.fstat();
    let same = stat_of(&next(r.as_mut()));
    assert_eq!(
        (same.size, same.mtime),
        (1000, t0 + Duration::from_secs(1)),
        "unseen"
    );
    // Right before request n (the 5th sent so far, counted from 0): cut to 500.
    let n = control.sent().len();
    control.change_before(n + 1, Change::Truncate(500));
    r.fstat();
    r.fstat();
    assert_eq!(stat_of(&next(r.as_mut())).size, 1000);
    assert_eq!(stat_of(&next(r.as_mut())).size, 500);
    control.change(Change::Truncate(600));
    r.read(490, 20);
    let mut grown = patterned(500)[490..].to_vec();
    grown.extend([0; 10]);
    assert_eq!(
        next(r.as_mut()).data().unwrap()[..],
        grown[..],
        "zeros past the cut"
    );
    // Replaced: the handle reads the old file, the path names the new one.
    control.change(Change::Replace(FakeFile::bytes(b"new".to_vec())));
    r.fstat();
    r.stat_path();
    assert_eq!(stat_of(&next(r.as_mut())).size, 600);
    assert_eq!(stat_of(&next(r.as_mut())).size, 3);
    let mut reopened = control.open().unwrap();
    reopened.read(0, 10);
    assert_eq!(&next(reopened.as_mut()).data().unwrap()[..], b"new");
    control.change(Change::Remove);
    r.stat_path();
    assert!(matches!(next(r.as_mut()).reply, Ok(Reply::Stat(None))));
    r.read(0, 4);
    assert_eq!(
        next(r.as_mut()).data().unwrap().len(),
        4,
        "the handle still reads"
    );
    assert!(control.open().is_err());
    assert!(control.file().is_none());
}

/// The Fake's faults: a status error on one request, then a lost channel failing that request
/// and every later one on it; a new channel works.
#[test]
fn the_fakes_faults() {
    let (mut r, control) = fake("/m.bin", FakeFile::pattern(1000), Link::gate());
    control.fault(1, Fault::Status("Failure"));
    control.fault(3, Fault::Lost);
    for _ in 0..5 {
        r.read(0, 10);
    }
    let ok: Vec<bool> = (0..5).map(|_| next(r.as_mut()).reply.is_ok()).collect();
    assert_eq!(ok, [true, false, true, false, false]);
    r.fstat();
    let e = format!("{:#}", next(r.as_mut()).reply.unwrap_err());
    assert!(e.contains("the connection closed"), "{e}");
    let mut fresh = control.open().unwrap();
    fresh.read(0, 10);
    assert!(next(fresh.as_mut()).reply.is_ok());
}

/// The Fake gives back a connection that stats and reads its path and opens it again.
#[test]
fn the_fakes_connection() {
    let (r, control) = fake("/m.bin", FakeFile::pattern(1000), Link::gate());
    let mut conn = r.into_connection();
    assert_eq!(conn.stat("/m.bin").unwrap().size, 1000);
    assert_eq!(conn.stat_opt("/other").unwrap(), None);
    assert!(conn.read_head("/m.bin", 16).unwrap() == patterned(16));
    let mut out = Vec::new();
    conn.read_to("/m.bin", &mut out, &mut |_| {}).unwrap();
    assert!(out == control.file().unwrap().read(0, 1000));
    let r = conn.into_ranged("/m.bin").unwrap();
    let e = r.reopen("/other").unwrap_err();
    assert!(e.conn.into_ranged("/m.bin").is_ok());
}

/// The local implementation (tests only): a local file's reads at their offsets, its `fstat` and
/// its path's `stat`, an early end after a cut, and the connection given back.
#[test]
fn the_local_ranged() {
    let (tmp, path, body) = four_mib();
    let mut r = Box::new(LocalFs::new()).into_ranged(p(&path)).unwrap();
    assert_eq!(r.size(), body.len() as u64);
    r.read(3 << 20, 100);
    r.read(10, 100);
    assert!(next(r.as_mut()).data().unwrap()[..] == body[3 << 20..(3 << 20) + 100]);
    assert!(next(r.as_mut()).data().unwrap()[..] == body[10..110]);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(50)
        .unwrap();
    r.read(40, 100);
    assert_eq!(next(r.as_mut()).data().unwrap().len(), 10);
    r.read(60, 100);
    assert!(next(r.as_mut()).early_end());
    r.fstat();
    assert_eq!(stat_of(&next(r.as_mut())).size, 50);
    std::fs::rename(&path, tmp.path().join("moved")).unwrap();
    r.stat_path();
    assert!(matches!(next(r.as_mut()).reply, Ok(Reply::Stat(None))));
    let mut conn = r.into_connection();
    assert_eq!(conn.stat(p(&tmp.path().join("moved"))).unwrap().size, 50);
}

/// Throughput of a whole-file read against the local `sftp-server` (06 §2's measurement), today's
/// pipelined read against 3a426b6's loop, and the p50 of a random 64 KiB read through the
/// transport: `SOPEN_BENCH_FILE=<a large file> cargo test --release --lib bench_ -- --ignored
/// --nocapture`.
#[test]
#[ignore]
fn bench_whole_read_throughput() {
    let file = std::env::var("SOPEN_BENCH_FILE").expect("SOPEN_BENCH_FILE");
    let mut fs = SftpRemote::local_server().unwrap();
    let size = std::fs::metadata(&file).unwrap().len();
    let rate = |n: u64, s: f64| n as f64 / s / 1e6;
    for round in 0..3 {
        let started = std::time::Instant::now();
        let n = fs
            .read_to(&file, &mut std::io::sink(), &mut |_| {})
            .unwrap();
        let s = started.elapsed().as_secs_f64();
        assert_eq!(n, size);
        eprintln!(
            "bench read_to round {round}: {n} bytes in {s:.3} s = {:.0} MB/s",
            rate(n, s)
        );
        let started = std::time::Instant::now();
        let n = fs.read_through_cursor(&file).unwrap().len() as u64;
        let s = started.elapsed().as_secs_f64();
        assert_eq!(n, size);
        eprintln!(
            "bench 3a426b6 loop round {round}: {s:.3} s = {:.0} MB/s",
            rate(n, s)
        );
    }
    let mut r = Box::new(fs).into_ranged(&file).unwrap();
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut took = Vec::new();
    for _ in 0..1000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let off = seed % (size - 65_536);
        r.read(off, 65_536);
        let c = next(r.as_mut());
        assert_eq!(c.data().unwrap().len(), 65_536);
        took.push(c.took);
    }
    took.sort();
    eprintln!(
        "bench random 64 KiB read: p50 {:?} p95 {:?} max {:?}",
        took[500], took[950], took[999]
    );
}
