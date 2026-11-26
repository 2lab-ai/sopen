//! X2-host's question of the browser ([`Browser::would_lose`], 10 §7.3): what HOST's window would
//! lose if its input ended now — a running file operation and a running git action, each true
//! while it runs and false once it has ended. (The viewer's unsaved edits are
//! `gui::switch_host`'s tests.)

use super::s2::{small, Gx};
use super::*;
use crate::git::history::fixture;
use std::time::{Duration, Instant};

/// A stand-in copy held at `gate`, then done.
fn held_copy(gate: &Gate) -> ops::Run {
    let gate = gate.clone();
    Arc::new(move |job: ops::Job, _: &ops::Link| {
        let ops::Job::Transfer { names, .. } = job else {
            return Report::default();
        };
        gate.hold();
        Report {
            items: vec![Item {
                name: names[0].clone(),
                outcome: ItemOutcome::Done,
            }],
            notes: Vec::new(),
        }
    })
}

/// A file operation that runs is something to lose; once it has ended, nothing is.
#[test]
fn a_running_file_operation_would_be_lost() {
    let mut fx = fixture();
    let dir = format!("{}/alpha", fx.root);
    fx.b.list(Some(1), dir, None, None);
    fx.settle();
    assert!(!fx.b.would_lose(), "nothing runs yet");
    let gate = Gate::default();
    fx.b.set_ops_run(held_copy(&gate));
    fx.select("main.rs");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    gate.reached();
    assert!(fx.b.ops.busy());
    assert!(fx.b.would_lose(), "the copy runs");
    gate.release();
    fx.drv
        .until(&mut fx.b, 20, |b| !b.ops.busy() && b.listing.is_empty());
    assert!(!fx.b.would_lose(), "the copy has ended");
}

/// The pid a stand-in hook wrote to `file`, waited for (at most 20 s).
fn pid_in(file: &Path) -> i32 {
    let t = Instant::now();
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return pid;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "no pid in {file:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A git action that runs (a commit held by its pre-commit hook) is something to lose; once it
/// has ended, nothing is.
#[test]
fn a_running_git_action_would_be_lost() {
    let Some(git) = fixture::git() else {
        return;
    };
    let repo = small(&git);
    let pidfile = repo.tmp.path().join("hook.pid");
    let release = repo.tmp.path().join("release");
    super::s3::script(
        &repo.top.join(".git/hooks/pre-commit"),
        // Held until the test lets it go — 30 s at most, so a failing test leaves no hook behind.
        &format!(
            "echo $$ > '{}'\nn=0\nwhile [ ! -e '{}' ] && [ $n -lt 600 ]; do sleep 0.05; n=$((n+1)); done",
            pidfile.display(),
            release.display()
        ),
    );
    repo.run(&["add", "a.txt"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    assert!(!g.fx.b.would_lose(), "nothing runs yet");
    g.f(Key::F7);
    g.fx.typed("held by its hook");
    g.idle(2);
    g.key(Key::Enter);
    pid_in(&pidfile);
    assert_eq!(g.panel().running.len(), 1, "the commit runs");
    assert!(g.fx.b.would_lose(), "the commit runs");
    std::fs::write(&release, "").unwrap();
    g.until_quiet("the commit's end");
    assert!(g.panel().running.is_empty());
    assert!(!g.fx.b.would_lose(), "the commit has ended");
}

/// Set in the child of [`a_report_after_the_window_never_hits_the_dead_session`]: where it lands
/// its last write.
const DEAD_SESSION_CHILD: &str = "SOPEN_TEST_DEAD_SESSION_CHILD";

/// The child's cue: the window has returned.
const RETURNED: &str = "sopen-test-returned";

/// Review round 1's sequence, on the real path: a copy runs when the window returns, so the
/// browser hands it to what outlives the window (`Leftover`); the cue goes out; the session dies
/// (its output's reader, then its input, gone); the copy ends and `Leftover::finish` prints its
/// report on stderr (`browser.rs`, "the file operation ended after the window closed") into the
/// dead session. With fds 1 and 2 relayed (`switch_host::relay_stdio`) that write fails nowhere:
/// what comes after it — here a write that lands a file — runs, and the run ends 0. A child of
/// this test binary plays HOST's sopen with its own fds; the test is the other side.
#[test]
fn a_report_after_the_window_never_hits_the_dead_session() {
    use std::io::BufRead as _;
    if let Some(landed) = std::env::var_os(DEAD_SESSION_CHILD) {
        return dead_session_child(Path::new(&landed));
    }
    let tmp = tempfile::tempdir().unwrap();
    let landed = tmp.path().join("landed");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "gui::browser::tests::x2host::a_report_after_the_window_never_hits_the_dead_session",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DEAD_SESSION_CHILD, &landed)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (seen, cue) = mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = std::io::BufReader::new(stdout.unwrap()).lines();
        // The test harness's `test … ... ` stands before it on its line.
        let saw = lines.any(|l| l.is_ok_and(|l| l.ends_with(RETURNED)));
        // The other side's end of the output goes before it says it saw the cue.
        drop(lines);
        let _ = seen.send(saw);
    });
    assert_eq!(
        cue.recv_timeout(Duration::from_secs(120)),
        Ok(true),
        "the child's cue"
    );
    drop(stderr);
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the child did not end");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "the child ended {status:?}");
    assert_eq!(
        std::fs::read_to_string(&landed).ok().as_deref(),
        Some("what comes after the report landed")
    );
}

/// The child's half: its output relayed, a held copy handed to what outlives the window, the cue,
/// then — once the session is gone — the copy released and `Leftover::finish`, then a write that
/// lands.
fn dead_session_child(landed: &Path) {
    // As `switch_host::run` does before its window.
    crate::gui::switch_host::relay_stdio().expect("fds 1 and 2 relayed");
    let mut fx = fixture();
    let dir = format!("{}/alpha", fx.root);
    fx.b.list(Some(1), dir, None, None);
    fx.settle();
    let gate = Gate::default();
    fx.b.set_ops_run(held_copy(&gate));
    fx.select("main.rs");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    gate.reached();
    let leftover = fx.b.leftover.clone();
    // The window's end: the browser hands the running copy over.
    drop(fx);
    println!("{RETURNED}");
    let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    gate.release();
    let outcome = leftover.borrow_mut().finish();
    assert!(outcome.refused.is_none());
    std::fs::write(landed, "what comes after the report landed").unwrap();
    crate::gui::switch_host::end_of_run();
}
