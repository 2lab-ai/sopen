//! The F-key switch's tests (04 §8 K-F1, K-F2, K-F4, K-F5, K-F6) against fakes behind
//! [`Switch`]: within this process a shared in-memory one with made-up holders, across processes
//! one in a file with real ones (this test binary run again as the holder, its watchdog and the
//! contenders: [`child`]). Nothing here reaches IOKit or the owner's state folder: the one test of
//! the real switch is `macos::tests`, ignored.

use super::*;
use std::collections::HashSet;
use std::io::BufRead;
use std::sync::Mutex;
use std::time::Instant;

// ---------------------------------------------------------------------------------------------
// The in-process fakes.

#[derive(Debug, Default)]
struct Hid {
    value: i64,
    reads: usize,
    writes: Vec<i64>,
    /// A set to 1 fails.
    refuse: bool,
    /// A set to 1 returns but does not take: the value reads 0 after it.
    drops: bool,
    /// Every write fails.
    broken: bool,
    /// The next this many writes fail.
    fail_writes: usize,
    /// Every write returns but changes nothing.
    ignores: bool,
    /// Every read fails.
    blind: bool,
}

/// The HID value every made-up process of a test shares, as processes share the real one.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Hid>>);

impl Fake {
    fn at(value: i64) -> Fake {
        let fake = Fake::default();
        fake.0.lock().unwrap().value = value;
        fake
    }

    fn value(&self) -> i64 {
        self.0.lock().unwrap().value
    }

    fn reads(&self) -> usize {
        self.0.lock().unwrap().reads
    }

    fn writes(&self) -> Vec<i64> {
        self.0.lock().unwrap().writes.clone()
    }

    fn calls(&self) -> usize {
        let hid = self.0.lock().unwrap();
        hid.reads + hid.writes.len()
    }

    fn with(&self, change: impl FnOnce(&mut Hid)) {
        change(&mut self.0.lock().unwrap())
    }
}

impl Switch for Fake {
    fn read(&mut self) -> Result<i64> {
        let mut hid = self.0.lock().unwrap();
        hid.reads += 1;
        if hid.blind {
            bail!("the fake cannot be read");
        }
        Ok(hid.value)
    }

    fn write(&mut self, value: i64) -> Result<()> {
        let mut hid = self.0.lock().unwrap();
        hid.writes.push(value);
        if hid.fail_writes > 0 {
            hid.fail_writes -= 1;
            bail!("the fake failed this write");
        }
        if hid.broken || (value == ON && hid.refuse) {
            bail!("refused by the fake");
        }
        if hid.ignores {
            return Ok(());
        }
        hid.value = if value == ON && hid.drops { OFF } else { value };
        Ok(())
    }
}

/// The made-up processes that run.
#[derive(Clone, Default)]
struct Procs(Arc<Mutex<HashSet<Holder>>>);

const BOOT: &str = "this boot";

impl Procs {
    fn machine(&self) -> Machine {
        let running = self.0.clone();
        Machine {
            runs: Arc::new(move |h| running.lock().unwrap().contains(h)),
            boot: Some(BOOT.to_string()),
        }
    }

    fn start(&self, pid: u32) -> Holder {
        let holder = Holder {
            pid,
            start: 1_000 + u64::from(pid),
        };
        self.0.lock().unwrap().insert(holder);
        holder
    }

    fn kill(&self, holder: Holder) {
        self.0.lock().unwrap().remove(&holder);
    }
}

/// The sopen process `pid`, with the shared fake and no watchdog.
fn process(dir: &Path, hid: &Fake, procs: &Procs, pid: u32) -> Fkeys<Fake> {
    let me = procs.start(pid);
    Fkeys::new(
        dir.to_path_buf(),
        hid.clone(),
        Ok(me),
        procs.machine(),
        Watchdog::off(),
    )
}

fn state(dir: &Path) -> Option<State> {
    read_state(dir).unwrap()
}

/// The state a holder writes before it sets the value (§6.3's order).
fn stale(dir: &Path, holders: Vec<Holder>, boot: &str) {
    std::fs::create_dir_all(dir).unwrap();
    write_state(
        dir,
        &State {
            original: OFF,
            holders,
            boot: Some(boot.to_string()),
        },
    )
    .unwrap();
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---------------------------------------------------------------------------------------------
// K-F1: focus, two processes, the owner's own 1, sleep and session.

/// 04 §8 K-F1 — focus gained → the fake reads 0 and is set to 1; focus lost → 0.
#[test]
fn kf1_focus_gained_sets_one_and_focus_lost_restores_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1);
    assert_eq!(hid.calls(), 0, "nothing is touched before a focus");
    assert!(!dir.exists(), "nor is anything created");

    a.focus(true);
    assert_eq!(hid.value(), ON);
    assert_eq!(hid.writes(), [ON]);
    assert_eq!(hid.reads(), 2, "the value before, and its read-back");
    assert!(a.holds());
    assert_eq!(a.status(), None);
    let s = state(&dir).unwrap();
    assert_eq!(s.original, OFF);
    assert_eq!(
        s.holders,
        [Holder {
            pid: 1,
            start: 1001
        }]
    );
    assert_eq!(s.boot.as_deref(), Some(BOOT));
    assert_eq!(mode(&dir.join(STATE)), 0o600);
    assert_eq!(mode(&dir.join(LOCK)), 0o600);
    let lock_ino = std::fs::metadata(dir.join(LOCK)).unwrap().ino();

    let calls = hid.calls();
    a.focus(true);
    a.focus(true);
    assert_eq!(
        hid.calls(),
        calls,
        "a focus reported again (each frame) does nothing"
    );

    a.focus(false);
    assert_eq!(hid.value(), OFF);
    assert_eq!(hid.writes(), [ON, OFF]);
    assert!(!a.holds());
    assert_eq!(state(&dir), None);
    assert_eq!(
        std::fs::metadata(dir.join(LOCK)).unwrap().ino(),
        lock_ino,
        "the lock file stays, never replaced"
    );
    a.focus(false);
    assert_eq!(
        hid.writes(),
        [ON, OFF],
        "a blur reported again does nothing"
    );
}

