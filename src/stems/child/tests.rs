//! The window's side of `sopen stems` against stand-in children (shell scripts): progress, the
//! go-ahead hand-shake, and what a cancel leaves behind.

use super::*;
use std::time::{Duration, Instant};

/// A stand-in `sopen` at `<dir>/fake-sopen`: `/bin/sh` running `body` with the arguments
/// spawn_child passes (`stems --work-dir W --wait-to-publish -- SOURCE`, so `$3` is the work
/// folder).
fn fake(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-sopen");
    std::fs::write(&path, format!("#!/bin/sh\nw=\"$3\"\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// spawn_child, retried while Linux calls the just-written script busy (ETXTBSY: another test
/// thread forked while it was open for writing; see testutil::run_fresh).
fn spawn(program: &Path, work_root: &Path) -> StemsChild {
    for _ in 0..50 {
        match spawn_child(Command::new(program), "box:/a.mp3", None, work_root) {
            Ok(child) => return child,
            Err(e)
                if e.chain().any(|c| {
                    c.downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::ExecutableFileBusy)
                }) =>
            {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(e) => panic!("{e:#}"),
        }
    }
    panic!("the stand-in stayed busy");
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Cancel mid-separation: the child and the ffmpeg-like grandchild it started are killed, its
/// work folder (with a half-written encode in it) is gone, and nothing is published.
#[test]
fn cancel_kills_the_group_removes_the_work_folder_and_publishes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let published = tmp.path().join("published");
    std::fs::create_dir(&published).unwrap();
    let pid_file = tmp.path().join("grandchild.pid");
    let program = fake(
        tmp.path(),
        &format!(
            "echo 'stems: piece 0/3, 0s elapsed' >&2
echo half > \"$w/a.voice.mp3\"
sleep 60 &
echo $! > '{pid}'
echo 'stems: piece 1/3, 20s elapsed, 40s remaining' >&2
sleep 60
echo 'stems: ready to publish' >&2
read go && echo voice > '{out}/a.voice.mp3'",
            pid = pid_file.display(),
            out = published.display()
        ),
    );
    let work_root = tmp.path().join("exports");
    let mut child = spawn(&program, &work_root);
    let work = child.work_dir().to_path_buf();
    until("piece 1/3", || {
        child.progress().is_some_and(|p| p.done == 1) && pid_file.exists()
    });
    let p = child.progress().unwrap();
    assert_eq!((p.done, p.total, p.remaining_s), (1, 3, Some(40)));
    assert!((p.fraction() - 1.0 / 3.0).abs() < 1e-9);
    assert!(
        work.join("a.voice.mp3").exists(),
        "the stand-in wrote its encode"
    );
    let grandchild: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(grandchild));

    // At once: the grandchild holds the child's stderr, so a cancel that killed the child
    // alone would wait out its 60 s here.
    let asked = Instant::now();
    assert_eq!(child.cancel(), Cancel::Cancelled);
    assert!(
        asked.elapsed() < Duration::from_secs(10),
        "{:?}",
        asked.elapsed()
    );
    assert!(!work.exists(), "the work folder is removed");
    until("the grandchild to die", || !alive(grandchild));
    assert!(child.poll().unwrap().is_err());
    let err = child.wait().unwrap_err().to_string();
    assert!(err.contains("cancelled"), "{err}");
    assert_eq!(std::fs::read_dir(&published).unwrap().count(), 0);
    // Only its own folder was removed.
    assert!(work_root.is_dir());
}

/// Encoded: the child says so and waits; the parent tells it to publish, and the two paths it
/// prints are the result, with its notes.
#[test]
fn the_child_publishes_only_after_the_go_ahead() {
    let tmp = tempfile::tempdir().unwrap();
    let heard = tmp.path().join("heard");
    let program = fake(
        tmp.path(),
        &format!(
            "echo 'stems: piece 1/1, 3s elapsed, 0s remaining' >&2
echo 'sopen: only the first audio track is separated (1 more not)' >&2
echo 'stems: ready to publish' >&2
read go
echo \"$go\" > '{heard}'
[ \"$go\" = publish ] || exit 1
echo 'box:/a.voice.mp3'
echo 'box:/a.music.mp3'",
            heard = heard.display()
        ),
    );
    let child = spawn(&program, &tmp.path().join("exports"));
    let work = child.work_dir().to_path_buf();
    let published = child.wait().unwrap();
    assert_eq!(published.voice, "box:/a.voice.mp3");
    assert_eq!(published.music, "box:/a.music.mp3");
    assert_eq!(
        published.notes,
        ["only the first audio track is separated (1 more not)"]
    );
    assert_eq!(std::fs::read_to_string(&heard).unwrap(), "publish\n");
    assert!(!work.exists());
}

/// A cancel after the go-ahead is too late: the child publishes and its result comes as usual.
#[test]
fn a_cancel_once_publishing_is_too_late() {
    let tmp = tempfile::tempdir().unwrap();
    let told = tmp.path().join("told");
    let program = fake(
        tmp.path(),
        &format!(
            "echo 'stems: ready to publish' >&2
read go
touch '{told}'
sleep 1
echo 'box:/a.voice.mp3'
echo 'box:/a.music.mp3'",
            told = told.display()
        ),
    );
    let mut child = spawn(&program, &tmp.path().join("exports"));
    until("the go-ahead", || told.exists());
    assert_eq!(child.cancel(), Cancel::TooLate);
    let published = child.wait().unwrap();
    assert_eq!(published.voice, "box:/a.voice.mp3");
}

/// A child that fails: its last `sopen: ` line is the error, and the work folder is gone.
#[test]
fn a_failed_child_reports_its_error_line() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fake(
        tmp.path(),
        "echo 'stems: piece 0/2, 0s elapsed' >&2
echo 'sopen: a.mp3 has no audio stream to separate' >&2
exit 1",
    );
    let mut child = spawn(&program, &tmp.path().join("exports"));
    let work = child.work_dir().to_path_buf();
    until("the end", || child.poll().is_some());
    let err = child.wait().unwrap_err().to_string();
    assert_eq!(err, "a.mp3 has no audio stream to separate");
    assert!(!work.exists());
}

/// A child nobody waits for is cancelled when dropped.
#[test]
fn dropping_a_running_child_cancels_it() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("pid");
    let program = fake(
        tmp.path(),
        &format!("echo $$ > '{}'\nsleep 60", pid_file.display()),
    );
    let child = spawn(&program, &tmp.path().join("exports"));
    let work = child.work_dir().to_path_buf();
    until("the child to start", || {
        std::fs::read_to_string(&pid_file).is_ok_and(|s| s.ends_with('\n'))
    });
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(child);
    assert!(!work.exists());
    assert!(!alive(pid), "reaped by the drop");
}

#[test]
fn the_go_ahead_is_one_exact_line() {
    go_ahead("publish\n").unwrap();
    for no in ["", "no\n", "publish now\n"] {
        let err = go_ahead(no).unwrap_err();
        assert!(err.to_string().contains("nothing was published"), "{err}");
    }
}

/// The watch hands the parent's line on; the end of stdin before it is tested on the real
/// child (tests/cli.rs), since it ends the process.
#[test]
fn the_watch_passes_the_first_line_on() {
    let rx = watch_parent(std::io::Cursor::new(b"publish\nmore\n".to_vec()), None);
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(line, "publish\n");
    go_ahead(&line).unwrap();
}

#[test]
fn lines_end_at_newlines_and_carriage_returns() {
    let mut r = BufReader::new(&b"a\rbb\n\rc"[..]);
    let mut got = Vec::new();
    let mut line = Vec::new();
    while read_line_cr(&mut r, &mut line).unwrap() > 0 {
        got.push(String::from_utf8(std::mem::take(&mut line)).unwrap());
    }
    assert_eq!(got, ["a\r", "bb\n", "\r", "c"]);
}

/// Signals this thread sent to `pid`'s group after its leader was reaped (whose id may by then
/// be anyone's).
fn signals_after_reaping(pid: u32) -> Vec<(i32, bool)> {
    SIGNALS.with(|s| {
        s.borrow()
            .iter()
            .copied()
            .filter(|(p, reaped)| *p == pid as i32 && *reaped)
            .collect()
    })
}

/// Every group signal this thread sent to `pid`, in order.
fn signals_to(pid: u32) -> Vec<(i32, bool)> {
    SIGNALS.with(|s| {
        s.borrow()
            .iter()
            .copied()
            .filter(|(p, _)| *p == pid as i32)
            .collect()
    })
}

/// A child that fails before it is ready, waited for: the error comes back; its group is
/// signalled exactly once — when it has exited and is not yet reaped, for whatever it left
/// behind — and nothing, not the wait, not the drop after it, signals it once it is reaped.
#[test]
fn waiting_for_a_failed_child_never_signals_it_after_reaping() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fake(
        tmp.path(),
        "echo 'sopen: a.mp3 has no audio stream to separate' >&2\nexit 1",
    );
    let child = spawn(&program, &tmp.path().join("exports"));
    let pid = child.id();
    let err = child.wait().unwrap_err().to_string();
    assert_eq!(err, "a.mp3 has no audio stream to separate");
    assert_eq!(
        signals_after_reaping(pid),
        [],
        "a reaped child's group was signalled"
    );
    assert_eq!(
        signals_to(pid),
        [(pid as i32, false)],
        "one kill, before reaping"
    );
}

