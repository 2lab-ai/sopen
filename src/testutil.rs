//! Support shared by the unit tests.

use crate::remote::{LocalFs, RemoteFs};
use anyhow::Result;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Set in the child [`rerun_under_umask_022`] starts, to the name of the test it runs.
const CHILD: &str = "SOPEN_TEST_UMASK_CHILD";

/// For tests that assert modes: those must not pass only because the umask happens to be
/// strict (under 077 plain creation already gives 0600 / 0700). Called first in such a test.
/// In the test process it runs `test` (its full path) again in a child process whose umask is
/// 022 — `sh` sets it, then execs this test binary — asserts that the child passed, and returns
/// `true`: the caller returns. In that child it checks that plain creation there gives
/// 0644 / 0755 and returns `false`: the caller runs its body. The test process's own umask is
/// never changed (it is shared by every test running in it).
pub fn rerun_under_umask_022(test: &str) -> bool {
    if std::env::var(CHILD).is_ok_and(|t| t == test) {
        let tmp = tempfile::tempdir().unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let (file, dir) = (tmp.path().join("plain"), tmp.path().join("plain-dir"));
        std::fs::File::create(&file).unwrap();
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(
            (mode(&file), mode(&dir)),
            (0o644, 0o755),
            "umask 022 in the child"
        );
        return false;
    }
    let exe = std::env::current_exe().unwrap();
    let out = Command::new("/bin/sh")
        .args(["-c", "umask 022 && exec \"$0\" \"$@\""])
        .arg(&exe)
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD, test)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{test} under umask 022:\n{text}");
    assert!(
        text.contains("1 passed"),
        "{test} did not run in the child:\n{text}"
    );
    true
}

/// The local disk, posing as a server without `posix-rename@openssh.com`.
pub(crate) struct NoReplace(pub(crate) LocalFs);

impl RemoteFs for NoReplace {
    fn stat_opt(&mut self, p: &str) -> Result<Option<crate::remote::Stat>> {
        self.0.stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<crate::remote::Stat>> {
        self.0.lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.0.realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.0.read_head(p, max)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.0.list_dir(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        self.0.read_to(p, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.0.create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.0.set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.0.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.0.link_new(from, to)
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.0.remove(p)
    }
    fn can_replace(&self) -> bool {
        false
    }
}
