//! The macOS F-key switch (`.prd/04-file-keys.md` §6.2–§6.4): while a sopen window is in front,
//! the Mac's top row sends F1…F12 without fn. The HID system parameter `HIDFKeyMode` is global —
//! 0 (the Mac's default): the top row sends media keys without fn; 1: F1…F12 — so sopen sets it
//! to 1 while one of its windows has focus and puts back what it found once none has, through a
//! lock, a state file and a watchdog that restore it even when sopen dies (§6.3). Linux and
//! Windows have no such switch for an unprivileged app (§6.4): there, as in a scripted run or a
//! test build, there is no switch at all.
//!
//! The caller's contract (the shell's focus events, S7-B):
//! - [`for_this_run`] gives this process's switch, or `None` where sopen must not touch the
//!   setting: `fkeys = "system"`, a scripted run ([`scripted_run`]), a test build, any OS but
//!   macOS. Creating it runs the start's clean-up: a state its dead holders left is restored.
//!   One per process — the holder is the process.
//! - Feed it [`Fkeys::focus`] (whether any of this process's windows has focus), [`Fkeys::sleep`]
//!   / [`Fkeys::wake`] and [`Fkeys::session`]; each may be called on every frame and acts only on
//!   a change. Dropping it (or [`Fkeys::quit`]) releases.
//! - [`Fkeys::status`] is the status row's line when an acquisition did not take effect
//!   ([`NOT_IN_EFFECT`]); the clickable row and Cmd/Ctrl+digits (§6.1) still work then.
//! - A release that failed is retried at a later call, after [`Fkeys::next_retry`] (a repaint
//!   then lets it run); at the end, the watchdog retries it.
//! - `sopen --fkeys-watchdog <pid>:<start> --fkeys-dir <dir>` (hidden) runs [`watchdog_main`].
//!
//! The responsibility for the restore never lapses: a restore counts only once the original
//! reads back; until then the state stays, and the process, its watchdog (bounded retries) or
//! the next start's clean-up retries it.
//!
//! The protocol (§6.3), every step holding an exclusive lock on [`LOCK`] in the state folder,
//! the read and the change of the HID value included: [`acquire`], [`release`] and the start's
//! [`clean_up`]. [`STATE`] holds the value to restore and the holders, each a pid with its start
//! time, so a reused pid is not taken for a live holder.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::config;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(test)]
mod tests;

/// The lock file in the state folder: created once, never replaced or removed.
pub const LOCK: &str = "fkeys.lock";
/// The state file in the state folder: the value to restore and the holders (0600, replaced
/// atomically); it exists exactly while sopen's own 1 is set, or a restore of it is pending.
pub const STATE: &str = "fkeys.json";
/// `HIDFKeyMode` 0: the top row sends media keys without fn (the Mac's default, measured).
pub const OFF: i64 = 0;
/// `HIDFKeyMode` 1: the top row sends F1…F12 without fn.
pub const ON: i64 = 1;
/// The status row's words when an acquisition did not take effect (04 §6.3).
pub const NOT_IN_EFFECT: &str = "the F-key switch did not take effect";

/// The hidden flags of the watchdog child (`main.rs`).
pub const WATCHDOG_FLAG: &str = "--fkeys-watchdog";
pub const DIR_FLAG: &str = "--fkeys-dir";

/// The line a watchdog prints on its output once it is ready: it can read the switch and waits
/// on its input. The holder acquires only after it.
pub const READY: &str = "sopen-fkeys-watchdog-ready";
/// How long a new watchdog has to say it is ready.
const READY_WAIT: Duration = Duration::from_secs(2);

/// How a restore is waited for and retried.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pace {
    /// How often the watchdog also checks that its holder still runs, besides waiting for the
    /// pipe's end: a child the holder started at the moment the pipe was made could keep a copy
    /// of its write end, and the pipe would never end.
    pub poll: Duration,
    /// A release that failed is retried after `first`, the wait doubling up to `most`, for at
    /// most `total`; then the state is left for the next start.
    pub first: Duration,
    pub most: Duration,
    pub total: Duration,
}