/// Dropped while publishing — after the go-ahead, not polled — the handle never cuts the upload
/// short: it waits for the leader to exit, then kills what the leader left in its group (here a
/// process holding its output open), then reaps it, all before drop returns. No survivor, no
/// zombie, the work folder gone, and one kill, before reaping.
#[test]
fn dropping_a_publishing_child_waits_for_it_then_kills_its_survivors_and_reaps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("descendant.pid");
    let told = tmp.path().join("told");
    let program = fake(
        tmp.path(),
        &format!(
            "echo 'stems: ready to publish' >&2
read go
touch '{told}'
sleep 60 &
echo $! > '{pids}.part' && mv '{pids}.part' '{pids}'
sleep 1
echo 'box:/a.voice.mp3'
echo 'box:/a.music.mp3'",
            told = told.display(),
            pids = pid_file.display()
        ),
    );
    let child = spawn(&program, &tmp.path().join("exports"));
    let pid = child.id();
    let work = child.work_dir().to_path_buf();
    until("the go-ahead", || told.exists());
    let started = Instant::now();
    drop(child);
    // The leader was alive when dropped (it sleeps 1 s after the go-ahead): drop waited for it.
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    let descendant: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // No zombie: the leader is not our child any more (reaped), so waitpid has nothing.
    let mut status = 0;
    // SAFETY: a plain syscall; WNOHANG never blocks.
    let r = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
    assert_eq!(r, -1, "the leader was left unreaped");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(descendant) {
        assert!(
            Instant::now() < deadline,
            "the descendant survived the drop"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!work.exists(), "the work folder is left");
    assert_eq!(
        signals_to(pid),
        [(pid as i32, false)],
        "one kill, before reaping"
    );
}

