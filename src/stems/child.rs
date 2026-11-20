//! `sopen stems` as a child process, for the window (spec §7a): sherpa-onnx's `Process` cannot be
//! interrupted and the engine holds several GB, so the window never separates in its own
//! process. The child runs in a process group of its own (its ffmpeg decoder and encoders
//! with it) and encodes into a work folder the parent made; cancelling kills the group,
//! removes that folder and publishes nothing.
//!
//! The child does not publish on its own: once both stems are encoded it prints
//! [`READY_LINE`] and waits for [`GO_AHEAD`] on its stdin. The parent answers under the same
//! lock [`StemsChild::cancel`] takes, so a cancel either comes first (the child is killed
//! before it touches the remote, and an upload cannot be cut half-way, leaving a temp there)
//! or comes too late and the stems are published. The child watches its stdin from the start
//! ([`watch_parent`]): its end before the go-ahead means the parent is gone, and the child
//! removes its work folder and kills its own group — no orphan separating for minutes in
//! gigabytes of memory.

use super::{PieceProgress, Published, GO_AHEAD, READY_LINE};
use anyhow::{anyhow, bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Where a child is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Separating and encoding; a cancel kills it.
    Working,
    /// Told to publish: too late to cancel.
    Publishing,
    /// Killed by [`StemsChild::cancel`].
    Cancelled,
}

struct Shared {
    phase: Phase,
    progress: Option<PieceProgress>,
    /// Lines of stderr that are not progress: notes (`sopen: …`) and the error, if any.
    said: Vec<String>,
    stdin: Option<ChildStdin>,
}

/// What [`StemsChild::cancel`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancel {
    /// Killed: nothing published, its work folder removed.
    Cancelled,
    /// It was already publishing (or done): its result still comes from
    /// [`StemsChild::poll`] / [`StemsChild::wait`].
    TooLate,
}

/// A running `sopen stems` child.
///
/// Its exit and its output are two things: the leader is reaped as soon as it has exited (and
/// what it left running in its group killed first, while its id is still its own), but the
/// result is read only once both output pipes are closed — never by blocking on them. After
/// the leader is reaped nothing signals its id again: it may be anyone's by then.
pub struct StemsChild {
    child: Child,
    work: PathBuf,
    shared: Arc<Mutex<Shared>>,
    readers: Vec<JoinHandle<()>>,
    stdout: Arc<Mutex<Vec<String>>>,
    result: Option<Result<Published>>,
    /// The leader has been reaped, with this status (`Err`: waiting for it failed).
    reaped: Option<std::io::Result<ExitStatus>>,
    /// What removing the work folder gave, when it was removed.
    removed: Option<std::io::Result<()>>,
    /// The work folder's job lock, held until the folder is removed: a sopen killed meanwhile
    /// leaves both to the next start's sweep ([`crate::cache::sweep_stale_jobs`]).
    lock: Option<crate::cache::JobLock>,
}

#[cfg(test)]
thread_local! {
    /// Every group signal this thread sent: `(pid, whether its leader was already reaped)`.
    pub(crate) static SIGNALS: std::cell::RefCell<Vec<(i32, bool)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether the child `pid` has exited, without reaping it (`WNOWAIT`: a zombie keeps its id).
/// `block` waits for it.
fn has_exited(pid: u32, block: bool) -> std::io::Result<bool> {
    let flags = libc::WEXITED | libc::WNOWAIT | if block { 0 } else { libc::WNOHANG };
    loop {
        // SAFETY: zeroed is a valid siginfo_t; waitid fills it in or leaves it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a plain syscall on our own child.
        let r = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
        if r == 0 {
            // With WNOHANG and nothing to report, waitid returns 0 and leaves it zeroed.
            return Ok(info.si_signo != 0);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Starts `sopen stems --work-dir <new folder> --wait-to-publish [--model M] -- SOURCE` from
/// `sopen` — `Command::new(std::env::current_exe()?)` with whatever environment it needs — in a
/// process group of its own. The work folder is made here, under `work_root`
/// (`<cache>/.exports`), owner-only, its job lock held, and removed when the child ends however
/// it ends.
pub fn spawn_child(
    mut cmd: Command,
    source: &str,
    model: Option<&Path>,
    work_root: &Path,
) -> Result<StemsChild> {
    let work = work_root.join(format!("stems-{}", crate::remote::random_hex()));
    let lock = crate::cache::JobLock::take(&work)?;
    crate::cache::private_dir(&work)?;
    let program = PathBuf::from(cmd.get_program());
    cmd.arg("stems")
        .arg("--work-dir")
        .arg(&work)
        .arg("--wait-to-publish");
    if let Some(model) = model {
        cmd.arg("--model").arg(model);
    }
    cmd.arg("--").arg(source);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&work);
            return Err(e).with_context(|| format!("cannot run {}", program.display()));
        }
    };
    let shared = Arc::new(Mutex::new(Shared {
        phase: Phase::Working,
        progress: None,
        said: Vec::new(),
        stdin: child.stdin.take(),
    }));
    let stdout = Arc::new(Mutex::new(Vec::new()));
    let err = child.stderr.take().expect("piped");
    let out = child.stdout.take().expect("piped");
    let readers = vec![
        {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || read_stderr(err, &shared))
        },
        {
            let stdout = Arc::clone(&stdout);
            std::thread::spawn(move || {
                for line in BufReader::new(out).lines().map_while(|l| l.ok()) {
                    stdout.lock().unwrap().push(line);
                }
            })
        },
    ];
    Ok(StemsChild {
        child,
        work,
        shared,
        readers,
        stdout,
        result: None,
        reaped: None,
        removed: None,
        lock: Some(lock),
    })
}