/// 04 §8 K-F1 — two sopen processes, focus moving A → B with B's acquire before or after A's
/// release → the original 0 is recorded once and restored when the last lets go.
#[test]
fn kf1_two_processes_hand_focus_over_in_either_order() {
    for b_first in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let hid = Fake::at(OFF);
        let procs = Procs::default();
        let mut a = process(&dir, &hid, &procs, 1);
        let mut b = process(&dir, &hid, &procs, 2);
        let b_holder = Holder {
            pid: 2,
            start: 1002,
        };

        a.focus(true);
        assert_eq!(hid.value(), ON, "b_first={b_first}");
        if b_first {
            b.focus(true);
            assert!(b.holds());
            a.focus(false);
            assert_eq!(hid.value(), ON, "A let go while B holds: still 1");
            assert_eq!(
                hid.writes(),
                [ON],
                "B's acquire and A's release wrote nothing"
            );
            assert_eq!(
                hid.reads(),
                2,
                "the original was read once (and read back once)"
            );
            let s = state(&dir).unwrap();
            assert_eq!((s.original, s.holders), (OFF, vec![b_holder]));
        } else {
            a.focus(false);
            assert_eq!(hid.value(), OFF);
            assert_eq!(state(&dir), None);
            b.focus(true);
            assert_eq!(hid.value(), ON);
            let s = state(&dir).unwrap();
            assert_eq!(
                (s.original, s.holders),
                (OFF, vec![b_holder]),
                "B recorded the Mac's own 0"
            );
        }
        b.focus(false);
        assert_eq!(hid.value(), OFF, "b_first={b_first}: the last let go");
        assert_eq!(state(&dir), None);
        let want: &[i64] = if b_first {
            &[ON, OFF]
        } else {
            &[ON, OFF, ON, OFF]
        };
        assert_eq!(hid.writes(), want, "b_first={b_first}");
    }
}

/// 04 §8 K-F1 — the owner's own 1 is never changed and never recorded.
#[test]
fn kf1_the_owners_own_one_is_never_changed_or_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(ON);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1);
    let mut b = process(&dir, &hid, &procs, 2);
    a.focus(true);
    b.focus(true);
    assert!(!a.holds() && !b.holds());
    assert_eq!((a.status(), b.status()), (None, None));
    a.sleep();
    a.wake();
    b.session(false);
    b.session(true);
    a.focus(false);
    b.focus(false);
    drop(a);
    drop(b);
    assert_eq!(hid.value(), ON);
    assert!(hid.writes().is_empty(), "written: {:?}", hid.writes());
    assert_eq!(state(&dir), None, "nothing recorded");
}

/// 04 §8 K-F1 — sleep and wake release and reacquire; a session switched away from and back too
/// (04 §6.2's "When"); quitting releases.
#[test]
fn kf1_sleep_wake_and_session_switch_release_and_reacquire() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1);
    a.focus(true);
    assert_eq!(hid.value(), ON);
    a.sleep();
    assert_eq!(hid.value(), OFF, "released before sleep");
    assert_eq!(state(&dir), None);
    a.wake();
    assert_eq!(
        hid.value(),
        ON,
        "acquired again on wake: the window still has focus"
    );
    a.session(false);
    assert_eq!(
        hid.value(),
        OFF,
        "released when the session is switched away"
    );
    a.session(true);
    assert_eq!(hid.value(), ON, "acquired again on the session's return");
    a.focus(false);
    a.sleep();
    a.wake();
    assert_eq!(hid.value(), OFF, "a wake without focus acquires nothing");
    a.focus(true);
    assert_eq!(hid.value(), ON);
    a.quit();
    assert_eq!(hid.value(), OFF, "quitting releases");
    assert_eq!(state(&dir), None);
    assert_eq!(hid.writes(), [ON, OFF, ON, OFF, ON, OFF, ON, OFF]);
}

// ---------------------------------------------------------------------------------------------
// K-F6: stale state at both crash points.

/// 04 §8 K-F6 — the holder died (a) after writing the state, before setting the value (0), and
/// (b) after setting it (1); its watchdog is held back. A process acquires → keeps the recorded
/// original 0, sets 1 and reads 1 back; the delayed watchdog restores nothing while the new
/// holder lives; the new holder releases → 0.
#[test]
fn kf6_a_dead_holders_state_is_taken_over_at_both_crash_points() {
    for value in [OFF, ON] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let hid = Fake::at(OFF);
        let procs = Procs::default();
        // Started before the crash, so its start's clean-up found nothing.
        let mut n = process(&dir, &hid, &procs, 2);
        let n_holder = Holder {
            pid: 2,
            start: 1002,
        };
        let dead = procs.start(9);
        stale(&dir, vec![dead], BOOT);
        hid.with(|h| h.value = value);
        procs.kill(dead);

        let reads = hid.reads();
        n.focus(true);
        assert!(n.holds(), "crash value {value}: {:?}", n.status());
        assert_eq!(hid.value(), ON, "crash value {value}");
        assert_eq!(hid.writes(), [ON]);
        assert_eq!(hid.reads(), reads + 1, "read back after the set");
        let s = state(&dir).unwrap();
        assert_eq!(
            (s.original, s.holders),
            (OFF, vec![n_holder]),
            "crash value {value}: the recorded original kept, the new process the only holder"
        );

        // The dead holder's watchdog, held back until now.
        release(&dir, &mut hid.clone(), dead, &procs.machine()).unwrap();
        assert_eq!(
            hid.value(),
            ON,
            "crash value {value}: nothing restored under a live holder"
        );
        assert_eq!(state(&dir).unwrap().holders, [n_holder]);

        n.focus(false);
        assert_eq!(hid.value(), OFF, "crash value {value}");
        assert_eq!(state(&dir), None);
    }
}