impl Pace {
    pub const DEFAULT: Pace = Pace {
        poll: Duration::from_secs(1),
        first: Duration::from_millis(50),
        most: Duration::from_secs(2),
        total: Duration::from_secs(60),
    };
}

/// Runs that hooks drive (04 §6.2: never switched): any of these set to a non-empty value.
/// `SOPEN_KEYS` too — its keys are not the owner's.
const SCRIPTED: [&str; 5] = [
    "SOPEN_SCREENSHOT",
    "SOPEN_AUTOSAVE",
    "SOPEN_AUTOEXPORT",
    "SOPEN_LIST",
    "SOPEN_KEYS",
];

/// The HID system's `HIDFKeyMode` ([`OFF`], [`ON`]), behind a trait so that every test runs
/// against a fake (04 §6.2); the real one is [`macos::IoHid`].
pub trait Switch {
    /// The value as the system reports it now.
    fn read(&mut self) -> Result<i64>;
    /// Sets the value.
    fn write(&mut self, value: i64) -> Result<()>;
}

impl<S: Switch + ?Sized> Switch for Box<S> {
    fn read(&mut self) -> Result<i64> {
        (**self).read()
    }

    fn write(&mut self, value: i64) -> Result<()> {
        (**self).write(value)
    }
}

/// A process holding the switch: its pid and when it started ([`process::start`]'s unit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Holder {
    pub pid: u32,
    pub start: u64,
}

impl Holder {
    /// This process.
    pub fn this_process() -> Result<Holder> {
        let pid = std::process::id();
        let start = process::start(pid).context("cannot read this process's start time")?;
        Ok(Holder { pid, start })
    }

    /// `<pid>:<start>`, as [`Holder`]'s `Display` writes it (the watchdog's argument).
    pub fn parse(text: &str) -> Option<Holder> {
        let (pid, start) = text.split_once(':')?;
        Some(Holder {
            pid: pid.parse().ok()?,
            start: start.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.pid, self.start)
    }
}

/// What the protocol asks of the machine besides the switch: whether a holder still runs, and
/// which boot this is.
#[derive(Clone)]
pub(crate) struct Machine {
    pub runs: Arc<dyn Fn(&Holder) -> bool + Send + Sync>,
    pub boot: Option<String>,
}

impl Machine {
    /// This machine: [`process::runs`] and [`process::boot`].
    pub fn this() -> Machine {
        Machine {
            runs: Arc::new(process::runs),
            boot: process::boot(),
        }
    }
}

/// [`STATE`]'s content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    /// The value to restore when the last holder lets go.
    pub original: i64,
    /// The processes holding the switch.
    pub holders: Vec<Holder>,
    /// The boot the state was written in. A state left from another boot is dropped without
    /// touching the value: a reboot is taken to reset it, with login applying the owner's
    /// preference — an inference (keys.md §4.1 item 3; E1 found the set writes no preference
    /// file), not measured. Were a stuck 1 to survive a reboot, it would then read as the
    /// owner's own and stay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot: Option<String>,
}

impl State {
    /// What a state file sopen cannot trust is read as.
    fn untrusted() -> State {
        State {
            original: OFF,
            holders: Vec::new(),
            boot: None,
        }
    }

    fn left_by_another_boot(&self, m: &Machine) -> bool {
        matches!((&self.boot, &m.boot), (Some(then), Some(now)) if then != now)
    }
}

// ---------------------------------------------------------------------------------------------
// The protocol (04 §6.3).

/// What an acquisition came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acquired {
    /// This process set 1 and read it back: it is the first holder, or it took over the state a
    /// dead last holder left (whose watchdog has not run yet).
    Set,
    /// Other sopen processes hold the switch, so the 1 is sopen's own: this process was added.
    Joined,
    /// No state and the value is not 0: the owner's own choice. Nothing recorded or touched.
    OwnersOwn,
    /// The write failed or did not read back 1: this process is not a holder, and with no holder
    /// left the recorded original was set back. The reason.
    NotInEffect(String),
}

impl Acquired {
    /// This process is a holder now, and must release.
    pub fn holds(&self) -> bool {
        matches!(self, Acquired::Set | Acquired::Joined)
    }
}