/// The child's stderr: progress lines (`\n` or `\r` ended), the ready line (answered with the
/// go-ahead unless cancelled), and everything else kept for the result.
fn read_stderr(err: impl Read, shared: &Mutex<Shared>) {
    let mut reader = BufReader::new(err);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match read_line_cr(&mut reader, &mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim_matches(|c| c == '\r' || c == '\n');
        // A terminal-style progress line starts by clearing the line.
        let line = line.trim_start_matches("\x1b[2K").trim();
        if line.is_empty() {
            continue;
        }
        let mut s = shared.lock().unwrap();
        if let Some(p) = PieceProgress::parse(line) {
            s.progress = Some(p);
        } else if line == READY_LINE {
            if s.phase == Phase::Working {
                let told = s
                    .stdin
                    .as_mut()
                    .is_some_and(|i| writeln!(i, "{GO_AHEAD}").and_then(|()| i.flush()).is_ok());
                if told {
                    s.phase = Phase::Publishing;
                }
            }
            // Either way the child reads one line: closing stdin tells it no.
            s.stdin = None;
        } else {
            s.said.push(line.to_string());
        }
    }
}

/// One line ended by `\n` or `\r` (included), into `out`; 0 at the end.
fn read_line_cr(r: &mut impl BufRead, out: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut n = 0;
    loop {
        let (used, end) = {
            let available = r.fill_buf()?;
            if available.is_empty() {
                return Ok(n);
            }
            match available.iter().position(|&b| b == b'\n' || b == b'\r') {
                Some(i) => {
                    out.extend_from_slice(&available[..=i]);
                    (i + 1, true)
                }
                None => {
                    out.extend_from_slice(available);
                    (available.len(), false)
                }
            }
        };
        r.consume(used);
        n += used;
        if end {
            return Ok(n);
        }
    }
}

impl StemsChild {
    /// The last progress line the child printed.
    pub fn progress(&self) -> Option<PieceProgress> {
        self.shared.lock().unwrap().progress
    }

    /// The work folder the child encodes into (removed when it ends).
    pub fn work_dir(&self) -> &Path {
        &self.work
    }

    /// The child's process id, which is also its process group's.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// The result once the child has ended and its output is read; `None` until then. Never
    /// blocks: not on the child, and not on a pipe something it started still holds open.
    pub fn poll(&mut self) -> Option<&Result<Published>> {
        if self.result.is_none() {
            self.step(false);
        }
        self.result.as_ref()
    }