/// 04 §8 K-F6 — with the fake set to refuse the write, or to read back 0 → the new process is
/// not a holder, the state is gone, the value is 0, and the status row says the switch did not
/// take effect. The same for a first acquisition with no state.
#[test]
fn kf6_a_refused_or_dropped_set_is_not_an_acquisition() {
    for value in [None, Some(OFF), Some(ON)] {
        for refuse in [true, false] {
            let case = format!("crash value {value:?}, refuse {refuse}");
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path().join("state");
            let hid = Fake::at(OFF);
            let procs = Procs::default();
            let mut n = process(&dir, &hid, &procs, 2);
            let dead = procs.start(9);
            if let Some(value) = value {
                stale(&dir, vec![dead], BOOT);
                hid.with(|h| h.value = value);
            }
            procs.kill(dead);
            hid.with(|h| {
                h.refuse = refuse;
                h.drops = !refuse;
            });

            n.focus(true);
            assert!(!n.holds(), "{case}");
            assert_eq!(state(&dir), None, "{case}: the state is gone");
            assert_eq!(hid.value(), OFF, "{case}");
            let status = n.status().unwrap_or_default();
            assert!(status.starts_with(NOT_IN_EFFECT), "{case}: {status:?}");

            // The delayed watchdog finds nothing; the blur releases nothing.
            release(&dir, &mut hid.clone(), dead, &procs.machine()).unwrap();
            let writes = hid.writes();
            n.focus(false);
            assert_eq!(
                hid.writes(),
                writes,
                "{case}: not a holder, nothing to release"
            );
            assert_eq!(hid.value(), OFF, "{case}");
            assert_eq!(state(&dir), None, "{case}");

            // A later focus tries again; this time the switch takes.
            hid.with(|h| (h.refuse, h.drops) = (false, false));
            n.focus(true);
            assert!(n.holds(), "{case}");
            assert_eq!(n.status(), None, "{case}: the status row is clear again");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// §6.3 "At start", and what the protocol does with a state it cannot trust.

/// 04 §6.3 "At start" — holders that no longer run are dropped; with none left the value is
/// restored. A state from another boot is dropped without touching the value: the reboot reset
/// it (a 1 now is the owner's).
#[test]
fn the_start_restores_what_dead_holders_left_and_keeps_live_ones() {
    let procs = Procs::default();
    let live = procs.start(1);
    let dead = procs.start(9);
    procs.kill(dead);
    for (holders, boot, want_value, want_holders) in [
        (vec![dead], BOOT, OFF, None),
        (vec![live, dead], BOOT, ON, Some(vec![live])),
        (vec![dead], "another boot", ON, None),
        (vec![], BOOT, OFF, None),
    ] {
        let case = format!("{holders:?} {boot}");
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let hid = Fake::at(ON);
        stale(&dir, holders, boot);
        let _n = process(&dir, &hid, &procs, 2);
        assert_eq!(hid.value(), want_value, "{case}");
        assert_eq!(state(&dir).map(|s| s.holders), want_holders, "{case}");
        if boot != BOOT {
            assert!(hid.writes().is_empty(), "{case}: {:?}", hid.writes());
        }
    }

    // No state: nothing read, nothing created.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let _n = process(&dir, &hid, &procs, 2);
    assert_eq!(hid.calls(), 0);
    assert!(!dir.exists());
}

/// A restore that fails keeps the state, without holders, so the next start retries it; a state
/// file sopen cannot read counts as original 0 without holders.
#[test]
fn a_failed_restore_is_retried_and_a_broken_state_reads_as_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1);
    a.focus(true);
    hid.with(|h| h.broken = true);
    a.focus(false);
    assert_eq!(hid.value(), ON, "the restore failed");
    let s = state(&dir).unwrap();
    assert_eq!(
        (s.original, s.holders.len()),
        (OFF, 0),
        "kept, without holders"
    );
    hid.with(|h| h.broken = false);
    let _b = process(&dir, &hid, &procs, 2);
    assert_eq!(hid.value(), OFF, "the next start restored it");
    assert_eq!(state(&dir), None);

    std::fs::write(dir.join(STATE), "{\"original\":").unwrap();
    hid.with(|h| h.value = ON);
    let _c = process(&dir, &hid, &procs, 3);
    assert_eq!(hid.value(), OFF);
    assert_eq!(state(&dir), None);
}

// ---------------------------------------------------------------------------------------------
// K-F4: `fkeys = "system"` never opens IOHIDSystem.

/// 04 §8 K-F4 — `sopen config fkeys system` → the file holds `fkeys = "system"` (its writer is
/// `config::set_fkeys`, the CLI's `run_config`) and the switch is never made. Through
/// [`from_config`], the construction [`for_this_run`] itself goes through, given the loaded file:
/// neither the switch's factory nor the rest is called, nothing is touched or created. The same
/// construction with the default makes the switch, once. (The test build's own short-circuit is
/// outside it: [`for_this_run`] gives no switch in a test build at all.)
#[test]
fn kf4_system_never_opens_the_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("config.toml");
    let dir = tmp.path().join("state");
    config::set_fkeys(&file, config::Fkeys::System).unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "fkeys = \"system\"\n"
    );
    let system = config::load(&file).unwrap();
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let (made, parted) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
    let open = || {
        made.set(made.get() + 1);
        Some(hid.clone())
    };
    let parts = || {
        parted.set(parted.get() + 1);
        Parts {
            me: Ok(procs.start(1)),
            machine: procs.machine(),
            watchdog: Watchdog::off(),
        }
    };
    assert!(from_config(&system, &dir, open, parts).is_none());
    assert_eq!((made.get(), parted.get()), (0, 0), "nothing was made");
    assert_eq!(hid.calls(), 0);
    assert!(!dir.exists(), "nothing was created");

    let default = config::load(&tmp.path().join("absent.toml")).unwrap();
    assert!(from_config(&default, &dir, open, parts).is_some());
    assert_eq!((made.get(), parted.get()), (1, 1), "the default makes it");
    assert_eq!(hid.calls(), 0, "and touches nothing before a focus");

    assert!(for_this_run(&system, tmp.path()).is_none());
    assert!(for_this_run(&default, tmp.path()).is_none());
    assert!(!tmp.path().join(LOCK).exists());
}

