//! The watchdog (05 §6.2): every local filesystem call of the tree runs on a pool thread and is
//! abandoned, never awaited, once it has made no progress for 2 s; at most 4 calls are
//! outstanding in the whole process (`T-watchdog`).
//!
//! A call is a closure that blocks in the kernel (a listing, a `statvfs`, …) and gives back what
//! to do with its answer. The pool runs at most [`MAX_CALLS`] calls that have not returned —
//! running or abandoned — so at most that many threads are ever left blocked: a call abandoned
//! keeps its slot until it actually returns (late; its answer is dropped), its thread is written
//! off and a fresh one takes its place, and when every slot is held by an abandoned call no call
//! starts, for any tree in the process ([`Pool::held`]). The 2 s run from the call's last beat,
//! so a long listing that keeps reading is never abandoned. Nothing here ever waits for a
//! call: a closed tree's calls not yet started are dropped, and those running are left to end.

use crate::remote::treefs::{Live, NOT_ANSWERING};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Calls started and not returned, at most, in the whole process (05 §6.2): the pool's workers
/// (the 4 of 05 §6.3's phase 3) and the abandoned calls' cap at once.
pub const MAX_CALLS: usize = 4;

/// The pool's clocks.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// A call without a beat this long is abandoned.
    pub watchdog: Duration,
    /// How often the watchdog looks.
    pub tick: Duration,
    /// A scan reports its progress at most this often.
    pub progress: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            watchdog: NOT_ANSWERING,
            tick: Duration::from_millis(50),
            progress: Duration::from_millis(100),
        }
    }
}

/// What to do with a call's answer, made by the call once it has returned.
pub type Then = Box<dyn FnOnce() + Send>;

/// One call for the pool.
pub struct Job {
    /// Lower runs first; then first come.
    pub prio: u64,
    /// Not wanted any more: dropped before it starts.
    pub dropped: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The call itself, on a pool thread.
    pub work: Box<dyn FnOnce(&Live) -> Then + Send>,
    /// Called once if the call is abandoned (its answer will never be used).
    pub abandoned: Box<dyn FnOnce() + Send>,
}

const RUNNING: u8 = 0;
const DONE: u8 = 1;
const ABANDONED: u8 = 2;

struct Running {
    beat: Arc<AtomicU64>,
    state: Arc<AtomicU8>,
    abandoned: Option<Box<dyn FnOnce() + Send>>,
}

#[derive(Default)]
struct State {
    queue: Vec<(u64, u64, Job)>,
    seq: u64,
    /// Calls started and not returned (running and abandoned).
    out: usize,
    abandoned: usize,
    running: HashMap<u64, Running>,
    /// Worker threads not written off.
    live: usize,
    monitor: bool,
    /// Tests: the threads end once idle.
    stop: bool,
}

struct Inner {
    timing: Timing,
    epoch: Instant,
    state: Mutex<State>,
    /// Workers wait here for a call and a free slot.
    work: Condvar,
    /// The watchdog waits here for running calls.
    watch: Condvar,
}

/// The pool of the tree's local calls. Cheap to clone.
#[derive(Clone)]
pub struct Pool(Arc<Inner>);

impl Pool {
    /// The process's pool: every tree's local calls go through it, so the abandoned calls are
    /// counted for the whole process (05 §6.2).
    pub fn global() -> Pool {
        static GLOBAL: OnceLock<Pool> = OnceLock::new();
        GLOBAL.get_or_init(|| Pool::new(Timing::default())).clone()
    }

    /// A pool of its own (tests: one that leaves calls blocked does not hold the others back).
    pub fn new(timing: Timing) -> Pool {
        Pool(Arc::new(Inner {
            timing,
            epoch: Instant::now(),
            state: Mutex::new(State::default()),
            work: Condvar::new(),
            watch: Condvar::new(),
        }))
    }