    /// Waits for the child's exit and its output, and returns its result.
    pub fn wait(mut self) -> Result<Published> {
        while self.result.is_none() {
            self.step(true);
            if self.result.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.result.take().expect("collected")
    }

    /// Kills the child (and its ffmpeg children) unless it is already publishing or has
    /// already ended; then nothing is published and its work folder is removed. Does not wait
    /// for the output: the result is the cancel's.
    pub fn cancel(&mut self) -> Cancel {
        {
            let mut s = self.shared.lock().unwrap();
            if s.phase == Phase::Publishing || self.reaped.is_some() || self.result.is_some() {
                return Cancel::TooLate;
            }
            s.phase = Phase::Cancelled;
            s.stdin = None;
            // The whole group: the child is its leader (`process_group(0)`), its ffmpeg
            // children are members. Not reaped yet, so the id is still the child's.
            self.signal_group();
        }
        self.reap();
        // Nothing to read: the readers end when the pipes close, on their own.
        self.readers.clear();
        self.result = Some(self.with_leftover(Err(anyhow!("cancelled; nothing was published"))));
        Cancel::Cancelled
    }

    /// SIGKILL to the process group the child leads — only while the leader is not reaped
    /// (alive, or a zombie: its id is still its own). Once it is reaped this sends nothing, its
    /// id may be anyone's by then; the tests' log records the call with the handle's real state.
    fn signal_group(&self) {
        let reaped = self.reaped.is_some();
        #[cfg(test)]
        SIGNALS.with(|s| s.borrow_mut().push((self.child.id() as i32, reaped)));
        if reaped {
            return;
        }
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // SAFETY: a plain syscall on our own, unreaped child's process group.
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }

    /// The leader's end, without touching its output: once it has exited (seen without reaping
    /// it — `block` waits for that), what it left running in its group is killed while its id is
    /// still its own, then it is reaped and the work folder removed. `false` while it runs.
    fn settle(&mut self, block: bool) -> bool {
        if self.reaped.is_none() {
            match has_exited(self.child.id(), block) {
                Ok(false) => return false,
                // A descendant holding the output open, say: it goes now.
                Ok(true) => self.signal_group(),
                // It cannot be looked at without reaping: reap it, and signal nothing.
                Err(_) => {}
            }
        }
        self.reap();
        true
    }

    /// Reaps the leader (it has exited, or was just killed) and removes the work folder.
    fn reap(&mut self) {
        if self.reaped.is_none() {
            self.reaped = Some(self.child.wait());
        }
        if self.removed.is_none() {
            self.removed = Some(std::fs::remove_dir_all(&self.work));
            self.lock = None;
        }
    }

    /// One look: has the leader exited (then: its group's survivors killed, it reaped, the work
    /// folder removed), and is its output all read (then: the result). `block` waits for the
    /// exit, never for the output.
    fn step(&mut self, block: bool) {
        if !self.settle(block) {
            return;
        }
        if self.readers.iter().any(|r| !r.is_finished()) {
            return;
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        self.collect();
    }

    /// The work folder's removal, folded into `result`.
    fn with_leftover(&self, result: Result<Published>) -> Result<Published> {
        let leftover = match &self.removed {
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Some(format!(
                "the work folder {} could not be removed: {e}",
                self.work.display()
            )),
            _ => None,
        };
        match (result, leftover) {
            (r, None) => r,
            (Ok(mut published), Some(note)) => {
                published.notes.push(note);
                Ok(published)
            }
            (Err(e), Some(note)) => Err(anyhow!("{e:#}; {note}")),
        }
    }

    /// The result, from the reaped leader's status and its (completely read) output.
    fn collect(&mut self) {
        if self.result.is_some() {
            return;
        }
        let status = match self.reaped.as_ref().expect("reaped before collected") {
            Ok(status) => Ok(*status),
            Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
        };
        let shared = self.shared.lock().unwrap();
        let said = &shared.said;
        let error_line = || {
            said.iter()
                .rev()
                .find_map(|l| l.strip_prefix("sopen: "))
                .map(str::to_string)
        };
        let result = match status {
            _ if shared.phase == Phase::Cancelled => {
                Err(anyhow!("cancelled; nothing was published"))
            }
            Err(e) => Err(anyhow!("lost the stems child: {e}")),
            Ok(status) if status.success() => {
                let lines = self.stdout.lock().unwrap();
                match lines.as_slice() {
                    [voice, music] => {
                        // Every `sopen: ` line of a run that succeeded is a note.
                        let notes = said
                            .iter()
                            .filter_map(|l| l.strip_prefix("sopen: "))
                            .map(str::to_string)
                            .collect();
                        Ok(Published {
                            voice: voice.clone(),
                            music: music.clone(),
                            notes,
                        })
                    }
                    other => Err(anyhow!(
                        "the stems child printed {} lines, not the two published paths",
                        other.len()
                    )),
                }
            }
            Ok(status) => Err(anyhow!(
                "{}",
                error_line().unwrap_or_else(|| format!("sopen stems ended with {status}"))
            )),
        };
        drop(shared);
        self.result = Some(self.with_leftover(result));
    }
}

impl Drop for StemsChild {
    /// A child nobody waits for is cancelled: no orphan holding gigabytes. One that was already
    /// reaped (a consumed `wait`, a cancel) is never signalled again. One that is publishing is
    /// never cut short: drop waits for the leader's exit, then ends it as `poll` would — what it
    /// left in its group killed, it reaped, the work folder removed — without reading its output
    /// (the readers end when the pipes close).
    fn drop(&mut self) {
        if self.reaped.is_none() && self.cancel() == Cancel::TooLate {
            self.settle(true);
        }
        self.reap();
    }
}

/// The supervised child's side of the hand-shake: the one line its stdin brought must be
/// [`GO_AHEAD`]; anything else and nothing is published.
pub fn go_ahead(line: &str) -> Result<()> {
    if line.trim() != GO_AHEAD {
        bail!("cancelled before publishing; nothing was published");
    }
    Ok(())
}

/// The supervised child (`sopen stems --wait-to-publish`) reads its stdin from the start, on a
/// thread: the first line goes to the receiver, for [`go_ahead`]. The end of stdin before any
/// line — the parent is gone, or cancelled — ends the child at once: `work` (the parent's
/// work folder) is removed and, when the child leads its own process group (as
/// [`spawn_child`] starts it), the group is killed, its ffmpeg children with it.
pub fn watch_parent(
    mut input: impl BufRead + Send + 'static,
    work: Option<PathBuf>,
) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(n) if n > 0 => {
                let _ = tx.send(line);
            }
            _ => {
                if let Some(work) = &work {
                    let _ = std::fs::remove_dir_all(work);
                }
                // SAFETY: plain syscalls; `_exit` skips destructors on purpose (the engine may
                // be mid-call on the main thread).
                unsafe {
                    if libc::getpgrp() == libc::getpid() {
                        libc::kill(0, libc::SIGKILL);
                    }
                    libc::_exit(1);
                }
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests;