/// Acquire for `me`: drop holders that no longer run; then join the holders that remain, take
/// over a state without holders (its recorded original kept), or — with no state — leave an
/// owner's 1 alone or record 0 and set 1. A set that fails or does not read back 1 is undone.
pub(crate) fn acquire(
    dir: &Path,
    switch: &mut dyn Switch,
    me: Holder,
    m: &Machine,
) -> Result<Acquired> {
    let _lock = lock(dir)?;
    let state = match read_state(dir)? {
        Some(s) if s.left_by_another_boot(m) => {
            remove_state(dir)?;
            None
        }
        found => found,
    };
    let state = match state {
        Some(mut s) => {
            s.holders.retain(|h| *h != me && (m.runs)(h));
            s.holders.push(me);
            if s.holders.len() > 1 {
                write_state(dir, &s)?;
                return Ok(Acquired::Joined);
            }
            // Its last holder died and its watchdog has not run yet: the state is sopen's, not
            // the owner's. The value is 0 (it died before setting it) or 1 (after); either way
            // the takeover ends with 1.
            s.boot = m.boot.clone();
            s
        }
        None => {
            let now = match switch.read() {
                Ok(now) => now,
                Err(e) => return Ok(Acquired::NotInEffect(format!("{e:#}"))),
            };
            if now != OFF {
                return Ok(Acquired::OwnersOwn);
            }
            State {
                original: now,
                holders: vec![me],
                boot: m.boot.clone(),
            }
        }
    };
    // Written before the value changes: a holder that dies between the two leaves a state its
    // watchdog (or the next start, or a takeover) acts on.
    write_state(dir, &state)?;
    let failed = match switch.write(ON) {
        Err(e) => Some(format!("{e:#}")),
        Ok(()) => match switch.read() {
            Ok(ON) => None,
            Ok(v) => Some(format!("HIDFKeyMode reads {v} after it was set to {ON}")),
            Err(e) => Some(format!("{e:#}")),
        },
    };
    let Some(why) = failed else {
        return Ok(Acquired::Set);
    };
    let mut state = state;
    state.holders.retain(|h| *h != me);
    let_go(dir, switch, state)?;
    Ok(Acquired::NotInEffect(why))
}

/// Release for `who` (this process, or the watchdog's dead holder): it and every holder that no
/// longer runs leave the holders; with none left, the recorded original is set and the state
/// removed (the lock file stays).
pub(crate) fn release(dir: &Path, switch: &mut dyn Switch, who: Holder, m: &Machine) -> Result<()> {
    let _lock = lock(dir)?;
    let Some(mut s) = read_state(dir)? else {
        return Ok(());
    };
    if s.left_by_another_boot(m) {
        return remove_state(dir);
    }
    let before = s.holders.len();
    s.holders.retain(|h| *h != who && (m.runs)(h));
    if !s.holders.is_empty() && s.holders.len() == before {
        return Ok(());
    }
    let_go(dir, switch, s)
}

/// The start's clean-up: holders that no longer run are dropped; with none left, the value is
/// restored and the state removed. Nothing is created when there is no state.
pub(crate) fn clean_up(dir: &Path, switch: &mut dyn Switch, m: &Machine) -> Result<()> {
    // Only a shortcut: the state is read again under the lock.
    if !dir.join(STATE).exists() {
        return Ok(());
    }
    let _lock = lock(dir)?;
    let Some(mut s) = read_state(dir)? else {
        return Ok(());
    };
    if s.left_by_another_boot(m) {
        return remove_state(dir);
    }
    let before = s.holders.len();
    s.holders.retain(|h| (m.runs)(h));
    if !s.holders.is_empty() && s.holders.len() == before {
        return Ok(());
    }
    let_go(dir, switch, s)
}

/// Under the lock: writes `s`, or with no holder left sets its original back and — once it reads
/// back — removes the state. A restore that fails or does not read back keeps the state, without
/// holders, and is an error: the caller retries, and so does the next start.
fn let_go(dir: &Path, switch: &mut dyn Switch, s: State) -> Result<()> {
    if !s.holders.is_empty() {
        return write_state(dir, &s);
    }
    let restored = switch
        .write(s.original)
        .and_then(|()| match switch.read()? {
            v if v == s.original => Ok(()),
            v => bail!("HIDFKeyMode reads {v} after it was set back"),
        });
    if let Err(e) = restored {
        write_state(dir, &s)?;
        return Err(e.context(format!("cannot set HIDFKeyMode back to {}", s.original)));
    }
    remove_state(dir)
}