/// 04 §6.2 "Never in a scripted run": each hook, set to anything but nothing.
#[test]
fn scripted_runs_are_the_hooks() {
    let none = |_: &str| None::<OsString>;
    assert!(!scripted_run(none));
    for hook in SCRIPTED {
        assert!(
            scripted_run(|name| (name == hook).then(|| OsString::from("1"))),
            "{hook}"
        );
        assert!(
            !scripted_run(|name| (name == hook).then(OsString::new)),
            "{hook} empty"
        );
    }
    assert!(!scripted_run(
        |name| (name == "SOPEN_STATE").then(|| OsString::from("mode=dual"))
    ));
}

// ---------------------------------------------------------------------------------------------
// Holders and processes.

#[test]
fn holders_and_proc_stat() {
    let h = Holder {
        pid: 42,
        start: 1_700_000_000_123_456,
    };
    assert_eq!(h.to_string(), "42:1700000000123456");
    assert_eq!(Holder::parse(&h.to_string()), Some(h));
    for bad in ["", "42", "42:", ":5", "x:5", "42:5:6", "-1:5", "42:-5"] {
        assert_eq!(Holder::parse(bad), None, "{bad:?}");
    }

    // A name with spaces and a `)`; starttime is field 22.
    let line = "4242 (a b) c) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 \
                1000 100 18446744073709551615\n";
    assert_eq!(process::parse_stat(line), Some(98765));
    assert_eq!(process::parse_stat(&line.replace(") S ", ") Z ")), None);
    assert_eq!(process::parse_stat(&line.replace(") S ", ") X ")), None);
    assert_eq!(process::parse_stat("4242 (x) S 1 2 3\n"), None);
    assert_eq!(process::parse_stat("no parenthesis"), None);

    let me = Holder::this_process().unwrap();
    assert_eq!(me.pid, std::process::id());
    assert!(process::runs(&me));
    assert!(!process::runs(&Holder {
        start: me.start + 1,
        ..me
    }));
    assert!(process::boot().is_some_and(|b| !b.is_empty()));
}

/// A process that was killed does not run, before it is reaped (a zombie) and after.
#[test]
fn a_killed_process_does_not_run_zombie_or_reaped() {
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let holder = Holder {
        pid,
        start: process::start(pid).unwrap(),
    };
    assert!(process::runs(&holder));
    child.kill().unwrap();
    let zombie = wait_until(Duration::from_secs(5), || !process::runs(&holder));
    child.wait().unwrap();
    assert!(zombie, "a killed, unreaped child still counts as running");
    assert!(!process::runs(&holder));
}

// ---------------------------------------------------------------------------------------------
// The watchdog, in this process.

/// The watchdog ends at the end of its input, and also — the pipe still open, as when another
/// child kept a copy of its write end — once its holder no longer runs; either way it releases.
#[test]
fn the_watchdog_ends_at_its_inputs_end_or_its_holders() {
    for by_pipe in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let hid = Fake::at(OFF);
        let procs = Procs::default();
        let mut a = process(&dir, &hid, &procs, 1);
        a.focus(true);
        assert_eq!(hid.value(), ON);
        let holder = Holder {
            pid: 1,
            start: 1001,
        };

        let (input, pipe) = std::io::pipe().unwrap();
        let (d, mut h, m) = (dir.clone(), hid.clone(), procs.machine());
        let poll = if by_pipe {
            Duration::from_secs(60)
        } else {
            Duration::from_millis(10)
        };
        let pace = Pace {
            poll,
            ..Pace::DEFAULT
        };
        let watchdog =
            std::thread::spawn(move || watch(&d, &mut h, holder, &m, input, || {}, &pace));
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !watchdog.is_finished(),
            "by_pipe={by_pipe}: waits while held"
        );
        assert_eq!(hid.value(), ON);

        // The holder dies: its end of the pipe closes, or (the copy kept open) it just stops.
        procs.kill(holder);
        let kept = if by_pipe {
            drop(pipe);
            None
        } else {
            Some(pipe)
        };
        assert!(
            wait_until(Duration::from_secs(5), || watchdog.is_finished()),
            "by_pipe={by_pipe}: the watchdog did not end"
        );
        watchdog.join().unwrap().unwrap();
        assert_eq!(hid.value(), OFF, "by_pipe={by_pipe}");
        assert_eq!(state(&dir), None, "by_pipe={by_pipe}");
        drop(kept);
        std::mem::forget(a); // the holder "died": no release of its own
    }
}

// ---------------------------------------------------------------------------------------------
// Across processes: K-F2 and K-F5.

const ROLE: &str = "SOPEN_TEST_FKEYS_ROLE";
const ROOT: &str = "SOPEN_TEST_FKEYS_ROOT";
const HOLDER: &str = "SOPEN_TEST_FKEYS_HOLDER";
const SEED: &str = "SOPEN_TEST_FKEYS_SEED";
const LOCK_INO: &str = "SOPEN_TEST_FKEYS_LOCK_INO";
const CHILD_TEST: &str = "fnkeys::tests::child";
const HELD: &str = "FKEYS-HELD ";
const JOINED: &str = "FKEYS-JOINED ";
const CYCLES: usize = 1_000;
/// The cycle at which each contender, holding, waits for the other to hold too.
const BARRIER_CYCLE: usize = CYCLES / 2;

/// The fake shared across processes: the value in `<root>/hid`, read and written only under
/// the protocol's lock (and by the test once its children have ended). Every write is logged in
/// `<root>/hid.log` with its time (nanoseconds since the epoch), so a test times the write itself.
struct FileSwitch(PathBuf);

impl Switch for FileSwitch {
    fn read(&mut self) -> Result<i64> {
        Ok(std::fs::read_to_string(&self.0)?.trim().parse()?)
    }

    fn write(&mut self, value: i64) -> Result<()> {
        std::fs::write(&self.0, value.to_string())?;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.0.with_file_name("hid.log"))?;
        writeln!(log, "{value} {}", now_nanos())?;
        Ok(())
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn hid_file(root: &Path) -> String {
    std::fs::read_to_string(root.join("hid"))
        .unwrap()
        .trim()
        .to_string()
}

/// This test binary again, running [`child`] as `role` on `root`.
fn child_command(role: &str, root: &Path) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(ROLE, role)
        .env(ROOT, root);
    cmd
}

