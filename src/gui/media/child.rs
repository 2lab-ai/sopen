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
//! - **Nothing starts once stopped**: a child is started under the slot's lock, after a look at
//!   the flag, so a stop either comes first (and nothing starts) or finds the child.

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
        let mut child = crate::ffmpeg::spawn(cmd)?;
        #[cfg(test)]
        log::started(file, child.id());
        let _ = file;
        let pipes = (
            child.stdout.take().expect("piped"),
            child.stderr.take().expect("piped"),
        );
        *live = Some(child);
        Ok(Some(pipes))
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
                #[cfg(test)]
                log::ended(child.id());
                live.take();
                Polled::Ended(status)
            }
            // It cannot be waited for: killed, and let go.
            Err(_) => {
                let _ = child.kill();
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

/// A run's hold on its slot: dropped — the run returned, or unwound — whatever child is still
/// in the slot is killed and reaped.
pub struct Ending<'a>(pub &'a Slot);

impl Drop for Ending<'_> {
    fn drop(&mut self) {
        self.0.end();
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
        let needle = dir.to_string_lossy().into_owned();
        let listed = ps("pid=,command=", None);
        listed.lines().filter(|l| l.contains(&needle)).count()
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