/// The exclusive lock on [`LOCK`], held until dropped. The file is opened as it is, created when
/// missing, never truncated, replaced or removed; a lock taken on a file no longer at its name
/// (someone else replaced it) is let go and taken again.
struct Locked {
    _file: File,
}

fn lock(dir: &Path) -> Result<Locked> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(LOCK);
    for _ in 0..3 {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        file.lock()
            .with_context(|| format!("cannot lock {}", path.display()))?;
        let held = file.metadata()?;
        match std::fs::symlink_metadata(&path) {
            Ok(now) if now.dev() == held.dev() && now.ino() == held.ino() => {
                return Ok(Locked { _file: file })
            }
            _ => continue,
        }
    }
    bail!("{} keeps being replaced", path.display())
}

/// [`STATE`], or `None` when there is none. Text sopen cannot read, or a state whose original is
/// not 0 — sopen records a state only after reading 0, so it never wrote that one — counts as a
/// state with original 0 and no holders: the restore is 0 whatever the file says, and the holders
/// it named are lost either way (each live one adds itself back when it acquires).
pub(crate) fn read_state(dir: &Path) -> Result<Option<State>> {
    let path = dir.join(STATE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let state = match serde_json::from_slice::<State>(&bytes) {
        Ok(s) if s.original == OFF => s,
        Ok(s) => {
            crate::debug!(
                "{}: original {}, not sopen's; read as 0",
                path.display(),
                s.original
            );
            State::untrusted()
        }
        Err(e) => {
            crate::debug!("{}: {e}; read as original 0, no holders", path.display());
            State::untrusted()
        }
    };
    Ok(Some(state))
}

/// Replaces [`STATE`] atomically: a 0600 temp in the same folder, renamed over it. Not flushed to
/// the disk: the rename is atomic for any process that dies, and a power loss or a crash of the
/// whole system reboots it, which resets the value and makes the state another boot's.
pub(crate) fn write_state(dir: &Path, s: &State) -> Result<()> {
    let text = serde_json::to_vec(s).context("cannot encode the F-key state")?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let temp = dir.join(format!(".{STATE}.{}-{nanos}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .with_context(|| format!("cannot create {}", temp.display()))?;
    let written = file
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(&text))
        .and_then(|()| std::fs::rename(&temp, dir.join(STATE)));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("cannot write {}", dir.join(STATE).display()));
    }
    Ok(())
}

fn remove_state(dir: &Path) -> Result<()> {
    let path = dir.join(STATE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("cannot remove {}", path.display())),
    }
}

// ---------------------------------------------------------------------------------------------
// One process's switch.

/// A run hooks drive: `SOPEN_SCREENSHOT`, `SOPEN_AUTOSAVE`, `SOPEN_AUTOEXPORT`, `SOPEN_LIST` or
/// `SOPEN_KEYS` set to anything but nothing (`var` reads the environment).
pub fn scripted_run(var: impl Fn(&str) -> Option<OsString>) -> bool {
    SCRIPTED
        .iter()
        .any(|name| var(name).is_some_and(|v| !v.is_empty()))
}

/// This run's switch, or `None` where sopen must not touch the setting: `fkeys = "system"`, a
/// scripted run, a test build (`cargo test` never switches it, 04 §6.2), any OS but macOS
/// (§6.4). `state_dir` is sopen's state folder. Nothing of IOKit is loaded before the switch is
/// first needed.
pub fn for_this_run(config: &config::Config, state_dir: &Path) -> Option<ThisRun> {
    if cfg!(test) || scripted_run(|name| std::env::var_os(name)) {
        return None;
    }
    from_config(config, state_dir, platform_switch, || Parts {
        me: Holder::this_process().map_err(|e| format!("{e:#}")),
        machine: Machine::this(),
        watchdog: Watchdog::start(Box::new(watchdog_command)),
    })
}

/// The switch of a run of this binary.
pub type ThisRun = Fkeys<Box<dyn Switch + Send>>;