/// Not a test of its own (it returns at once): the body of the processes K-F2 and K-F5 start.
#[test]
fn child() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    match role.as_str() {
        "holder" => holder_child(&root),
        "watchdog" => watchdog_child(&root),
        "contender" => contender_child(&root),
        other => panic!("unknown role {other}"),
    }
}

/// K-F2's holder: acquires through the whole of `Fkeys` (its watchdog this binary as
/// "watchdog", which says it is ready before the acquisition), says so, and waits to be killed.
fn holder_child(root: &Path) {
    let me = Holder::this_process().unwrap();
    let watchdog_root = root.to_path_buf();
    let command: WatchdogCommand = Box::new(move |_dir, holder| {
        let mut cmd = child_command("watchdog", &watchdog_root);
        cmd.env(HOLDER, holder.to_string());
        Ok(cmd)
    });
    let mut f = Fkeys::new(
        root.join("state"),
        FileSwitch(root.join("hid")),
        Ok(me),
        Machine::this(),
        // A test binary starts slower than sopen: a longer wait for it to say it is ready.
        Watchdog::start(command).ready_within(Duration::from_secs(30)),
    );
    f.focus(true);
    assert!(f.holds(), "{:?}", f.status());
    let pid = f.watchdog_pid().unwrap();
    println!("\n{HELD}{pid}");
    std::thread::sleep(Duration::from_secs(60));
}

/// K-F2's watchdog: says it is ready as sopen's does, then waits on its input with a poll far
/// longer than the test's second, so what ends it in time is the pipe's end.
fn watchdog_child(root: &Path) {
    let holder = Holder::parse(&std::env::var(HOLDER).unwrap()).unwrap();
    watch(
        &root.join("state"),
        &mut FileSwitch(root.join("hid")),
        holder,
        &Machine::this(),
        io::stdin(),
        // libtest has printed `test … ... ` without its line's end: the ready line starts anew.
        || {
            println!();
            say_ready()
        },
        &Pace {
            poll: Duration::from_secs(60),
            ..Pace::DEFAULT
        },
    )
    .unwrap();
}

/// 04 §8 K-F2 — a process holding the switch is killed with SIGKILL → within 1 s the fake
/// is back at 0 and the state file is gone; then the watchdog ends. The second runs from just
/// before the kill to the restoring write, as the fake logged it: this test's polling and
/// scheduling are not in it.
#[test]
fn kf2_a_killed_holder_is_restored_by_its_watchdog_within_a_second() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("hid"), "0").unwrap();
    let mut holder = child_command("holder", root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let lines = lines_of(holder.stdout.take().unwrap());
    let watchdog: u32 = loop {
        match lines.recv_timeout(Duration::from_secs(60)) {
            Ok(line) => {
                if let Some(pid) = line.strip_prefix(HELD) {
                    break pid.trim().parse().unwrap();
                }
            }
            Err(_) => {
                let _ = holder.kill();
                let _ = holder.wait();
                let mut err = String::new();
                let _ = holder.stderr.take().unwrap().read_to_string(&mut err);
                panic!("the holder did not hold:\n{err}");
            }
        }
    };
    let state_file = root.join("state").join(STATE);
    assert_eq!(hid_file(root), "1");
    assert!(state_file.exists());

    let before_kill = now_nanos();
    holder.kill().unwrap(); // SIGKILL
    holder.wait().unwrap();
    let restored = wait_until(Duration::from_secs(10), || {
        hid_file(root) == "0" && !state_file.exists()
    });
    assert!(restored, "not restored: hid {}", hid_file(root));
    let log = std::fs::read_to_string(root.join("hid.log")).unwrap();
    let restore = log
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(value, _)| *value == "0")
        .filter_map(|(_, at)| at.parse::<u128>().ok())
        .find(|at| *at >= before_kill)
        .unwrap_or_else(|| panic!("no restoring write after the kill:\n{log}"));
    let took = Duration::from_nanos(u64::try_from(restore - before_kill).unwrap());
    assert!(
        took <= Duration::from_secs(1),
        "restored only {took:?} after the kill"
    );
    assert!(
        wait_until(Duration::from_secs(5), || process::start(watchdog)
            .is_none()),
        "the watchdog {watchdog} still runs"
    );
}

/// K-F5's contender: [`CYCLES`] acquisitions and releases, the gaps between them (extra looks at
/// the state under the lock, yields) drawn from its seed; every look checks the invariants. At
/// [`BARRIER_CYCLE`] it marks that it holds and, holding, waits for the other's mark, so the two
/// hold at once at least there; it prints how often it joined holders that remained.
fn contender_child(root: &Path) {
    let dir = root.join("state");
    let seed: u64 = std::env::var(SEED).unwrap().parse().unwrap();
    let ino: u64 = std::env::var(LOCK_INO).unwrap().parse().unwrap();
    let me = Holder::this_process().unwrap();
    let m = Machine::this();
    let mut hid = FileSwitch(root.join("hid"));
    let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut joined = 0;
    for cycle in 0..CYCLES {
        let got = acquire(&dir, &mut hid, me, &m).unwrap();
        assert!(got.holds(), "cycle {cycle}: {got:?}");
        if got == Acquired::Joined {
            joined += 1;
        }
        if cycle == BARRIER_CYCLE {
            std::fs::write(root.join(format!("barrier-{seed}")), "").unwrap();
            let other = root.join(format!("barrier-{}", 3 - seed));
            assert!(
                wait_until(Duration::from_secs(20), || other.exists()),
                "the other contender never held at the barrier"
            );
        }
        for _ in 0..next() % 3 {
            look(&dir, &mut hid, &m, ino, cycle);
        }
        if next() % 2 == 0 {
            std::thread::yield_now();
        }
        release(&dir, &mut hid, me, &m).unwrap();
        for _ in 0..next() % 3 {
            look(&dir, &mut hid, &m, ino, cycle);
        }
    }
    println!("\n{JOINED}{joined}");
}

