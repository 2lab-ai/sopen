use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, UNIX_EPOCH};

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Files whose name contains `.sopen-` — temp files that must never survive a write.
fn temps(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".sopen-"))
        .collect()
}

fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

/// LocalFs with one primitive made to fail, to prove the provided algorithms clean up.
struct Failing {
    inner: LocalFs,
    fail_rename: bool,
    fail_create_after_bytes: bool,
}

impl RemoteFs for Failing {
    fn stat_opt(&mut self, path: &str) -> Result<Option<Stat>> {
        self.inner.stat_opt(path)
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
    fn create_from(&mut self, local: &Path, path: &str, mode: Option<u32>) -> Result<()> {
        self.inner.create_from(local, path, mode)?;
        if self.fail_create_after_bytes {
            bail!("simulated connection loss after upload");
        }
        Ok(())
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> Result<()> {
        self.inner.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        if self.fail_rename {
            bail!("simulated rename failure");
        }
        self.inner.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.inner.link_new(from, to)
    }
    fn remove(&mut self, path: &str) -> Result<()> {
        self.inner.remove(path)
    }
}

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

    std::fs::write(&local, "echo two, longer\n").unwrap();
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Written { now } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "echo two, longer\n"
    );
    assert_eq!(now.size, 17);
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
        0o750
    );
    assert!(temps(&remote).is_empty(), "{:?}", temps(&remote));

    // The returned stat is the baseline for the next save.
    std::fs::write(&local, "echo three\n").unwrap();
    let again = fs
        .write_atomic(&local, p(&target), now.fingerprint())
        .unwrap();
    assert!(matches!(again, WriteOutcome::Written { .. }), "{again:?}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "echo three\n");
}

#[test]
fn write_atomic_refuses_when_size_or_mtime_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("notes.txt");
    std::fs::write(&target, "v1").unwrap();
    set_mtime(&target, 1_700_000_000);
    let mut fs = LocalFs::new();
    let local = tmp.path().join("copy.txt");
    let at = fs.fetch(p(&target), &local, &mut |_, _| {}).unwrap();
    std::fs::write(&local, "my edit").unwrap();

    // Someone else rewrites it: same size, different mtime.
    std::fs::write(&target, "v2").unwrap();
    set_mtime(&target, 1_700_000_100);
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    let WriteOutcome::Conflict { now: Some(now) } = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(now.mtime, UNIX_EPOCH + Duration::from_secs(1_700_000_100));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2");
    assert_eq!(std::fs::read_to_string(&local).unwrap(), "my edit");

    // Same mtime, different size.
    std::fs::write(&target, "v2 plus").unwrap();
    set_mtime(&target, 1_700_000_000);
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert!(matches!(outcome, WriteOutcome::Conflict { now: Some(_) }));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2 plus");

    // Deleted since the fetch.
    std::fs::remove_file(&target).unwrap();
    let outcome = fs
        .write_atomic(&local, p(&target), at.fingerprint())
        .unwrap();
    assert_eq!(outcome, WriteOutcome::Conflict { now: None });
    assert!(!target.exists());
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
        let mut fs = Failing {
            inner: LocalFs::new(),
            fail_rename,
            fail_create_after_bytes: fail_create,
        };
        let at = fs.stat(p(&target)).unwrap();
        let err = fs
            .write_atomic(&local, p(&target), at.fingerprint())
            .unwrap_err();
        assert!(format!("{err:#}").contains("simulated"), "{err:#}");
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
    let mut fs = LocalFs::new();
    assert_eq!(fs.list_dir(p(tmp.path())).unwrap(), ["a", "b"]);
    assert_eq!(fs.read_head(p(&tmp.path().join("b")), 4).unwrap(), b"0123");
    let real = fs.realpath(p(&tmp.path().join("b"))).unwrap();
    assert!(real.ends_with("/b"), "{real}");
    assert!(fs.stat_opt(p(&tmp.path().join("nope"))).unwrap().is_none());
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
}