/// What a process's switch is made of besides the switch itself.
pub(crate) struct Parts {
    pub me: std::result::Result<Holder, String>,
    pub machine: Machine,
    pub watchdog: Watchdog,
}

/// The one construction of a process's switch, [`for_this_run`]'s and the tests': none with
/// `fkeys = "system"`, and then neither `open` nor `parts` is called (K-F4: sopen never opens
/// IOHIDSystem); else the switch `open` makes, after the start's clean-up.
pub(crate) fn from_config<S: Switch>(
    config: &config::Config,
    dir: &Path,
    open: impl FnOnce() -> Option<S>,
    parts: impl FnOnce() -> Parts,
) -> Option<Fkeys<S>> {
    if config.fkeys() == config::Fkeys::System {
        return None;
    }
    let switch = open()?;
    let Parts {
        me,
        machine,
        watchdog,
    } = parts();
    Some(Fkeys::new(dir.to_path_buf(), switch, me, machine, watchdog))
}

#[cfg(target_os = "macos")]
fn platform_switch() -> Option<Box<dyn Switch + Send>> {
    Some(Box::new(macos::IoHid::new()))
}

#[cfg(not(target_os = "macos"))]
fn platform_switch() -> Option<Box<dyn Switch + Send>> {
    None
}

/// The production watchdog: this binary's hidden subcommand.
fn watchdog_command(dir: &Path, holder: &Holder) -> io::Result<Command> {
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg(WATCHDOG_FLAG)
        .arg(holder.to_string())
        .arg(DIR_FLAG)
        .arg(dir);
    Ok(cmd)
}

/// The command a watchdog is started with, given the state folder and the holder.
pub(crate) type WatchdogCommand = Box<dyn Fn(&Path, &Holder) -> io::Result<Command> + Send>;

/// This process's watchdog: how to start one, and the one running, if any.
pub(crate) struct Watchdog {
    /// `None`: no watchdog (tests of the protocol alone).
    command: Option<WatchdogCommand>,
    running: Option<Running>,
    /// How long a new one has to say it is ready.
    ready_wait: Duration,
}

/// A watchdog that said it was ready.
struct Running {
    /// The pipe's write end, which only this process holds: its end is the watchdog's cue.
    _input: ChildStdin,
    /// Polled (`try_wait`) before each acquisition; never waited for otherwise: the watchdog
    /// ends after this process.
    child: Child,
}

impl Watchdog {
    /// Started with `command` at the first acquisition.
    pub fn start(command: WatchdogCommand) -> Self {
        Watchdog {
            command: Some(command),
            running: None,
            ready_wait: READY_WAIT,
        }
    }

    /// No watchdog.
    #[cfg(test)]
    pub fn off() -> Self {
        Watchdog {
            command: None,
            running: None,
            ready_wait: READY_WAIT,
        }
    }

    /// The same, given `wait` to say it is ready.
    #[cfg(test)]
    pub fn ready_within(mut self, wait: Duration) -> Self {
        self.ready_wait = wait;
        self
    }

    /// A watchdog that runs and has said it is ready: the one there if it still runs, else a new
    /// one, started once. An error when there is none: no acquisition then.
    fn ensure(&mut self, dir: &Path, me: &Holder) -> io::Result<()> {
        let Some(command) = &self.command else {
            return Ok(());
        };
        if let Some(running) = &mut self.running {
            if let Ok(None) = running.child.try_wait() {
                return Ok(());
            }
            crate::debug!("F-key switch: its watchdog has ended; starting another");
            self.running = None;
        }
        let mut cmd = command(dir, me)?;
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        if crate::debug_enabled() {
            cmd.stderr(Stdio::inherit());
        } else {
            cmd.stderr(Stdio::null());
        }
        // SAFETY: setsid and signal are async-signal-safe, as `pre_exec` requires. A session of
        // its own: a terminal's Ctrl-C or hangup does not reach it. SIGTERM, SIGINT and SIGHUP
        // ignored: a `killall sopen` that ends this process leaves the watchdog to restore. It
        // ends by itself once this process has.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                    libc::signal(signal, libc::SIG_IGN);
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("the watchdog has no pipes"));
        };
        // Its output is read to its end, the ready line looked for on the way.
        let (said, ready) = mpsc::channel();
        std::thread::spawn(move || {
            let mut said = Some(said);
            for line in io::BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                if line == READY {
                    if let Some(said) = said.take() {
                        let _ = said.send(());
                    }
                }
            }
        });
        match ready.recv_timeout(self.ready_wait) {
            Ok(()) => {
                self.running = Some(Running {
                    _input: input,
                    child,
                });
                Ok(())
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(io::Error::other(match e {
                    mpsc::RecvTimeoutError::Timeout => format!(
                        "its watchdog did not say it was ready within {:?}",
                        self.ready_wait
                    ),
                    mpsc::RecvTimeoutError::Disconnected => {
                        "its watchdog ended before it was ready".to_string()
                    }
                }))
            }
        }
    }

    #[cfg(test)]
    fn pid(&self) -> Option<u32> {
        self.running.as_ref().map(|r| r.child.id())
    }
}

