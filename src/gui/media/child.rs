//! One ffprobe or ffmpeg that can be stopped from another thread: what a preview's preparation
//! ([`super::preview`]), the waveform's decode ([`super::wave`]) and the media window's
//! encoder question run their programs in.
//!
//! - **The slot owns the child until it has exited.** Whoever runs it polls for its end
//!   ([`Slot::poll`], [`Slot::wait_end`]) and never takes it out to wait: a program that closed
//!   its output but lives on is still here to be killed.
//! - **A stop only signals** ([`Slot::stop`]): it sets the slot's flag and sends the kill; it
//!   never waits for the process to end, and for the slot's lock no longer than
//!   [`STOP_WAIT`] — it is called on a window's frame (the bar left a preview, the window
//!   closes). The thread that runs the child reaps it, at its next look.
//! - **Nothing starts once stopped, and no stop is lost**: a child is started under the slot's
//!   lock, after a look at the flag, and the flag is looked at again once the child is in the
//!   slot — a stop that came during the start is applied there. A child is never read from
//!   without a thread watching it ([`Slot::watched`]), so a stop is applied within [`POLL`]
//!   whatever its reader blocks on.
//! - **Unwinding leaves no child**: every runner holds an [`Ending`] from the start of its
//!   child on, and a reader that unwinds stops the child ([`StopOnUnwind`]).

use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

/// How often a running child is looked at.
pub const POLL: Duration = Duration::from_millis(5);

/// How long [`Slot::stop`] waits for the slot's lock (held by the thread that starts the
/// child, for the time of that start): past it the flag alone stops the child, at that
/// thread's next look.
const STOP_WAIT: Duration = Duration::from_millis(20);

/// The child that runs now (one at a time), and whether it was told to stop.
#[derive(Default)]
pub struct Slot {
    live: Mutex<Option<Child>>,
    stopped: AtomicBool,
    /// Tests: the thread that watches the child cannot be started.
    #[cfg(test)]
    pub(crate) fail_watcher: AtomicBool,
    /// Tests: whoever reads the child's output panics before reading.
    #[cfg(test)]
    pub(crate) panic_in_read: AtomicBool,
    /// Tests: called once in [`Slot::start`], under its lock, after its look at the flag and
    /// before the child is started (a start that takes its while).
    #[cfg(test)]
    pub(crate) while_starting: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// What a look at the child found.
pub enum Polled {
    Running,
    Ended(ExitStatus),
    /// No child: none was started, or its end was already taken.
    Gone,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, Option<Child>> {
        self.live.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Tests: the slot's lock, held as the thread starting a child holds it.
    #[cfg(test)]
    pub(crate) fn hold(&self) -> MutexGuard<'_, Option<Child>> {
        self.lock()
    }

    /// Told to stop.
    pub fn stopped(&self) -> bool {
        self.stopped.load(SeqCst)
    }

    /// Starts `cmd` (which reads `file`) with its output piped, unless the slot was stopped:
    /// then `Ok(None)`, and nothing was started.
    pub fn start(
        &self,
        cmd: &mut Command,
        file: Option<&std::path::Path>,
    ) -> std::io::Result<Option<(ChildStdout, ChildStderr)>> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut live = self.lock();
        if self.stopped() {
            return Ok(None);
        }
        #[cfg(test)]
        if let Some(pause) = self.while_starting.lock().unwrap().take() {
            pause();
        }
        end_children_on_signals();
        let mut child = crate::ffmpeg::spawn(cmd)?;
        running().insert(child.id());
        #[cfg(test)]
        log::started(file, child.id());
        let _ = file;
        let pipes = (
            child.stdout.take().expect("piped"),
            child.stderr.take().expect("piped"),
        );
        // A stop that came while it was being started (it gave up waiting for this lock, and
        // only left its flag) is applied here: the child is never left to run unstopped.
        if self.stopped() {
            let _ = child.kill();
        }
        *live = Some(child);
        Ok(Some(pipes))
    }

    /// Runs `read` (it reads the child's output, and may block for as long as the child lives)
    /// with the child watched from a thread of its own, so nothing ever blocks on a child that
    /// nobody looks at: a stop is applied within [`POLL`], and the child is reaped when it has
    /// ended — which is what this waits for after `read`. A `read` that unwinds stops the child.
    /// Returns what `read` made and how the child ended (`None`: it was gone).
    pub fn watched<T>(&self, read: impl FnOnce() -> T) -> std::io::Result<(T, Option<ExitStatus>)> {
        std::thread::scope(|scope| {
            #[cfg(test)]
            if self.fail_watcher.load(SeqCst) {
                return Err(std::io::Error::other("no thread (a test)"));
            }
            let watcher = std::thread::Builder::new().spawn_scoped(scope, || self.wait_end())?;
            let _unwinding = StopOnUnwind(self);
            #[cfg(test)]
            if self.panic_in_read.load(SeqCst) {
                panic!("reading the child's output (a test)");
            }
            let got = read();
            Ok((got, watcher.join().expect("the watcher")))
        })
    }