/// Under the lock: the lock file is the one the test made; a state names only holders that run,
/// records 0 and goes with the value 1; no state goes with 0.
fn look(dir: &Path, hid: &mut FileSwitch, m: &Machine, ino: u64, cycle: usize) {
    let _lock = lock(dir).unwrap();
    assert_eq!(
        std::fs::metadata(dir.join(LOCK)).unwrap().ino(),
        ino,
        "cycle {cycle}: the lock file was replaced"
    );
    let value = hid.read().unwrap();
    match read_state(dir).unwrap() {
        Some(s) => {
            assert!(!s.holders.is_empty(), "cycle {cycle}: {s:?}");
            for h in &s.holders {
                assert!(
                    (m.runs)(h),
                    "cycle {cycle}: a holder that does not run: {h} in {s:?}"
                );
            }
            assert_eq!(s.original, OFF, "cycle {cycle}");
            assert_eq!(
                value, ON,
                "cycle {cycle}: a state while the value is {value}"
            );
        }
        None => assert_eq!(
            value, OFF,
            "cycle {cycle}: the value {value} without a state"
        ),
    }
}

/// 04 §8 K-F5 — two processes acquire and release 1 000 times each, at random, against the
/// fake → the lock file is never replaced or removed, the fake ends at its original value, and no
/// run sees a state file with a holder that does not run. The barrier makes them hold at once at
/// least once, and the contested path — an acquisition that joins holders that remain — must
/// have run. Both children's outcomes are collected (each killed and reaped past its bound) before
/// anything is asserted.
#[test]
fn kf5_two_processes_acquire_and_release_a_thousand_times_each() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let dir = root.join("state");
    std::fs::write(root.join("hid"), "0").unwrap();
    drop(lock(&dir).unwrap());
    let ino = std::fs::metadata(dir.join(LOCK)).unwrap().ino();
    let started = Instant::now();
    let children = [1u64, 2].map(|seed| {
        child_command("contender", root)
            .env(SEED, seed.to_string())
            .env(LOCK_INO, ino.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    });
    let outcomes: Vec<_> = children
        .into_iter()
        .map(|child| finish(child, Duration::from_secs(120)))
        .collect();
    let mut joined = 0;
    for (n, (ok, text)) in outcomes.iter().enumerate() {
        assert!(ok, "contender {n}:\n{text}");
        assert!(
            text.contains("1 passed"),
            "contender {n} did not run:\n{text}"
        );
        joined += text
            .lines()
            .find_map(|line| line.strip_prefix(JOINED))
            .and_then(|n| n.trim().parse::<usize>().ok())
            .unwrap_or_else(|| panic!("contender {n} did not count:\n{text}"));
    }
    eprintln!(
        "K-F5: 2 × {CYCLES} cycles in {:?}, {joined} joined",
        started.elapsed()
    );
    assert!(joined >= 1, "the contested path never ran");
    assert_eq!(hid_file(root), "0", "the fake ends at its original value");
    assert_eq!(
        std::fs::metadata(dir.join(LOCK)).unwrap().ino(),
        ino,
        "the lock file was never replaced or removed"
    );
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, [LOCK], "no state and no temp left");
}

// ---------------------------------------------------------------------------------------------
// Helpers.

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn lines_of(out: std::process::ChildStdout) -> mpsc::Receiver<String> {
    let (send, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in io::BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    lines
}

/// Waits for `child` (killed and reaped at `limit`): whether it succeeded, and its output.
fn finish(mut child: Child, limit: Duration) -> (bool, String) {
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let out = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let ended = wait_until(limit, || child.try_wait().unwrap().is_some());
    if !ended {
        let _ = child.kill();
    }
    let status = child.wait().unwrap();
    let text = format!("{}{}", out.join().unwrap(), err.join().unwrap());
    (ended && status.success(), text)
}

// ---------------------------------------------------------------------------------------------
// Review round 1 (PR #65): a restore that keeps its responsibility, a watchdog that is ready, a
// state that cannot record 1.

/// The token a watchdog prints once it is ready, as the stand-ins below spell it.
const READY_LINE: &str = "sopen-fkeys-watchdog-ready";

#[test]
fn the_stand_ins_say_ready_as_sopen_does() {
    assert_eq!(READY_LINE, READY);
}

/// A stand-in watchdog: `sh -c <script>`.
fn stand_in(script: &'static str) -> WatchdogCommand {
    Box::new(move |_, _| {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", script]);
        Ok(cmd)
    })
}

/// R1 — a restore the watchdog could not make (a transient failure) is retried until it holds;
/// it is not logged and dropped.
#[test]
fn r1_the_watchdog_retries_a_failed_restore_until_it_holds() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1);
    a.focus(true);
    assert_eq!(hid.value(), ON);
    let holder = Holder {
        pid: 1,
        start: 1001,
    };
    procs.kill(holder);
    std::mem::forget(a); // it "died": no release of its own
    hid.with(|h| h.fail_writes = 3);

    let (input, pipe) = std::io::pipe().unwrap();
    drop(pipe);
    let done = watch(
        &dir,
        &mut hid.clone(),
        holder,
        &procs.machine(),
        input,
        || {},
        &Pace::DEFAULT,
    );
    assert_eq!(hid.value(), OFF, "{done:?}");
    assert_eq!(state(&dir), None);
    assert_eq!(
        hid.writes(),
        [ON, OFF, OFF, OFF, OFF],
        "three failures, then the restore"
    );
    done.unwrap();
}