/// A release that failed, to be tried again.
#[derive(Debug, Clone, Copy)]
struct Pending {
    at: Instant,
    wait: Duration,
    until: Instant,
}

/// One process's switch: what it wants (a focused window, awake, its session in front), whether
/// it holds, its watchdog. Acquires and releases through the protocol on each change.
pub struct Fkeys<S: Switch> {
    dir: PathBuf,
    switch: S,
    me: std::result::Result<Holder, String>,
    machine: Machine,
    watchdog: Watchdog,
    focused: bool,
    asleep: bool,
    away: bool,
    holds: bool,
    /// An acquisition was tried since the switch was last wanted: not tried again until it is
    /// let go of and wanted anew.
    tried: bool,
    status: Option<String>,
    /// A release (or an acquisition's undo) that failed: retried at a later call while the
    /// switch is not wanted, and at the end; the watchdog retries after that.
    pending: Option<Pending>,
    pace: Pace,
}

impl<S: Switch> Fkeys<S> {
    /// The switch of the process `me`, after the start's clean-up (§6.3).
    pub(crate) fn new(
        dir: PathBuf,
        mut switch: S,
        me: std::result::Result<Holder, String>,
        machine: Machine,
        watchdog: Watchdog,
    ) -> Self {
        if let Err(e) = clean_up(&dir, &mut switch, &machine) {
            crate::debug!("F-key switch: the start's clean-up failed: {e:#}");
        }
        Fkeys {
            dir,
            switch,
            me,
            machine,
            watchdog,
            focused: false,
            asleep: false,
            away: false,
            holds: false,
            tried: false,
            status: None,
            pending: None,
            pace: Pace::DEFAULT,
        }
    }

    /// The same, retrying at `pace`.
    #[cfg(test)]
    pub(crate) fn with_pace(mut self, pace: Pace) -> Self {
        self.pace = pace;
        self
    }