    /// Stops the child: the flag, and the kill if it runs. Never waits for the process, and
    /// for the lock at most [`STOP_WAIT`]; the thread that runs the child reaps it.
    pub fn stop(&self) {
        self.stopped.store(true, SeqCst);
        let give_up = Instant::now() + STOP_WAIT;
        loop {
            let live = match self.live.try_lock() {
                Ok(live) => Some(live),
                Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some(mut live) = live {
                if let Some(child) = live.as_mut() {
                    let _ = child.kill();
                }
                return;
            }
            if Instant::now() >= give_up {
                return;
            }
            std::thread::yield_now();
        }
    }

    /// A look at the child, by the thread that runs it: killed if the slot was stopped, and
    /// taken out — reaped — once it has ended.
    pub fn poll(&self) -> Polled {
        let mut live = self.lock();
        let Some(child) = live.as_mut() else {
            return Polled::Gone;
        };
        if self.stopped() {
            let _ = child.kill();
        }
        match child.try_wait() {
            Ok(None) => Polled::Running,
            Ok(Some(status)) => {
                running().remove(&child.id());
                #[cfg(test)]
                log::ended(child.id());
                live.take();
                Polled::Ended(status)
            }
            // It cannot be waited for: killed, and let go.
            Err(_) => {
                let _ = child.kill();
                running().remove(&child.id());
                #[cfg(test)]
                log::ended(child.id());
                live.take();
                Polled::Gone
            }
        }
    }

    /// Looks at the child until it has ended (`None`: there was none).
    pub fn wait_end(&self) -> Option<ExitStatus> {
        loop {
            match self.poll() {
                Polled::Running => std::thread::sleep(POLL),
                Polled::Ended(status) => return Some(status),
                Polled::Gone => return None,
            }
        }
    }

    /// Kills the child and waits for it, if one is still here: the last thing its runner does,
    /// on every way out.
    pub fn end(&self) {
        if let Some(mut child) = self.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
            running().remove(&child.id());
            #[cfg(test)]
            log::ended(child.id());
        }
    }

    /// Tests: the process id of the child that runs now.
    #[cfg(test)]
    pub(crate) fn pid(&self) -> Option<u32> {
        self.lock().as_ref().map(Child::id)
    }
}

/// The process ids of every child that runs in a [`Slot`] of this process: from its start to
/// the moment its end is taken.
static LIVE: Mutex<std::collections::BTreeSet<u32>> = Mutex::new(std::collections::BTreeSet::new());

fn running() -> MutexGuard<'static, std::collections::BTreeSet<u32>> {
    LIVE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Stops and reaps every child that runs in any [`Slot`] of this process: what sopen does
/// before it ends without unwinding (a signal, the screenshot hook's watchdog) — a child left
/// to itself would go on under another parent.
pub fn end_all() {
    let pids: Vec<u32> = running().iter().copied().collect();
    for pid in &pids {
        // Our own children, not yet reaped by their runners (those take them out of the
        // list under its lock first).
        unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL) };
    }
    for pid in pids {
        let mut status = 0;
        // Reaped here unless its runner was faster (then there is no such child: fine).
        unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
        running().remove(&pid);
    }
}

/// Where the signal handler writes the signal's number: the thread that ends the children
/// reads it (a handler may do nothing but write).
static SIGNALLED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn signalled(signal: libc::c_int) {
    let byte = [signal as u8];
    let to = SIGNALLED.load(SeqCst);
    unsafe { libc::write(to, byte.as_ptr().cast(), 1) };
}