/// Cancelled, then waited for: the one kill comes before reaping, never after.
#[test]
fn waiting_for_a_cancelled_child_never_signals_it_again() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fake(tmp.path(), "sleep 60");
    let mut child = spawn(&program, &tmp.path().join("exports"));
    let pid = child.id();
    assert_eq!(child.cancel(), Cancel::Cancelled);
    let err = child.wait().unwrap_err().to_string();
    assert!(err.contains("cancelled"), "{err}");
    assert_eq!(
        signals_after_reaping(pid),
        [],
        "a reaped child's group was signalled"
    );
    let before: Vec<_> = SIGNALS.with(|s| {
        s.borrow()
            .iter()
            .copied()
            .filter(|(p, _)| *p == pid as i32)
            .collect()
    });
    assert_eq!(
        before,
        [(pid as i32, false)],
        "one kill, while it was not reaped"
    );
}

/// The leader exits while a process it started still holds its output open: `poll` never
/// blocks on that process — it returns at once, the group's survivors are killed, and the
/// result comes on a later poll.
#[test]
fn poll_never_blocks_on_a_descendant_holding_the_output_open() {
    for (exit, outcome_ok) in [
        ("exit 1", false),
        ("echo box:/a.voice.mp3\necho box:/a.music.mp3\nexit 0", true),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let pid_file = tmp.path().join("descendant.pid");
        let program = fake(
            tmp.path(),
            &format!(
                "echo 'sopen: it failed' >&2\nsleep 30 &\necho $! > '{}'\n{exit}",
                pid_file.display()
            ),
        );
        let mut child = spawn(&program, &tmp.path().join("exports"));
        let work = child.work_dir().to_path_buf();
        let started = Instant::now();
        let result = loop {
            let asked = Instant::now();
            let done = child.poll().map(|r| {
                r.as_ref()
                    .map(|p| p.voice.clone())
                    .map_err(|e| e.to_string())
            });
            assert!(
                asked.elapsed() < Duration::from_secs(2),
                "poll blocked for {:?}",
                asked.elapsed()
            );
            if let Some(done) = done {
                break done;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "no result");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(started.elapsed() < Duration::from_secs(20));
        assert_eq!(result.is_ok(), outcome_ok, "{result:?}");
        let descendant: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        until("the descendant to die", || !alive(descendant));
        assert!(!work.exists());
        assert_eq!(signals_after_reaping(child.id()), []);
    }
}