    /// Whether any of this process's windows has focus.
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
        self.update();
    }

    /// The Mac is about to sleep: released.
    pub fn sleep(&mut self) {
        self.asleep = true;
        self.update();
    }

    /// The Mac woke: acquired again if a window still has focus.
    pub fn wake(&mut self) {
        self.asleep = false;
        self.update();
    }

    /// This login session became active (`true`) or was switched away from (`false`).
    pub fn session(&mut self, active: bool) {
        self.away = !active;
        self.update();
    }

    /// sopen quits: released (as dropping it does).
    pub fn quit(self) {}

    /// This process holds the switch now.
    pub fn holds(&self) -> bool {
        self.holds
    }

    /// The status row's line after an acquisition that did not take effect, until one does.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// When a release that failed is tried again — at the first call from then on (a repaint
    /// requested for then makes one).
    pub fn next_retry(&self) -> Option<Instant> {
        self.pending.map(|p| p.at)
    }

    fn update(&mut self) {
        let wanted = self.focused && !self.asleep && !self.away;
        if !wanted {
            self.tried = false;
            let due = self.pending.is_some_and(|p| Instant::now() >= p.at);
            if self.holds || due {
                self.let_go();
            }
        } else if !self.holds && !self.tried {
            self.tried = true;
            self.take();
        }
    }

    fn take(&mut self) {
        let me = match &self.me {
            Ok(me) => *me,
            Err(why) => {
                self.status = Some(format!("{NOT_IN_EFFECT}: {why}"));
                return;
            }
        };
        // A watchdog that runs and said it is ready, before every acquisition: a death of this
        // process right after it is covered.
        if let Err(e) = self.watchdog.ensure(&self.dir, &me) {
            self.status = Some(format!("{NOT_IN_EFFECT}: {e}"));
            return;
        }
        match acquire(&self.dir, &mut self.switch, me, &self.machine) {
            Ok(got) if got.holds() => {
                self.holds = true;
                self.status = None;
                self.pending = None;
            }
            Ok(Acquired::NotInEffect(why)) => self.status = Some(format!("{NOT_IN_EFFECT}: {why}")),
            Ok(_) => self.status = None,
            Err(e) => {
                // An undo that did not hold may have left the value set: released again later.
                self.status = Some(format!("{NOT_IN_EFFECT}: {e:#}"));
                self.retry_later();
            }
        }
    }

    fn let_go(&mut self) {
        self.holds = false;
        let Ok(me) = self.me else {
            self.pending = None;
            return;
        };
        match release(&self.dir, &mut self.switch, me, &self.machine) {
            Ok(()) => self.pending = None,
            Err(e) => {
                crate::debug!("F-key switch: release failed: {e:#}");
                self.retry_later();
            }
        }
    }

    /// Schedules the next try of a release that failed: `first`, doubling up to `most`, for at
    /// most `total`; past that only the end (and the watchdog, and the next start) retries.
    fn retry_later(&mut self) {
        let now = Instant::now();
        let pace = self.pace;
        self.pending = match self.pending {
            None => Some(Pending {
                at: now + pace.first,
                wait: pace.first,
                until: now + pace.total,
            }),
            Some(p) if now >= p.until => None,
            Some(p) => {
                let wait = (p.wait * 2).min(pace.most);
                Some(Pending {
                    at: now + wait,
                    wait,
                    until: p.until,
                })
            }
        };
    }

    /// The watchdog's pid, once started.
    #[cfg(test)]
    pub(crate) fn watchdog_pid(&self) -> Option<u32> {
        self.watchdog.pid()
    }
}

impl<S: Switch> Drop for Fkeys<S> {
    fn drop(&mut self) {
        if self.holds || self.pending.is_some() {
            self.let_go();
        }
        // Then the watchdog's input is dropped with the rest: it releases (nothing left to do,
        // or the retries of a release that failed here) and ends.
    }
}

// ---------------------------------------------------------------------------------------------
// The watchdog.

/// `sopen --fkeys-watchdog <pid>:<start> --fkeys-dir <dir>` (hidden): says [`READY`] once it can
/// read the switch and waits on its input, then waits for the end of that input — the pipe whose
/// write end only the holder has — or for the holder to stop running, and releases for it
/// (§6.3), retrying until the restore reads back or a live holder has the switch (at most
/// [`Pace::DEFAULT`]'s total; then the state stays for the next start). Opens no window and prints
/// nothing else (`SOPEN_LOG=debug` lines aside). Returns the exit code: 0, 1 when it gave up or
/// could not start, 2 for an argument it cannot read.
pub fn watchdog_main(holder: &str, dir: &Path) -> u8 {
    let Some(holder) = Holder::parse(holder) else {
        crate::debug!("F-key watchdog: not a holder: {holder:?}");
        return 2;
    };
    let Some(mut switch) = platform_switch() else {
        return 1;
    };
    let watched = watch(
        dir,
        &mut switch,
        holder,
        &Machine::this(),
        io::stdin(),
        say_ready,
        &Pace::DEFAULT,
    );
    match watched {
        Ok(()) => 0,
        Err(e) => {
            crate::debug!("F-key watchdog: {e:#}");
            1
        }
    }
}

/// Prints [`READY`] on stdout, then points stdout at `/dev/null`: the holder stops reading it,
/// and nothing written later can fail on a closed pipe.
pub(crate) fn say_ready() {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{READY}").and_then(|()| out.flush());
    drop(out);
    if let Ok(null) = File::options().write(true).open("/dev/null") {
        use std::os::fd::AsRawFd;
        // SAFETY: both are open descriptors; dup2 replaces fd 1 atomically.
        unsafe { libc::dup2(null.as_raw_fd(), libc::STDOUT_FILENO) };
    }
}