/// From the first child on, a signal that ends sopen (SIGTERM, SIGHUP) ends its children
/// first ([`end_all`]); sopen then dies of that signal as it would have. A signal that is
/// ignored (a `nohup` run's SIGHUP) stays ignored.
fn end_children_on_signals() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut ends = [0 as libc::c_int; 2];
        if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
            return;
        }
        let (from, to) = (ends[0], ends[1]);
        unsafe {
            libc::fcntl(from, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(to, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        SIGNALLED.store(to, SeqCst);
        let waits = std::thread::Builder::new()
            .name("sopen-signals".to_string())
            .spawn(move || {
                let mut byte = [0u8; 1];
                loop {
                    let read = unsafe { libc::read(from, byte.as_mut_ptr().cast(), 1) };
                    if read == 1 {
                        break;
                    }
                    if read == 0 {
                        return;
                    }
                }
                end_all();
                // And what its previews keep in the cache (nothing unwinds from here).
                crate::cache::remove_preview_spaces();
                let signal = libc::c_int::from(byte[0]);
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                    libc::raise(signal);
                }
            });
        if waits.is_err() {
            return;
        }
        let handler = signalled as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for signal in [libc::SIGTERM, libc::SIGHUP] {
            unsafe {
                if libc::signal(signal, handler) == libc::SIG_IGN {
                    libc::signal(signal, libc::SIG_IGN);
                }
            }
        }
    });
}

/// A run's hold on its slot: dropped — the run returned, or unwound — whatever child is still
/// in the slot is killed and reaped.
pub struct Ending<'a>(pub &'a Slot);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// Stops the slot's child when the thread it is dropped on is unwinding: whoever watches the
/// child then sees it end, instead of waiting for a child nobody will read.
pub struct StopOnUnwind<'a>(pub &'a Slot);

impl Drop for StopOnUnwind<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.stop();
        }
    }
}

/// Tests: every child started through a [`Slot`] — the file it read, its process id, when it
/// started and when its end was taken.
#[cfg(test)]
pub(crate) mod log {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;
    use std::time::Instant;

    struct Run {
        file: PathBuf,
        pid: u32,
        from: Instant,
        to: Option<Instant>,
    }

    static RUNS: Mutex<Vec<Run>> = Mutex::new(Vec::new());

    pub(super) fn started(file: Option<&Path>, pid: u32) {
        if let Some(file) = file {
            RUNS.lock().unwrap_or_else(|p| p.into_inner()).push(Run {
                file: file.to_path_buf(),
                pid,
                from: Instant::now(),
                to: None,
            });
        }
    }

    pub(super) fn ended(pid: u32) {
        let mut runs = RUNS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(run) = runs
            .iter_mut()
            .rev()
            .find(|r| r.pid == pid && r.to.is_none())
        {
            run.to = Some(Instant::now());
        }
    }

    /// The process ids of every child started to read a file under `dir`, in order.
    pub(crate) fn started_under(dir: &Path) -> Vec<u32> {
        let runs = RUNS.lock().unwrap_or_else(|p| p.into_inner());
        runs.iter()
            .filter(|r| r.file.starts_with(dir))
            .map(|r| r.pid)
            .collect()
    }

    /// The most children reading files under `dir` that were alive at one moment — from each
    /// one's start to the moment its end was taken (now, for one not ended).
    pub(crate) fn most_at_once(dir: &Path) -> usize {
        let runs = RUNS.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let spans: Vec<(Instant, Instant)> = runs
            .iter()
            .filter(|r| r.file.starts_with(dir))
            .map(|r| (r.from, r.to.unwrap_or(now)))
            .collect();
        spans
            .iter()
            .map(|(from, _)| spans.iter().filter(|(a, b)| a <= from && from < b).count())
            .max()
            .unwrap_or(0)
    }

    fn ps(columns: &str, pids: Option<&[u32]>) -> String {
        let mut cmd = Command::new("ps");
        match pids {
            Some(pids) => {
                let list: Vec<String> = pids.iter().map(u32::to_string).collect();
                cmd.args(["-o", columns, "-p", &list.join(",")]);
            }
            None => {
                cmd.args(["-axo", columns]);
            }
        }
        String::from_utf8_lossy(&cmd.output().expect("ps").stdout).into_owned()
    }

    /// How many of `pids` are still processes — running, or ended and not reaped (`ps` lists
    /// both).
    pub(crate) fn still_processes(pids: &[u32]) -> usize {
        if pids.is_empty() {
            return 0;
        }
        let listed = ps("pid=", Some(pids));
        listed.lines().filter(|l| !l.trim().is_empty()).count()
    }

