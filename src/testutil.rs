//! Support shared by the unit tests.

use crate::remote::{LocalFs, RemoteFs};
use anyhow::Result;
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::time::Duration;

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

/// Runs `cmd` to its end, for a stub executable a test has just written. On Linux such a file can
/// be "busy" (ETXTBSY) for a moment: a child that another test thread forked meanwhile still holds
/// its write descriptor until that child execs. A busy start is retried, up to 50 times 20 ms
/// apart; any other error, or the last busy one, panics. Each busy refusal is counted for the
/// program ([`busy_refusals`]).
pub fn run_fresh(cmd: &mut Command) -> Output {
    let program = PathBuf::from(cmd.get_program());
    retry_busy(|| {
        let out = cmd.output();
        if out
            .as_ref()
            .is_err_and(|e| e.kind() == ErrorKind::ExecutableFileBusy)
        {
            *BUSY.lock().unwrap().entry(program.clone()).or_default() += 1;
        }
        out
    })
    .unwrap_or_else(|e| panic!("cannot run {cmd:?}: {e}"))
}

/// [`run_fresh`]'s busy refusals, per program as its `Command` names it.
static BUSY: Mutex<BTreeMap<PathBuf, usize>> = Mutex::new(BTreeMap::new());

/// How many starts of `program` [`run_fresh`] has had refused as busy (ETXTBSY) so far: a test
/// that keeps a stub busy on purpose waits for a refusal before it lets the stub go. Such tests
/// are Linux's alone (macOS starts a script still open for writing).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn busy_refusals(program: &Path) -> usize {
    BUSY.lock().unwrap().get(program).copied().unwrap_or(0)
}

/// `attempt` until it is not refused as busy (ETXTBSY), at most 50 retries 20 ms apart.
fn retry_busy<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut retries = 0;
    loop {
        match attempt() {
            Err(e) if e.kind() == ErrorKind::ExecutableFileBusy && retries < 50 => {
                retries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            done => return done,
        }
    }
}

#[test]
fn a_busy_start_is_retried_then_given_up() {
    let busy = || std::io::Error::from(ErrorKind::ExecutableFileBusy);
    let mut calls = 0;
    let got = retry_busy(|| {
        calls += 1;
        if calls < 3 {
            Err(busy())
        } else {
            Ok(calls)
        }
    });
    assert_eq!(got.unwrap(), 3);
    // Busy for good: given up after 50 retries, with that error.
    let mut calls = 0;
    let got = retry_busy(|| -> std::io::Result<()> {
        calls += 1;
        Err(busy())
    });
    assert_eq!(got.unwrap_err().kind(), ErrorKind::ExecutableFileBusy);
    assert_eq!(calls, 51);
    // Any other error: at once.
    let mut calls = 0;
    let got = retry_busy(|| -> std::io::Result<()> {
        calls += 1;
        Err(ErrorKind::NotFound.into())
    });
    assert_eq!(got.unwrap_err().kind(), ErrorKind::NotFound);
    assert_eq!(calls, 1);
}

/// The real thing (Linux only: macOS starts a script that is still open for writing): a stub
/// whose write descriptor is still open is busy; it runs once that descriptor is closed. The
/// descriptor is closed only after [`run_fresh`]'s start was refused ([`busy_refusals`]; 5 s at
/// most, so a start that never retries still ends), never on a timer: a stall of this thread
/// cannot let a start that does not retry pass.
#[cfg(target_os = "linux")]
#[test]
fn a_stub_still_open_for_writing_runs_once_it_is_closed() {
    use std::io::Write as _;
    use std::time::Instant;
    let tmp = tempfile::tempdir().unwrap();
    let stub = tmp.path().join("stub");
    let mut file = std::fs::File::create(&stub).unwrap();
    file.write_all(b"#!/bin/sh\necho ran\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = Command::new(&stub).output().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ExecutableFileBusy, "{err}");
    let refused = || busy_refusals(&stub);
    let out = std::thread::scope(|s| {
        s.spawn(move || {
            let until = Instant::now() + Duration::from_secs(5);
            while refused() == 0 && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(file);
        });
        run_fresh(&mut Command::new(&stub))
    });
    assert!(refused() >= 1, "the start was refused, then retried");
    assert_eq!(out.stdout, b"ran\n");
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
    fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
        self.0.list_entries(p)
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