/// The watchdog's body: reads the switch once (it can, or it is not ready), starts reading
/// `input`, calls `ready`, then waits until `input` ends or `holder` no longer runs (checked every
/// `pace.poll`) and releases for `holder`, retrying a release that fails at `pace` until it
/// succeeds — the original read back, a live holder found, or nothing left to do — or `pace.total`
/// has passed (an error: the state stays for the next start).
pub(crate) fn watch(
    dir: &Path,
    switch: &mut dyn Switch,
    holder: Holder,
    m: &Machine,
    input: impl Read + Send + 'static,
    ready: impl FnOnce(),
    pace: &Pace,
) -> Result<()> {
    switch
        .read()
        .context("the watchdog cannot read the switch")?;
    let (ended, end) = mpsc::channel();
    std::thread::spawn(move || {
        let mut input = input;
        let _ = io::copy(&mut input, &mut io::sink());
        let _ = ended.send(());
    });
    ready();
    loop {
        match end.recv_timeout(pace.poll) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if !(m.runs)(&holder) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    let until = Instant::now() + pace.total;
    let mut wait = pace.first;
    loop {
        let e = match release(dir, switch, holder, m) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let now = Instant::now();
        if now >= until {
            return Err(e.context("gave up; the state stays for the next start"));
        }
        crate::debug!("F-key watchdog: {e:#}; again in {wait:?}");
        std::thread::sleep(wait.min(until - now));
        wait = (wait * 2).min(pace.most);
    }
}

// ---------------------------------------------------------------------------------------------
// Processes and boots.

pub(crate) mod process {
    use super::Holder;

    /// Whether the holder still runs: a process with its pid runs, is not a zombie, and started
    /// when the holder did.
    pub fn runs(holder: &Holder) -> bool {
        start(holder.pid) == Some(holder.start)
    }

    /// When the process `pid` started — microseconds since the epoch (macOS), clock ticks since
    /// boot (Linux) — or `None` when no such process runs (a zombie does not).
    #[cfg(target_os = "macos")]
    pub fn start(pid: u32) -> Option<u64> {
        let pid = libc::c_int::try_from(pid).ok()?;
        // SAFETY: proc_bsdinfo is plain data; proc_pidinfo writes at most `size` bytes into it.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if got != size || info.pbi_status == libc::SZOMB {
            return None;
        }
        Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }

    #[cfg(target_os = "linux")]
    pub fn start(pid: u32) -> Option<u64> {
        parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn start(_pid: u32) -> Option<u64> {
        None
    }

    /// `/proc/<pid>/stat`'s start time (field 22), `None` for a zombie or a dead process (state
    /// `Z`, `X`). The name in field 2 may hold spaces and `)`: the fields after it are read from
    /// the last `)` on.
    #[cfg(any(target_os = "linux", test))]
    pub fn parse_stat(text: &str) -> Option<u64> {
        let rest = &text[text.rfind(')')? + 1..];
        let mut fields = rest.split_whitespace();
        let state = fields.next()?;
        if state == "Z" || state == "X" {
            return None;
        }
        // Field 3 was the state: field 22 is the 19th after it.
        fields.nth(18)?.parse().ok()
    }

    /// This boot's identity: `kern.bootsessionuuid` (macOS), `/proc/sys/kernel/random/boot_id`
    /// (Linux).
    #[cfg(target_os = "macos")]
    pub fn boot() -> Option<String> {
        let mut buf = [0u8; 128];
        let mut len = buf.len();
        // SAFETY: the name is a C string; the kernel writes at most `len` bytes into `buf`.
        let failed = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        } != 0;
        if failed {
            return None;
        }
        let text = std::ffi::CStr::from_bytes_until_nul(&buf[..len.min(buf.len())])
            .ok()?
            .to_str()
            .ok()?;
        (!text.is_empty()).then(|| text.to_string())
    }

    #[cfg(target_os = "linux")]
    pub fn boot() -> Option<String> {
        let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub fn boot() -> Option<String> {
        None
    }
}