/// R1 — the watchdog's retries are bounded and never spin: past their total it gives up and
/// leaves the state, without holders, for the next start, which restores.
#[test]
fn r1_the_watchdog_gives_up_after_its_total_and_leaves_the_state() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1);
    a.focus(true);
    let holder = Holder {
        pid: 1,
        start: 1001,
    };
    procs.kill(holder);
    std::mem::forget(a);
    hid.with(|h| h.broken = true);

    let (input, pipe) = std::io::pipe().unwrap();
    drop(pipe);
    let pace = Pace {
        poll: Duration::from_secs(60),
        first: Duration::from_millis(10),
        most: Duration::from_millis(40),
        total: Duration::from_millis(200),
    };
    let started = Instant::now();
    let done = watch(
        &dir,
        &mut hid.clone(),
        holder,
        &procs.machine(),
        input,
        || {},
        &pace,
    );
    let took = started.elapsed();
    let e = format!("{:#}", done.unwrap_err());
    assert!(e.contains("gave up"), "{e}");
    assert!(
        took >= pace.total && took < Duration::from_secs(5),
        "{took:?}"
    );
    let tries = hid.writes().len() - 1;
    assert!(
        (2..=12).contains(&tries),
        "{tries} tries in {took:?}: no spinning"
    );
    let s = state(&dir).unwrap();
    assert!(s.holders.is_empty(), "{s:?}");
    assert_eq!(hid.value(), ON);

    hid.with(|h| h.broken = false);
    let _next = process(&dir, &hid, &procs, 2);
    assert_eq!(hid.value(), OFF, "the next start restored");
    assert_eq!(state(&dir), None);
}

/// R1 — a release that failed in the process itself is tried again at a later call once it is
/// due (not before), and at the end; never left to the watchdog alone while the process lives.
#[test]
fn r1_a_failed_release_is_tried_again_at_a_later_call_and_at_the_end() {
    let pace = Pace {
        poll: Duration::from_secs(1),
        first: Duration::from_millis(20),
        most: Duration::from_millis(40),
        total: Duration::from_secs(10),
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1).with_pace(pace);
    a.focus(true);
    hid.with(|h| h.fail_writes = 2);
    a.focus(false);
    assert_eq!(hid.value(), ON);
    let at = a.next_retry().unwrap();
    let writes = hid.writes().len();
    a.focus(false);
    if Instant::now() < at {
        assert_eq!(
            hid.writes().len(),
            writes,
            "not tried again before it is due"
        );
    }
    while let Some(at) = a.next_retry() {
        std::thread::sleep(at.saturating_duration_since(Instant::now()) + Duration::from_millis(2));
        a.focus(false);
    }
    assert_eq!(hid.value(), OFF);
    assert_eq!(state(&dir), None);
    assert_eq!(a.next_retry(), None);
    assert_eq!(hid.writes(), [ON, OFF, OFF, OFF]);

    // At the end: one more try before the watchdog's.
    a.focus(true);
    hid.with(|h| h.fail_writes = 1);
    a.focus(false);
    assert_eq!(hid.value(), ON);
    drop(a);
    assert_eq!(hid.value(), OFF, "the drop tried again");
    assert_eq!(state(&dir), None);
}

/// R2 — a restore that returns but does not take (or cannot be read back) keeps the state, so it
/// is retried; the next start restores.
#[test]
fn r2_a_restore_that_does_not_read_back_keeps_the_state() {
    for blind in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let hid = Fake::at(OFF);
        let procs = Procs::default();
        let mut a = process(&dir, &hid, &procs, 1);
        a.focus(true);
        hid.with(|h| {
            h.ignores = !blind;
            h.blind = blind;
        });
        a.focus(false);
        let s = state(&dir).unwrap_or_else(|| {
            panic!("blind={blind}: the state was removed with nothing verifying the restore")
        });
        assert!(s.holders.is_empty(), "blind={blind}: {s:?}");
        assert_eq!(s.original, OFF);
        hid.with(|h| {
            h.ignores = false;
            h.blind = false;
        });
        std::mem::forget(a); // it "died": what restores now is the next start
        let _b = process(&dir, &hid, &procs, 2);
        assert_eq!(hid.value(), OFF, "blind={blind}: the next start restored");
        assert_eq!(state(&dir), None, "blind={blind}");
    }
}

/// R3 — a watchdog that dies before it says it is ready is no watchdog: no acquisition, the value
/// stays 0, and the status row says the switch did not take effect.
#[test]
fn r3_a_watchdog_that_dies_before_ready_is_no_acquisition() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let me = procs.start(1);
    let mut a = Fkeys::new(
        dir.clone(),
        hid.clone(),
        Ok(me),
        procs.machine(),
        Watchdog::start(stand_in("exit 3")),
    );
    a.focus(true);
    assert!(!a.holds());
    let status = a.status().unwrap_or_default();
    assert!(status.starts_with(NOT_IN_EFFECT), "{status:?}");
    assert!(hid.writes().is_empty(), "{:?}", hid.writes());
    assert_eq!(hid.value(), OFF);
    assert_eq!(state(&dir), None);
}

/// R3 — a watchdog that never says it is ready is stopped at the bound: no acquisition.
#[test]
fn r3_a_watchdog_that_never_says_ready_is_stopped_at_the_bound() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let me = procs.start(1);
    let wait = Duration::from_millis(300);
    let mut a = Fkeys::new(
        dir.clone(),
        hid.clone(),
        Ok(me),
        procs.machine(),
        // Waits for its input's end with its output open, and says nothing.
        Watchdog::start(stand_in("exec cat")).ready_within(wait),
    );
    let started = Instant::now();
    a.focus(true);
    let took = started.elapsed();
    assert!(!a.holds());
    assert!(took >= wait && took < Duration::from_secs(10), "{took:?}");
    let status = a.status().unwrap_or_default();
    assert!(
        status.starts_with(NOT_IN_EFFECT) && status.contains("ready"),
        "{status:?}"
    );
    assert_eq!(a.watchdog_pid(), None);
    assert!(hid.writes().is_empty());
    assert_eq!(hid.value(), OFF);
    assert_eq!(state(&dir), None);
}