    pub fn timing(&self) -> Timing {
        self.0.timing
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Queues `job`; it starts when a slot is free and nothing before it waits.
    pub fn submit(&self, job: Job) {
        let mut s = self.lock();
        s.seq += 1;
        let seq = s.seq;
        s.queue.push((job.prio, seq, job));
        let spawn = MAX_CALLS.saturating_sub(s.live);
        s.live += spawn;
        let monitor = !s.monitor;
        s.monitor = true;
        drop(s);
        for _ in 0..spawn {
            self.spawn_worker();
        }
        if monitor {
            let pool = self.clone();
            let started = std::thread::Builder::new()
                .name("sopen-tree-watchdog".into())
                .spawn(move || pool.monitor());
            if started.is_err() {
                self.lock().monitor = false;
            }
        }
        self.0.work.notify_all();
    }

    fn spawn_worker(&self) {
        let pool = self.clone();
        let spawned = std::thread::Builder::new()
            .name("sopen-tree".into())
            .spawn(move || pool.worker());
        if spawned.is_err() {
            // One worker fewer; the next submit tries again.
            self.lock().live -= 1;
        }
    }

    /// Why no call starts, when that is so (the status row's words, 05 §6.2).
    pub fn held(&self) -> Option<String> {
        let s = self.lock();
        (s.abandoned >= MAX_CALLS).then(|| {
            format!(
                "{} calls not answering; the tree waits for one to return",
                s.abandoned
            )
        })
    }

    /// Calls started and not returned, of which abandoned (tests).
    #[cfg(test)]
    pub fn outstanding(&self) -> (usize, usize) {
        let s = self.lock();
        (s.out, s.abandoned)
    }

    fn now(&self) -> u64 {
        self.0.epoch.elapsed().as_nanos() as u64
    }

    /// Tests: the pool's threads end once idle (none is waited for).
    #[cfg(test)]
    pub fn shutdown(&self) {
        self.lock().stop = true;
        self.0.work.notify_all();
        self.0.watch.notify_all();
    }

    fn worker(self) {
        loop {
            let (n, work, dropped, beat, state) = {
                let mut s = self.lock();
                loop {
                    s.queue.retain(|(_, _, j)| !(j.dropped)());
                    let next = (s.out < MAX_CALLS)
                        .then(|| {
                            s.queue
                                .iter()
                                .enumerate()
                                .min_by_key(|(_, (p, q, _))| (*p, *q))
                                .map(|(i, _)| i)
                        })
                        .flatten();
                    if let Some(i) = next {
                        let (_, n, job) = s.queue.swap_remove(i);
                        let Job {
                            work,
                            abandoned,
                            dropped,
                            ..
                        } = job;
                        s.out += 1;
                        let beat = Arc::new(AtomicU64::new(self.now()));
                        let state = Arc::new(AtomicU8::new(RUNNING));
                        s.running.insert(
                            n,
                            Running {
                                beat: beat.clone(),
                                state: state.clone(),
                                abandoned: Some(abandoned),
                            },
                        );
                        break (n, work, dropped, beat, state);
                    }
                    if s.stop && s.queue.is_empty() {
                        s.live -= 1;
                        return;
                    }
                    s = self.0.work.wait(s).unwrap_or_else(|p| p.into_inner());
                }
            };
            self.0.watch.notify_all();
            let then = {
                let pool = &self;
                let beat_fn = || beat.store(pool.now(), Ordering::Relaxed);
                let stop_fn = || dropped();
                work(&Live {
                    beat: &beat_fn,
                    stop: &stop_fn,
                })
            };
            let ours = state
                .compare_exchange(RUNNING, DONE, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
            {
                let mut s = self.lock();
                s.out -= 1;
                if ours {
                    s.running.remove(&n);
                } else {
                    s.abandoned -= 1;
                }
            }
            self.0.work.notify_all();
            if !ours {
                // Written off: a fresh worker has taken this one's place.
                drop(then);
                return;
            }
            then();
        }
    }

    fn monitor(self) {
        let limit = self.0.timing.watchdog.as_nanos() as u64;
        loop {
            let mut gone = Vec::new();
            let mut fresh = 0;
            {
                let mut s = self.lock();
                while s.running.is_empty() {
                    if s.stop {
                        return;
                    }
                    s = self.0.watch.wait(s).unwrap_or_else(|p| p.into_inner());
                }
                let now = self.now();
                let late: Vec<u64> = s
                    .running
                    .iter()
                    .filter(|(_, r)| now.saturating_sub(r.beat.load(Ordering::Relaxed)) > limit)
                    .map(|(n, _)| *n)
                    .collect();
                for n in late {
                    let Some(r) = s.running.remove(&n) else {
                        continue;
                    };
                    if r.state
                        .compare_exchange(RUNNING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        s.abandoned += 1;
                        s.live -= 1;
                        fresh += 1;
                        gone.extend(r.abandoned);
                    } else {
                        // It returned just now: its worker removes nothing more.
                        s.running.insert(n, r);
                    }
                }
                s.live += fresh;
            }
            for _ in 0..fresh {
                self.spawn_worker();
            }
            for f in gone {
                f();
            }
            std::thread::sleep(self.0.timing.tick);
        }
    }
}