    /// How many of `pids` still run (one that ended and waits to be reaped does not).
    pub(crate) fn still_running(pids: &[u32]) -> usize {
        if pids.is_empty() {
            return 0;
        }
        let listed = ps("stat=", Some(pids));
        listed
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('Z'))
            .count()
    }

    /// How many processes on this machine name `dir` in their command line — a program in it, or
    /// a file in it as an argument: every stand-in and every ffmpeg a test started, whoever its
    /// parent is now.
    pub(crate) fn naming(dir: &Path) -> usize {
        pids_naming(dir).len()
    }

    /// The processes that name `dir` in their command line ([`naming`]).
    pub(crate) fn pids_naming(dir: &Path) -> Vec<u32> {
        let needle = dir.to_string_lossy().into_owned();
        let listed = ps("pid=,command=", None);
        listed
            .lines()
            .filter(|l| l.contains(&needle))
            .filter_map(|l| l.split_whitespace().next()?.parse().ok())
            .collect()
    }

    /// Nothing a test started under `dir` is left: none of the logged children is a process any
    /// more (reaped, each of them), and no process names `dir`. Waits up to 5 s for it — a
    /// stop only signals, and the reaping is its runner's next look.
    pub(crate) fn none_left(dir: &Path) -> Result<(), String> {
        let give_up = Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let pids = started_under(dir);
            let (logged, named) = (still_processes(&pids), naming(dir));
            if logged == 0 && named == 0 {
                return Ok(());
            }
            if Instant::now() >= give_up {
                return Err(format!(
                    "{logged} of {} started children still processes, {named} processes name {}",
                    pids.len(),
                    dir.display()
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests {
    //! The slot's own two defences against a lost stop, each by itself: nothing here but the
    //! slot runs or watches the child.

    use super::super::fixtures::{bounded, stand_in, started_under, still_running, until};
    use super::*;
    use std::io::Read as _;
    use std::sync::{mpsc, Arc};

    /// A stop that comes while a child is being started — after the start's look at the flag,
    /// its lock held — is applied by the start itself: the child it makes no longer runs when
    /// the start returns, with nobody watching it.
    #[test]
    fn a_start_applies_the_stop_that_came_while_it_started() {
        bounded(60, |t| {
            let tmp = t.tmp();
            let hold = t.hold(tmp.path(), "hold");
            let stalls = stand_in(tmp.path(), "stalls", &hold.stalls());
            let slot = Arc::new(Slot::default());
            let (reached_tx, reached) = mpsc::channel();
            let (go, go_rx) = mpsc::channel::<()>();
            *slot.while_starting.lock().unwrap() = Some(Box::new(move || {
                let _ = reached_tx.send(());
                let _ = go_rx.recv_timeout(Duration::from_secs(30));
            }));
            let file = tmp.path().join("some.mp4");
            let (starting, f) = (slot.clone(), file.clone());
            let starter = std::thread::spawn(move || {
                let mut cmd = Command::new(stalls);
                cmd.arg(&f);
                starting
                    .start(&mut cmd, Some(&f))
                    .map(|pipes| pipes.is_some())
            });
            reached
                .recv_timeout(Duration::from_secs(20))
                .expect("the start");
            // Gives up on the lock, and leaves its flag.
            slot.stop();
            go.send(()).unwrap();
            assert!(starter.join().unwrap().unwrap(), "the child was started");
            let pid = started_under(tmp.path());
            assert_eq!(pid.len(), 1);
            // Killed is not gone at once: it no longer runs within a moment, unlooked at.
            until("it no longer runs", || still_running(&pid) == 0);
            slot.end();
        });
    }

    /// A child that is read from is watched: a stop that only left its flag (it could not get
    /// the lock) is applied while the reader blocks on a child that writes nothing — the read
    /// ends, and the child is reaped.
    #[test]
    fn a_watched_read_is_stopped_whatever_it_blocks_on() {
        bounded(60, |t| {
            let tmp = t.tmp();
            let hold = t.hold(tmp.path(), "hold");
            let stalls = stand_in(tmp.path(), "stalls", &hold.stalls());
            let slot = Arc::new(Slot::default());
            let file = tmp.path().join("some.mp4");
            let mut cmd = Command::new(stalls);
            cmd.arg(&file);
            let (mut out, _err) = slot.start(&mut cmd, Some(&file)).unwrap().unwrap();
            let flagged = slot.clone();
            let reader = std::thread::spawn(move || {
                let _ending = Ending(&slot);
                slot.watched(|| {
                    let mut all = Vec::new();
                    let _ = out.read_to_end(&mut all);
                    all.len()
                })
                .map(|(read, status)| (read, status.is_some()))
            });
            let pid = started_under(tmp.path());
            until("it runs", || still_running(&pid) == 1);
            // The flag alone, as a stop that gave up on the lock leaves it.
            flagged.stopped.store(true, SeqCst);
            until("the read ends", || reader.is_finished());
            assert_eq!(reader.join().unwrap().unwrap(), (0, true), "reaped");
            assert_eq!(super::log::still_processes(&pid), 0);
        });
    }
}