/// R3 — a watchdog that was ready once but has died since is not taken for a live one: before the
/// next acquisition it is started again (and ready), or there is no acquisition.
#[test]
fn r3_a_dead_watchdog_is_replaced_before_the_next_acquisition() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let me = procs.start(1);
    let started = Arc::new(Mutex::new(0usize));
    let count = started.clone();
    let scripts = [
        // Ready, then gone.
        "echo sopen-fkeys-watchdog-ready",
        // Gone before ready.
        "exit 3",
        // Ready, and waits for its input's end.
        "echo sopen-fkeys-watchdog-ready; exec cat >/dev/null",
    ];
    assert!(scripts[0].ends_with(READY_LINE));
    let command: WatchdogCommand = Box::new(move |_, _| {
        let mut n = count.lock().unwrap();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", scripts[(*n).min(2)]]);
        *n += 1;
        Ok(cmd)
    });
    let mut a = Fkeys::new(
        dir.clone(),
        hid.clone(),
        Ok(me),
        procs.machine(),
        // A loaded machine may start `sh` slowly: the bound is not what is tested here.
        Watchdog::start(command).ready_within(Duration::from_secs(30)),
    );
    a.focus(true);
    assert!(a.holds(), "{:?}", a.status());
    a.focus(false);
    assert_eq!(hid.value(), OFF);
    let pid = a.watchdog_pid().unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || process::start(pid).is_none()),
        "the first watchdog did not end"
    );

    a.focus(true);
    assert!(!a.holds(), "acquired with a dead watchdog");
    assert_eq!(*started.lock().unwrap(), 2, "started again once");
    assert!(a.status().unwrap_or_default().starts_with(NOT_IN_EFFECT));
    assert_eq!(hid.writes(), [ON, OFF]);
    assert_eq!(hid.value(), OFF);

    a.focus(false);
    a.focus(true);
    assert!(a.holds(), "a third, ready watchdog: {:?}", a.status());
    assert_eq!(*started.lock().unwrap(), 3);
    a.quit();
    assert_eq!(hid.value(), OFF);
}

/// R4 — sopen records only 0, so a state that says otherwise is not trusted: whatever it says,
/// the restore is 0 and 1 is never written back.
#[test]
fn r4_a_state_recording_one_never_restores_one() {
    let procs = Procs::default();
    let dead = procs.start(9);
    procs.kill(dead);
    for by_watchdog in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(STATE),
            format!(r#"{{"original":1,"holders":[{{"pid":9,"start":1009}}],"boot":"{BOOT}"}}"#),
        )
        .unwrap();
        let hid = Fake::at(ON);
        if by_watchdog {
            release(&dir, &mut hid.clone(), dead, &procs.machine()).unwrap();
        } else {
            let _n = process(&dir, &hid, &procs, 2);
        }
        assert_eq!(hid.writes(), [OFF], "by_watchdog={by_watchdog}");
        assert_eq!(hid.value(), OFF);
        assert_eq!(state(&dir), None);
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 2 (PR #65): the watchdog's wait for the lock is bounded; a state from another boot
// is dropped whatever it records.

/// R2-1 — another sopen holds the lock and is stopped: the watchdog does not block on it past
/// its total. It ends at the deadline without writing, and the state stays for the next start.
#[test]
fn r5_the_watchdog_does_not_wait_for_a_held_lock_past_its_total() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    let hid = Fake::at(OFF);
    let procs = Procs::default();
    let mut a = process(&dir, &hid, &procs, 1);
    a.focus(true);
    let holder = Holder {
        pid: 1,
        start: 1001,
    };
    procs.kill(holder);
    std::mem::forget(a);
    let writes = hid.writes();

    // A stopped sopen in the middle of a step: the lock is held and stays held.
    let held = lock(&dir).unwrap();
    let (input, pipe) = std::io::pipe().unwrap();
    drop(pipe);
    let pace = Pace {
        poll: Duration::from_secs(60),
        first: Duration::from_millis(10),
        most: Duration::from_millis(40),
        total: Duration::from_millis(200),
    };
    let (d, mut h, m) = (dir.clone(), hid.clone(), procs.machine());
    let started = Instant::now();
    let watchdog = std::thread::spawn(move || watch(&d, &mut h, holder, &m, input, || {}, &pace));
    let ended_in_time = wait_until(Duration::from_secs(5), || watchdog.is_finished());
    let took = started.elapsed();
    let writes_while_held = hid.writes();
    drop(held); // a watchdog still blocked goes on from here (and must not have before)
    let done = watchdog.join().unwrap();

    assert!(ended_in_time, "still waiting for the lock after {took:?}");
    assert!(took >= pace.total, "{took:?}");
    let e = format!("{:#}", done.unwrap_err());
    assert!(e.contains("gave up"), "{e}");
    assert_eq!(
        writes_while_held, writes,
        "nothing written without the lock"
    );
    assert_eq!(hid.writes(), writes);
    assert_eq!(hid.value(), ON);
    assert!(state(&dir).is_some(), "the state stays for the next start");

    let _next = process(&dir, &hid, &procs, 2);
    assert_eq!(hid.value(), OFF, "the next start restored");
    assert_eq!(state(&dir), None);
}

/// R2-2 — a state from another boot is removed without touching the value, whatever original it
/// records: after a reboot the owner's own 1 is not written over, by the start's clean-up, a
/// watchdog's release or an acquisition.
#[test]
fn r6_a_state_from_another_boot_is_dropped_whatever_it_records() {
    let procs = Procs::default();
    let dead = procs.start(9);
    procs.kill(dead);
    for path in ["start", "watchdog", "acquire"] {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(STATE),
            r#"{"original":1,"holders":[{"pid":9,"start":1009}],"boot":"another boot"}"#,
        )
        .unwrap();
        let hid = Fake::at(ON); // the owner's own 1, set again after the reboot
        match path {
            "start" => drop(process(&dir, &hid, &procs, 2)),
            "watchdog" => release(&dir, &mut hid.clone(), dead, &procs.machine()).unwrap(),
            _ => {
                let me = procs.start(3);
                let got = acquire(&dir, &mut hid.clone(), me, &procs.machine()).unwrap();
                assert_eq!(got, Acquired::OwnersOwn, "{path}");
            }
        }
        assert!(hid.writes().is_empty(), "{path}: wrote {:?}", hid.writes());
        assert_eq!(hid.value(), ON, "{path}");
        assert!(!dir.join(STATE).exists(), "{path}: the file is removed");
    }
}
