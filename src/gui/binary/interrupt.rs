//! Ctrl-C and SIGTERM during a "Save to…" that runs without a window (`SOPEN_CHOICE=save`).
//! With a window, Esc or closing it cancels the save (06 §7.5); without one the only way to
//! stop it is a signal, and the default disposition ends sopen mid-way with the bytes moved so
//! far — gigabytes — left hidden in the private folder. So while such a save runs, SIGINT and
//! SIGTERM cancel it instead: the transfer stops between two chunks, the part and its private
//! folder are removed through the save's own cancel path, nothing is published, and sopen ends
//! with 128 + the signal's number ([`Interrupted`]).
//!
//! sopen had a process-wide signal handler once, and S1-M removed it: that one signalled
//! process ids and wrote to a pipe that could block. This one does neither. It is installed
//! for the save alone — the dispositions are put back when the save ends — and all it does is
//! store into two atomics: the signal's number, and "cancelled" into the save's [`Decision`].
//! No allocation, no lock, no I/O, no process is signalled.
//!
//! A cancel is seen between two chunks, so a transfer whose remote has stalled does not see
//! it. The handler is therefore installed for one delivery (`SA_RESETHAND`): the same signal
//! sent again has its default disposition and ends sopen at once. What that leaves in the
//! folder is what a SIGKILL leaves, and the next save there removes it (`sweep_dead`).

use super::Decision;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};

/// The decision of the save being watched: what the handler cancels. A static, so the handler
/// reaches it without a pointer that could outlive what it points at.
static DECISION: Decision = Decision::new();
/// The first signal that arrived during the watch (0: none).
static SIGNAL: AtomicI32 = AtomicI32::new(0);
/// One watch at a time: a sopen runs one windowless save, and tests that run the route several
/// times in one process take turns. Never taken by the handler.
static WATCHING: Mutex<()> = Mutex::new(());

const SIGNALS: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

/// The handler: two atomic stores, both async-signal-safe.
extern "C" fn cancel(signal: libc::c_int) {
    let _ = SIGNAL.compare_exchange(0, signal, Ordering::SeqCst, Ordering::SeqCst);
    DECISION.cancel();
}

/// A windowless save was cancelled by this signal: nothing was saved, and nothing is left of
/// it. sopen ends with [`Interrupted::exit_code`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupted(pub i32);

impl Interrupted {
    /// 128 + the signal's number, as a shell reports a command a signal ended: 130 for SIGINT,
    /// 143 for SIGTERM.
    pub fn exit_code(&self) -> u8 {
        128u8.saturating_add(u8::try_from(self.0).unwrap_or(0))
    }
}

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(super::SAVE_CANCELLED)
    }
}

impl std::error::Error for Interrupted {}

/// SIGINT and SIGTERM cancel [`Watch::decision`] from [`Watch::start`] until the watch ends
/// (or is dropped), when the dispositions found at the start are put back.
pub(super) struct Watch {
    /// What each signal's disposition was (`None`: left as it was — it was ignored).
    before: [Option<libc::sigaction>; 2],
    _one: MutexGuard<'static, ()>,
}

impl Watch {
    pub(super) fn start() -> Self {
        let one = WATCHING.lock().unwrap_or_else(|p| p.into_inner());
        SIGNAL.store(0, Ordering::SeqCst);
        DECISION.reopen();
        let mut before = [None; 2];
        for (signal, before) in SIGNALS.into_iter().zip(&mut before) {
            // SAFETY: `old` and `new` are whole `sigaction`s; the handler only stores into
            // atomics. A signal that is ignored (a job started with `nohup`, or in the
            // background of a shell without job control) stays ignored.
            unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut old) != 0
                    || old.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                let mut new: libc::sigaction = std::mem::zeroed();
                new.sa_sigaction = cancel as extern "C" fn(libc::c_int) as libc::sighandler_t;
                libc::sigemptyset(&mut new.sa_mask);
                // One delivery (see the module's words on a stalled transfer); a system call
                // the signal interrupts is started again, so the save sees a cancel, never a
                // stray EINTR.
                new.sa_flags = libc::SA_RESTART | libc::SA_RESETHAND;
                if libc::sigaction(signal, &new, std::ptr::null_mut()) == 0 {
                    *before = Some(old);
                }
            }
        }
        Self { before, _one: one }
    }

    /// What the watched save is cancelled through.
    pub(super) fn decision(&self) -> &'static Decision {
        &DECISION
    }

    /// Ends the watch: the dispositions are as they were, and the signal that cancelled the
    /// save is returned — `None` when none arrived, or when one arrived too late to cancel it
    /// (the file was already getting its name: it is saved, and reported as saved).
    pub(super) fn end(self) -> Option<Interrupted> {
        self.restore();
        let signal = SIGNAL.load(Ordering::SeqCst);
        (signal != 0 && DECISION.cancelled()).then_some(Interrupted(signal))
    }

    fn restore(&self) {
        for (signal, before) in SIGNALS.into_iter().zip(&self.before) {
            if let Some(before) = before {
                // SAFETY: the disposition this watch found when it started.
                unsafe { libc::sigaction(signal, before, std::ptr::null_mut()) };
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // Putting a disposition back twice is the same as once.
        self.restore();
    }
}
