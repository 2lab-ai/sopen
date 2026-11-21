//! [`Source`] (06 §5): one remote file open for viewing. The state lives behind one lock
//! ([`Core`]); the [`Engine`] owns the transport and is the only one that sends: each turn it
//! plans what may go (06 §4's admission), waits for the next completion and applies it. In a
//! window the engine runs on a thread of its own; tests step it by hand on the `Fake`'s clock.
//!
//! Validation (06 §5): every batch of reads is followed at once by an `fstat` of the handle, its
//! fence. A block enters the cache only when all of it has arrived and the fence behind its
//! last part returned the fingerprint taken at open. Every request carries the generation it
//! was sent in; a change, a failure or a reload starts a new one, and a completion of an older
//! one is dropped unread. Each fetch of a block carries an attempt id besides, which does the
//! same for a fetch given up within a generation (its fence failed).

use super::cache::{BlockCache, BLOCK};
use super::prefetch::{Access, Cand, Class, Item, Prefetcher, Rest, STATUS_BYTES};
use super::{
    explain, Fetched, ReadError, ReaderKind, Reopen, Result, State, Throttle, Throttled, Waker,
    MAX_READ, POLL_EVERY, RETRY_EVERY,
};
use crate::remote::{Completion, Fingerprint, Ranged, Reply, Stat, Wake};
use bytes::Bytes;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How a blocked `read_at` looks at its cancel flag when nothing wakes it.
const CANCEL_CHECK: Duration = Duration::from_millis(100);
/// `idle` (06 §11): this long after `first` with no input in between.
const IDLE_AFTER: Duration = Duration::from_secs(2);
/// One `read_at` asks at most this much at a time, so a long read is a sequential run.
const READ_CHUNK: usize = 1 << 20;

/// Where a stats line goes.
pub(crate) type Sink = Arc<dyn Fn(&str) + Send + Sync>;
pub(crate) type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// What the open that led to a Source knows (06 §11): the stats lines carry it.
#[derive(Debug, Clone)]
pub struct Opening {
    /// `host:/abs`, as the window's title has it.
    pub label: String,
    /// When the open began: the Enter, the dialog's button, or `--hex`.
    pub began: Instant,
    /// The kind sniff's bytes, read on the open's connection before it became the transport.
    pub sniff: u64,
}

/// What `Source::open` reads from the environment; tests pass their own.
#[derive(Clone)]
pub(crate) struct Settings {
    pub poll_every: Duration,
    pub stats: Option<Sink>,
    pub throttle: Option<Throttle>,
    /// The clock where there is no transport to ask (a failed stream waiting to retry).
    pub clock: Clock,
}

impl Settings {
    pub(crate) fn plain() -> Self {
        Self {
            poll_every: POLL_EVERY,
            stats: None,
            throttle: None,
            clock: Arc::new(Instant::now),
        }
    }

    /// `SOPEN_STREAM_STATS=1` and `SOPEN_STREAM_THROTTLE=<ms>,<bytes/s>` (06 §11).
    fn from_env() -> Self {
        let stats: Option<Sink> = std::env::var("SOPEN_STREAM_STATS")
            .is_ok_and(|v| v == "1")
            .then(|| Arc::new(|line: &str| eprintln!("{line}")) as Sink);
        let throttle = std::env::var("SOPEN_STREAM_THROTTLE")
            .ok()
            .and_then(|v| Throttle::parse(&v));
        Self {
            stats,
            throttle,
            ..Self::plain()
        }
    }
}

/// The counters of 06 §11's stats line; every one is cumulative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// What arrived from the remote, by the class it was sent in.
    pub fetched: Fetched,
    /// The union of the byte ranges readers consumed.
    pub delivered: u64,
    /// The union of the byte ranges the byte server sent.
    pub http_sent: u64,
    pub sniff: u64,
    /// Requests sent: reads, fences and polls.
    pub requests: u64,
    pub max_in_flight: u64,
    /// Sequential runs begun (06 §4).
    pub runs: u64,
    pub jumps: u64,
    /// Responses the byte server ended early (06 §9).
    pub cuts: u64,
}

/// A viewer's events on the stats line (06 §11). `open`, `idle` and `close` are the Source's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The first frame drawn with content from the stream.
    First,
    /// A key, click or drag moved the view to this offset, not yet shown.
    Input(u64),
    /// The first frame showing content for the last input's place.
    Ready,
}

/// What a reader is waiting for (06 §5: over 1 s a viewer says `loading 0x… (N KiB)`, over 10 s
/// `the remote is slow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waiting {
    pub off: u64,
    pub len: u64,
    pub waited: Duration,
}

/// A union of byte ranges.
#[derive(Default)]
struct Ranges {
    map: BTreeMap<u64, u64>,
    total: u64,
}

impl Ranges {
    fn add(&mut self, mut a: u64, mut b: u64) {
        if b <= a {
            return;
        }
        let touching: Vec<u64> = self
            .map
            .range(..=b)
            .rev()
            .take_while(|(_, end)| **end >= a)
            .map(|(start, _)| *start)
            .collect();
        for start in touching {
            let end = self.map.remove(&start).expect("a range just found");
            self.total -= end - start;
            a = a.min(start);
            b = b.max(end);
        }
        self.map.insert(a, b);
        self.total += b - a;
    }
}

enum OutKind {
    Read {
        batch: u64,
        block: u64,
        off: u64,
        len: u32,
        class: Class,
        /// The block's first read (not the rest of a short reply).
        first: bool,
        /// Which fetch of the block it belongs to.
        attempt: u64,
    },
    Fence {
        batch: u64,
    },
    PollHandle,
    PollPath,
}

/// A request sent and not yet answered.
struct Out {
    gen: u64,
    /// What it counts against what is in flight.
    bytes: u64,
    kind: OutKind,
}

/// The reads admitted together for one reader, and the fence sent right behind them.
struct Batch {
    waited: bool,
    /// Every block this batch brought bytes of, with the fetch they belong to: when its fence
    /// fails, each of those fetches is given up — a part no fence vouched for is never shown.
    touched: Vec<(u64, u64)>,
    /// Those whose last byte it brought: visible once its fence returns the fingerprint.
    complete: Vec<(u64, u64)>,
}

/// A block not yet visible: its bytes so far.
struct Partial {
    buf: Vec<u8>,
    want: usize,
    cold: bool,
    /// The fetch these bytes are of. A reply of another fetch of the block — one given up
    /// since it was sent — adds nothing to them.
    attempt: u64,
}

/// A reload asked for and not started.
struct ReloadReq {
    id: u64,
    /// The fingerprint a save expects to find.
    expect: Option<Fingerprint>,
    /// A caller waits for its result.
    wanted: bool,
}

/// One request as the engine sent it (tests).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Traced {
    /// Which transport (1 at open, +1 at each reload or retry) and which ticket on it.
    pub channel: u64,
    pub ticket: u64,
    pub class: Option<Class>,
    pub block: Option<u64>,
    pub bytes: u64,
    /// What was in flight, and the cap, when it was sent.
    pub in_flight: u64,
    pub cap: u64,
}

pub(crate) struct Core {
    pub(crate) state: State,
    pub(crate) gen: u64,
    path: String,
    pub(crate) opened: Stat,
    pub(crate) cache: BlockCache,
    pub(crate) pf: Prefetcher,
    out: HashMap<u64, Out>,
    batches: HashMap<u64, Batch>,
    next_batch: u64,
    /// Blocks with a read out (and which fetch it is of), and blocks whose bytes have come in
    /// part or wait for a fence.
    inflight: HashMap<u64, u64>,
    partial: HashMap<u64, Partial>,
    next_attempt: u64,
    visible: bool,
    poll_out: bool,
    poll_at: Instant,
    poll_every: Duration,
    stats: Stats,
    delivered: Ranges,
    http_sent: Ranges,
    label: String,
    began: Instant,
    sink: Option<Sink>,
    /// `first` was marked and the engine has not stamped it yet; then when it did.
    first_marked: bool,
    idle_at: Option<Instant>,
    /// Reloads asked for, in order: each is run, and none replaces another. Only two plain
    /// ones, neither started, are one.
    reload_queue: VecDeque<ReloadReq>,
    next_reload: u64,
    /// How the reloads a caller waits for went, until it takes its own.
    reload_results: HashMap<u64, std::result::Result<(), String>>,
    /// Which version is open: +1 at each reload that opened one. A reader reads only its own.
    epoch: u64,
    retry: bool,
    last_retry: Option<Instant>,
    drop_transport: bool,
    closed: bool,
    kicked: bool,
    /// A wake was sent to the transport and the engine's wait has not ended on it yet.
    woke: bool,
    wake: Option<Wake>,
    /// Something a viewer shows changed: the waker is called once the lock is released.
    notify: bool,
    #[cfg(test)]
    pub(crate) channel: u64,
    #[cfg(test)]
    pub(crate) trace: Vec<Traced>,
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Core {
    /// Whether `stat` is the fingerprint: the size, and the mtime in whole seconds (all SFTP
    /// carries, 06 §5).
    fn same(&self, stat: &Stat) -> bool {
        stat.size == self.opened.size && secs(stat.mtime) == secs(self.opened.mtime)
    }

    fn live(&self) -> bool {
        matches!(self.state, State::Live | State::Replaced)
    }

    /// Tests: blocks whose bytes are held and not visible, and batches awaiting a fence.
    #[cfg(test)]
    pub(crate) fn held(&self) -> (usize, usize) {
        (self.partial.len() + self.inflight.len(), self.batches.len())
    }

    /// Tests: whether anything waits to be sent.
    #[cfg(test)]
    pub(crate) fn queued(&mut self) -> bool {
        self.candidate().is_some()
    }

    /// Tests: reloads asked for and not started.
    #[cfg(test)]
    pub(crate) fn reloads_queued(&self) -> usize {
        self.reload_queue.len()
    }

    /// Queues a reload and gives its id. A plain one joins a plain one that waits at the back
    /// of the queue; one that carries a save's baseline is always its own.
    fn ask_reload(&mut self, expect: Option<Fingerprint>, wanted: bool) -> u64 {
        if let Some(back) = self.reload_queue.back_mut() {
            if expect.is_none() && back.expect.is_none() && !(wanted && back.wanted) {
                back.wanted |= wanted;
                return back.id;
            }
        }
        self.next_reload += 1;
        self.reload_queue.push_back(ReloadReq {
            id: self.next_reload,
            expect,
            wanted,
        });
        self.next_reload
    }

    pub(crate) fn stats(&self) -> Stats {
        Stats {
            delivered: self.delivered.total,
            http_sent: self.http_sent.total,
            max_in_flight: self.pf.adm.max_in_flight(),
            ..self.stats
        }
    }

    /// 06 §11's line for `event`, to the sink.
    fn line(&self, event: &str, extra: &str) {
        let Some(sink) = &self.sink else {
            return;
        };
        let s = self.stats();
        sink(&format!(
            "stream {event} t={} {} fetched={} demand={} prefetch={} bulk={} delivered={} \
             http_sent={} sniff={} requests={} max_in_flight={} runs={} jumps={} cuts={}{extra}",
            self.began.elapsed().as_millis(),
            self.label,
            s.fetched.total,
            s.fetched.demand,
            s.fetched.prefetch,
            s.fetched.bulk,
            s.delivered,
            s.http_sent,
            s.sniff,
            s.requests,
            s.max_in_flight,
            s.runs,
            s.jumps,
            s.cuts,
        ));
    }

    fn candidate(&mut self) -> Option<Cand> {
        let (cache, inflight, partial) = (&self.cache, &self.inflight, &self.partial);
        let have =
            |b: u64| cache.contains(b) || inflight.contains_key(&b) || partial.contains_key(&b);
        self.pf.candidate(&have)
    }

    fn record(&mut self, ticket: u64, bytes: u64, kind: OutKind) {
        #[cfg(test)]
        {
            let (class, block) = match &kind {
                OutKind::Read { class, block, .. } => (Some(*class), Some(*block)),
                _ => (None, None),
            };
            self.trace.push(Traced {
                channel: self.channel,
                ticket,
                class,
                block,
                bytes,
                in_flight: self.pf.adm.in_flight(),
                cap: self.pf.adm.cap(),
            });
        }
        self.pf.adm.sent(bytes);
        self.stats.requests += 1;
        let gen = self.gen;
        self.out.insert(ticket, Out { gen, bytes, kind });
    }

    /// Sends `cand`'s read in `batch` if the cap admits it.
    fn send_read(&mut self, r: &mut dyn Ranged, cand: &Cand, batch: u64) -> bool {
        let (block, off, len, rest) = match cand.item {
            Item::Block(b) => {
                let off = b * BLOCK;
                let len = BLOCK.min(self.opened.size.saturating_sub(off)) as u32;
                (b, off, len, None)
            }
            Item::Rest(rest) => (rest.block, rest.off, rest.len, Some(rest.attempt)),
            Item::Poll => return false,
        };
        if !self.pf.adm.admits(len as u64) {
            return false;
        }
        self.pf.take(cand);
        let class = cand.class.unwrap_or(Class::Prefetch);
        let ticket = r.read(off, len);
        // A block's first read begins a new fetch of it; a rest continues its own.
        let attempt = rest.unwrap_or_else(|| {
            self.next_attempt += 1;
            self.next_attempt
        });
        self.inflight.insert(block, attempt);
        let kind = OutKind::Read {
            batch,
            block,
            off,
            len,
            class,
            first: rest.is_none(),
            attempt,
        };
        self.record(ticket.0, len as u64, kind);
        true
    }

    /// Sends everything that may go now (06 §4): by class, a batch's reads and at once its
    /// fence, the poll behind queued demand and ahead of the rest.
    fn plan(&mut self, r: &mut dyn Ranged) {
        if !self.live() {
            return;
        }
        if self.visible && !self.poll_out && r.now() >= self.poll_at {
            self.pf.poll_due = true;
        }
        while let Some(cand) = self.candidate() {
            if cand.item == Item::Poll {
                self.pf.take(&cand);
                let ticket = r.fstat();
                self.record(ticket.0, STATUS_BYTES, OutKind::PollHandle);
                let ticket = r.stat_path();
                self.record(ticket.0, STATUS_BYTES, OutKind::PollPath);
                self.poll_out = true;
                self.poll_at = r.now() + self.poll_every;
                continue;
            }
            let batch = self.next_batch;
            if !self.send_read(r, &cand, batch) {
                break;
            }
            self.next_batch += 1;
            let waited = cand.class == Some(Class::Demand);
            self.batches.insert(
                batch,
                Batch {
                    waited,
                    touched: Vec::new(),
                    complete: Vec::new(),
                },
            );
            while let Some(more) = self.candidate() {
                if !Prefetcher::same_batch(&cand, &more) || !self.send_read(r, &more, batch) {
                    break;
                }
            }
            // The fence travels with its batch: nothing queued gets between them.
            let ticket = r.fstat();
            self.record(ticket.0, STATUS_BYTES, OutKind::Fence { batch });
        }
    }

    /// Every fetch out is given up: its reply, when it comes, finds nothing to add to.
    fn drop_everything_out(&mut self) {
        self.gen += 1;
        self.batches.clear();
        self.inflight.clear();
        self.partial.clear();
        self.poll_out = false;
    }

    /// The handle's file is not the one opened any more (06 §5): the cache goes, and nothing
    /// is read until a reload.
    fn changed(&mut self) {
        if !self.live() {
            return;
        }
        self.state = State::Changed;
        self.drop_everything_out();
        self.cache.clear();
        self.pf.clear(false);
        self.notify = true;
    }

    /// A read failed or the channel closed: the cache stays, the transport goes, and the next
    /// read retries.
    fn fail(&mut self, reason: String) {
        if !self.live() {
            return;
        }
        self.state = State::Failed(reason);
        self.drop_everything_out();
        self.pf.clear(false);
        self.drop_transport = true;
        self.notify = true;
    }

    fn protect_and_insert(&mut self, block: u64, part: Partial) {
        let protect = self.pf.protect();
        let bytes = Bytes::from(part.buf);
        self.cache.insert(block, bytes.clone(), part.cold, &protect);
        // Every reader waiting for it holds it from here, whatever the cache keeps.
        self.pf.deliver(block, &bytes);
        self.notify = true;
    }

    /// Gives up fetch `attempt` of `block`: its bytes so far, its rest if queued, and — its
    /// rest already sent — the claim that the block is on its way. The reply still to come
    /// finds no fetch to add to.
    fn give_up(&mut self, block: u64, attempt: u64) {
        if self
            .partial
            .get(&block)
            .is_some_and(|p| p.attempt == attempt)
        {
            self.partial.remove(&block);
            self.pf.drop_rest(block);
        }
        if self.inflight.get(&block) == Some(&attempt) {
            self.inflight.remove(&block);
        }
    }

    /// One completion, in the order the transport sent the requests.
    pub(crate) fn apply(&mut self, c: Completion) {
        let Some(out) = self.out.remove(&c.ticket.0) else {
            return;
        };
        self.pf.adm.answered(out.bytes);
        // The link is measured whatever the reply is for.
        match (&out.kind, c.data()) {
            (OutKind::Read { len, class, .. }, Some(data)) => {
                let n = data.len() as u64;
                self.pf.adm.delivered(n, c.took, c.at);
                // What arrived counts as fetched, shown or not.
                self.stats.fetched.total += n;
                match class {
                    Class::Demand => self.stats.fetched.demand += n,
                    Class::Prefetch => self.stats.fetched.prefetch += n,
                    Class::Bulk => self.stats.fetched.bulk += n,
                }
                if *len as u64 <= BLOCK && data.len() == *len as usize {
                    self.pf.adm.small_reply(c.took);
                }
            }
            (OutKind::Read { .. }, None) => {}
            _ if c.reply.is_ok() => self.pf.adm.small_reply(c.took),
            _ => {}
        }
        // Sent before a change, a failure or a reload: dropped unread. Defense in depth:
        // attempt ids drop stale block replies, and `out` is cleared on reconnect, so no
        // reply of an older generation can reach here today.
        if out.gen != self.gen {
            return;
        }
        match out.kind {
            OutKind::Read {
                batch,
                block,
                off,
                len,
                class,
                first,
                attempt,
            } => {
                // Of a fetch given up since (its earlier part's fence failed): dropped before
                // it is read — its error, its emptiness or its bytes say nothing of the
                // fetch that may have replaced it.
                if self.inflight.get(&block) != Some(&attempt) {
                    return;
                }
                self.inflight.remove(&block);
                let data = match c.reply {
                    Ok(Reply::Data(data)) => data,
                    Ok(Reply::Stat(_)) => {
                        return self.fail(format!("{}: a read was answered with a stat", self.path))
                    }
                    Err(e) => {
                        self.give_up(block, attempt);
                        // A failed prefetch is never shown; it is only retried as demand.
                        if self.pf.waits(block) {
                            self.fail(format!("{e:#}"));
                        }
                        return;
                    }
                };
                let n = data.len();
                if n > len as usize {
                    let path = &self.path;
                    return self.fail(format!(
                        "{path}: the server sent {n} bytes for a read of {len}"
                    ));
                }
                if n == 0 {
                    // The file ends before the size it had at open (06 §2).
                    return self.changed();
                }
                if first {
                    let part = Partial {
                        buf: Vec::with_capacity(len as usize),
                        want: len as usize,
                        cold: class == Class::Bulk,
                        attempt,
                    };
                    self.partial.insert(block, part);
                }
                let Some(part) = self
                    .partial
                    .get_mut(&block)
                    .filter(|p| p.attempt == attempt)
                else {
                    return;
                };
                part.buf.extend_from_slice(&data);
                if let Some(b) = self.batches.get_mut(&batch) {
                    b.touched.push((block, attempt));
                }
                if part.buf.len() < part.want {
                    // A short reply: the rest is read at its own offset, with its own fence.
                    self.pf.push_rest(Rest {
                        block,
                        off: off + n as u64,
                        len: len - n as u32,
                        class: if self.pf.waits(block) && class != Class::Bulk {
                            Class::Demand
                        } else {
                            class
                        },
                        attempt,
                    });
                } else if let Some(b) = self.batches.get_mut(&batch) {
                    b.complete.push((block, attempt));
                }
            }
            OutKind::Fence { batch } => {
                let Some(batch) = self.batches.remove(&batch) else {
                    return;
                };
                match c.reply {
                    Ok(Reply::Stat(Some(stat))) if self.same(&stat) => {
                        for (block, attempt) in batch.complete {
                            if self
                                .partial
                                .get(&block)
                                .is_some_and(|p| p.attempt == attempt)
                            {
                                if let Some(part) = self.partial.remove(&block) {
                                    self.protect_and_insert(block, part);
                                }
                            }
                        }
                    }
                    Ok(Reply::Stat(Some(_))) => self.changed(),
                    Ok(_) => self.fail(format!("{}: a fence was not answered", self.path)),
                    Err(e) => {
                        // No fence vouches for what this batch brought: every fetch it
                        // brought a part of is given up, the short ones with a rest to come too.
                        let waited = batch.waited
                            || batch.touched.iter().any(|(block, _)| self.pf.waits(*block));
                        for (block, attempt) in &batch.touched {
                            self.give_up(*block, *attempt);
                        }
                        if waited {
                            self.fail(format!("{e:#}"));
                        }
                    }
                }
            }
            OutKind::PollHandle => match c.reply {
                Ok(Reply::Stat(Some(stat))) if self.same(&stat) => {}
                Ok(Reply::Stat(Some(_))) => self.changed(),
                Ok(_) => self.fail(format!("{}: a poll was not answered", self.path)),
                Err(e) => self.fail(format!("{e:#}")),
            },
            OutKind::PollPath => {
                self.poll_out = false;
                match c.reply {
                    Ok(Reply::Stat(at_path)) => {
                        let replaced = !at_path.is_some_and(|s| s.is_file && self.same(&s));
                        let state = if replaced {
                            State::Replaced
                        } else {
                            State::Live
                        };
                        if self.live() && self.state != state {
                            self.state = state;
                            self.notify = true;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => self.fail(format!("{e:#}")),
                }
            }
        }
    }

    /// A new transport is in place: a reload takes its fingerprint and drops the cache; a
    /// retry keeps the cache when the fingerprint is the one opened, and is `Changed` otherwise.
    fn install(&mut self, r: &dyn Ranged, reload: Option<Option<Fingerprint>>, now: Instant) {
        // `reload`: `Some` for a reload, with the fingerprint a save expects, if any.
        self.out.clear();
        self.drop_everything_out();
        self.pf.adm.idle();
        self.wake = Some(r.waker());
        #[cfg(test)]
        {
            self.channel += 1;
        }
        let new = r.opened();
        match reload {
            Some(expect) => {
                self.cache.clear();
                self.pf.clear(true);
                self.opened = new;
                self.epoch += 1;
                let as_saved =
                    expect.is_none_or(|e| e.size == new.size && secs(e.mtime) == secs(new.mtime));
                self.state = if as_saved {
                    State::Live
                } else {
                    State::Changed
                };
            }
            None if self.same(&new) => {
                self.pf.clear(false);
                self.state = State::Live;
            }
            None => {
                self.cache.clear();
                self.pf.clear(false);
                self.state = State::Changed;
            }
        }
        self.poll_at = now + self.poll_every;
        self.notify = true;
    }

    /// Reader `id` wants `[off, end)`: queues what is missing. Whether the engine has work.
    fn want(&mut self, id: u64, off: u64, end: u64, now: Instant) -> bool {
        let (cache, inflight, partial) = (&self.cache, &self.inflight, &self.partial);
        let cached = |b: u64| cache.contains(b);
        let have =
            |b: u64| cache.contains(b) || inflight.contains_key(&b) || partial.contains_key(&b);
        let size = self.opened.size;
        let grab = |b: u64| cache.peek(b);
        let Some(touch) = self
            .pf
            .access(id, off, end, size, now, &grab, &cached, &have)
        else {
            return false;
        };
        match touch.access {
            Access::Jump => self.stats.jumps += 1,
            Access::Hit if touch.run => self.stats.runs += 1,
            _ => {}
        }
        touch.queued
    }

    /// Whether reader `id` can read `block` now: it holds it, or the cache has it.
    fn there(&self, id: u64, block: u64) -> bool {
        self.pf.holds(id, block).is_some() || self.cache.contains(block)
    }

    /// `[off, end)` when every block of it is there: from what the reader holds first, then
    /// from the cache.
    fn serve(&mut self, id: u64, off: u64, end: u64) -> Option<Bytes> {
        let (first, last) = (off / BLOCK, (end - 1) / BLOCK);
        if !(first..=last).all(|b| self.there(id, b)) {
            return None;
        }
        let touch = !matches!(self.pf.kind(id), Some(ReaderKind::Bulk));
        let mut piece = |b: u64| {
            // A read makes the block recent in the cache too, when it is still there.
            let cached = self.cache.get(b, touch);
            let block = match self.pf.holds(id, b) {
                Some(held) => held.clone(),
                None => cached.expect("a block just seen"),
            };
            let base = b * BLOCK;
            let from = (off.max(base) - base) as usize;
            let to = ((end.min(base + BLOCK) - base) as usize).min(block.len());
            block.slice(from.min(to)..to)
        };
        let bytes = if first == last {
            piece(first)
        } else {
            let mut all = Vec::with_capacity((end - off) as usize);
            for b in first..=last {
                all.extend_from_slice(&piece(b));
            }
            all.into()
        };
        self.delivered.add(off, off + bytes.len() as u64);
        self.pf.satisfied(id);
        Some(bytes)
    }
}

pub(crate) struct Shared {
    core: Mutex<Core>,
    /// Blocked `read_at`s and `reload`s wait here.
    readers: Condvar,
    /// The engine waits here while it has no transport.
    engine: Condvar,
    waker: Waker,
    clock: Clock,
    #[cfg(test)]
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    #[cfg(test)]
    probe: Mutex<Probe>,
}

/// Tests: a blocked `read_at`'s wait, watched.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Probe {
    /// Runs once, after a `read_at` found its bytes missing and before it takes the lock to
    /// wait for them.
    pub seam: Option<Box<dyn FnOnce() + Send>>,
    /// How long a wait lasts instead of [`CANCEL_CHECK`].
    pub wait: Option<Duration>,
    /// Waits that ended on a notification, and on their timeout.
    pub notified: usize,
    pub timed_out: usize,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Releases the lock, then tells whoever waits: the engine (when `kick`), blocked readers,
    /// and the viewer when something it shows changed.
    fn release(&self, mut core: MutexGuard<'_, Core>, kick: bool) {
        let notify = std::mem::take(&mut core.notify);
        let wake = if kick {
            core.kicked = true;
            core.woke = core.wake.is_some();
            core.wake.clone()
        } else {
            None
        };
        drop(core);
        if kick {
            if let Some(wake) = wake {
                wake.wake();
            }
            self.engine.notify_all();
        }
        if notify {
            self.readers.notify_all();
            (self.waker)();
        }
    }
}

/// What the engine does next.
pub(crate) enum Flow {
    /// Wait for a completion this long (`None`: until one comes, or a wake).
    Wait(Option<Duration>),
    Closed,
}

/// The one sender of a Source: it owns the transport.
pub(crate) struct Engine {
    shared: Arc<Shared>,
    ranged: Option<Box<dyn Ranged>>,
    reopen: Reopen,
    throttle: Option<Throttle>,
    /// Tests: runs once a retry or reload has been taken and the lock released, before the
    /// reconnect looks at anything.
    #[cfg(test)]
    pub(crate) at_handoff: Option<Box<dyn FnMut() + Send>>,
    /// Tests: runs once a reconnect has seen the Source open and let go of the lock, right
    /// before it goes to the network.
    #[cfg(test)]
    pub(crate) at_reopen: Option<Box<dyn FnMut() + Send>>,
}

impl Engine {
    fn now(&self) -> Instant {
        match &self.ranged {
            Some(r) => r.now(),
            None => (self.shared.clock)(),
        }
    }

    /// A reload (`Some`) or a retry (`None`), with no lock held: it waits on the network.
    /// A Source closed since the command was taken opens nothing, and one closed while the
    /// network answered sends nothing on what was opened.
    fn reconnect(&mut self, request: Option<ReloadReq>) {
        #[cfg(test)]
        if let Some(hook) = self.at_handoff.as_mut() {
            hook();
        }
        let reload = request.as_ref().map(|req| req.expect);
        let core = self.shared.lock();
        if core.closed {
            return;
        }
        let path = core.path.clone();
        drop(core);
        #[cfg(test)]
        if let Some(hook) = self.at_reopen.as_mut() {
            hook();
        }
        let got = match (reload.is_some(), self.ranged.take()) {
            // A reload re-opens the path on the connection it has.
            (true, Some(r)) => r.reopen(&path).map_err(|refused| refused.error),
            (_, old) => {
                if let Some(old) = old {
                    old.close();
                }
                let throttle = self.throttle;
                (self.reopen)().map(|r| match throttle {
                    Some(t) => Throttled::wrap(r, t),
                    None => r,
                })
            }
        };
        let mut core = self.shared.lock();
        let result = match got {
            Ok(r) if !core.closed => {
                core.install(r.as_ref(), reload, r.now());
                self.ranged = Some(r);
                Ok(())
            }
            Ok(r) => {
                r.close();
                Err("the stream is closed".to_string())
            }
            Err(e) => {
                let reason = explain(&e);
                if reload.is_some() {
                    // The version that was open is given up; a retry opens whatever is there.
                    core.epoch += 1;
                    core.cache.clear();
                }
                if !core.closed {
                    core.out.clear();
                    core.drop_everything_out();
                    core.pf.adm.idle();
                    core.pf.clear(false);
                    core.wake = None;
                    core.state = State::Failed(reason.clone());
                    core.notify = true;
                }
                Err(reason)
            }
        };
        if let Some(req) = request {
            if req.wanted {
                core.reload_results.insert(req.id, result);
            }
            core.notify = true;
        }
        self.shared.release(core, false);
    }

    /// Everything that needs no completion: commands, timers, and what may be sent now.
    pub(crate) fn pump(&mut self) -> Flow {
        loop {
            let now = self.now();
            let mut core = self.shared.lock();
            core.kicked = false;
            if core.closed {
                drop(core);
                if let Some(r) = self.ranged.take() {
                    r.close();
                }
                return Flow::Closed;
            }
            if std::mem::take(&mut core.drop_transport) {
                core.out.clear();
                core.pf.adm.idle();
                core.wake = None;
                drop(core);
                if let Some(r) = self.ranged.take() {
                    r.close();
                }
                continue;
            }
            if let Some(request) = core.reload_queue.pop_front() {
                drop(core);
                self.reconnect(Some(request));
                continue;
            }
            if std::mem::take(&mut core.retry) && matches!(core.state, State::Failed(_)) {
                let due = core
                    .last_retry
                    .is_none_or(|last| now.saturating_duration_since(last) >= RETRY_EVERY);
                if due {
                    core.last_retry = Some(now);
                    drop(core);
                    self.reconnect(None);
                    continue;
                }
            }
            if std::mem::take(&mut core.first_marked) {
                core.idle_at = Some(now + IDLE_AFTER);
            }
            if core.idle_at.is_some_and(|at| now >= at) {
                core.idle_at = None;
                core.line("idle", "");
            }
            let mut wait = core.idle_at.map(|at| at - now);
            if let Some(r) = self.ranged.as_mut() {
                core.plan(r.as_mut());
                let polls = core.live() && core.visible && !core.poll_out && core.poll_at > now;
                if polls {
                    let until = core.poll_at - now;
                    wait = Some(wait.map_or(until, |w| w.min(until)));
                }
            }
            self.shared.release(core, false);
            return Flow::Wait(wait);
        }
    }

    fn apply(&mut self, c: Completion) {
        let mut core = self.shared.lock();
        core.apply(c);
        self.shared.release(core, false);
    }

    /// One turn: pump, wait up to `limit` for a completion, apply it. Whether one came.
    #[cfg(test)]
    pub(crate) fn step(&mut self, limit: Option<Duration>) -> bool {
        let Flow::Wait(wait) = self.pump() else {
            return false;
        };
        let wait = match (wait, limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // With no bound and nothing out, nothing can come: a stepped test never waits on that.
        let idle = wait.is_none() && self.ranged.as_ref().is_some_and(|r| r.pending() == 0);
        let Some(c) = self
            .ranged
            .as_mut()
            .filter(|_| !idle)
            .and_then(|r| r.next(wait))
        else {
            return false;
        };
        self.apply(c);
        true
    }

    #[cfg(test)]
    pub(crate) fn transport(&mut self) -> Option<&mut Box<dyn Ranged>> {
        self.ranged.as_mut()
    }

    /// The engine's thread.
    fn run(mut self) {
        // `next` returning nothing, unasked and early, twice running: the transport may be gone.
        let mut unasked = 0;
        loop {
            let Flow::Wait(wait) = self.pump() else {
                return;
            };
            let Some(r) = self.ranged.as_mut() else {
                let core = self.shared.lock();
                let parked = |c: &mut Core| !c.kicked && !c.closed;
                let _unused = match wait {
                    Some(d) => {
                        self.shared
                            .engine
                            .wait_timeout_while(core, d, parked)
                            .unwrap_or_else(|e| e.into_inner())
                            .0
                    }
                    None => self
                        .shared
                        .engine
                        .wait_while(core, parked)
                        .unwrap_or_else(|e| e.into_inner()),
                };
                continue;
            };
            let asked = r.now();
            if let Some(c) = r.next(wait) {
                unasked = 0;
                self.apply(c);
                continue;
            }
            let early = wait.is_none_or(|w| r.now().saturating_duration_since(asked) < w);
            let mut core = self.shared.lock();
            // A wake ends one wait: that one was asked for.
            if std::mem::take(&mut core.woke) || core.kicked || !early {
                unasked = 0;
                continue;
            }
            unasked += 1;
            if unasked >= 2 {
                unasked = 0;
                // Ask it something: a transport that is gone answers with its error.
                if core.live() {
                    core.poll_out = false;
                    core.pf.poll_due = true;
                } else {
                    core.drop_transport = true;
                }
            }
        }
    }
}

/// Closes the Source when its last handle drops.
struct Handle(Arc<Shared>);

impl Drop for Handle {
    fn drop(&mut self) {
        let mut core = self.0.lock();
        core.closed = true;
        core.state = State::Closed;
        core.notify = true;
        core.line("close", "");
        self.0.release(core, true);
    }
}

/// One remote file opened for viewing. Cheap to clone; the last drop closes it, without
/// waiting for the network (06 §9). Readers do not keep it open: they see `Closed`.
#[derive(Clone)]
pub struct Source {
    shared: Arc<Shared>,
    _handle: Arc<Handle>,
}

impl Source {
    /// `transport` is the file already open (`RemoteFs::into_ranged`); `reopen` makes a new one
    /// after a failure; `wake` is called when something shown may have changed.
    pub fn open(transport: Box<dyn Ranged>, reopen: Reopen, wake: Waker) -> anyhow::Result<Source> {
        let opening = Opening {
            label: transport.path().to_string(),
            began: Instant::now(),
            sniff: 0,
        };
        Self::open_as(transport, reopen, wake, opening)
    }

    /// [`Source::open`], with what the stats lines say of the open (06 §11).
    pub fn open_as(
        transport: Box<dyn Ranged>,
        reopen: Reopen,
        wake: Waker,
        opening: Opening,
    ) -> anyhow::Result<Source> {
        let (source, engine) = Self::build(transport, reopen, wake, opening, Settings::from_env());
        let thread = std::thread::Builder::new()
            .name("sopen-stream-core".into())
            .spawn(move || engine.run())
            .map_err(|e| anyhow::anyhow!("cannot start the stream's thread: {e}"))?;
        #[cfg(test)]
        {
            *source.shared.thread.lock().unwrap() = Some(thread);
        }
        #[cfg(not(test))]
        drop(thread);
        Ok(source)
    }

    /// The Source and its engine, not yet running.
    pub(crate) fn build(
        transport: Box<dyn Ranged>,
        reopen: Reopen,
        wake: Waker,
        opening: Opening,
        settings: Settings,
    ) -> (Source, Engine) {
        let transport = match settings.throttle {
            Some(t) => Throttled::wrap(transport, t),
            None => transport,
        };
        let core = Core {
            state: State::Live,
            gen: 0,
            path: transport.path().to_string(),
            opened: transport.opened(),
            cache: BlockCache::default(),
            pf: Prefetcher::default(),
            out: HashMap::new(),
            batches: HashMap::new(),
            next_batch: 0,
            inflight: HashMap::new(),
            partial: HashMap::new(),
            next_attempt: 0,
            visible: true,
            poll_out: false,
            poll_at: transport.now() + settings.poll_every,
            poll_every: settings.poll_every,
            stats: Stats {
                sniff: opening.sniff,
                ..Stats::default()
            },
            delivered: Ranges::default(),
            http_sent: Ranges::default(),
            label: opening.label,
            began: opening.began,
            sink: settings.stats,
            first_marked: false,
            idle_at: None,
            reload_queue: VecDeque::new(),
            next_reload: 0,
            reload_results: HashMap::new(),
            epoch: 0,
            retry: false,
            last_retry: None,
            drop_transport: false,
            closed: false,
            kicked: false,
            woke: false,
            wake: Some(transport.waker()),
            notify: false,
            #[cfg(test)]
            channel: 1,
            #[cfg(test)]
            trace: Vec::new(),
        };
        core.line("open", "");
        let shared = Arc::new(Shared {
            core: Mutex::new(core),
            readers: Condvar::new(),
            engine: Condvar::new(),
            waker: wake,
            clock: settings.clock,
            #[cfg(test)]
            thread: Mutex::new(None),
            #[cfg(test)]
            probe: Mutex::new(Probe::default()),
        });
        let engine = Engine {
            shared: shared.clone(),
            ranged: Some(transport),
            reopen,
            throttle: settings.throttle,
            #[cfg(test)]
            at_handoff: None,
            #[cfg(test)]
            at_reopen: None,
        };
        let source = Source {
            _handle: Arc::new(Handle(shared.clone())),
            shared,
        };
        (source, engine)
    }

    /// The handle's size at open (or at the last reload).
    pub fn size(&self) -> u64 {
        self.shared.lock().opened.size
    }

    /// `(size, mtime)` at open; no hash.
    pub fn opened(&self) -> Fingerprint {
        let opened = self.shared.lock().opened;
        Fingerprint {
            size: opened.size,
            mtime: opened.mtime,
            sha256: None,
        }
    }

    pub fn state(&self) -> State {
        self.shared.lock().state.clone()
    }

    /// A new reader at no position: its window is 0 until it reads in sequence (06 §4).
    pub fn reader(&self, kind: ReaderKind) -> Reader {
        let mut core = self.shared.lock();
        let (id, epoch) = (core.pf.open(kind), core.epoch);
        drop(core);
        Reader {
            shared: self.shared.clone(),
            id,
            epoch,
        }
    }

    pub fn fetched(&self) -> Fetched {
        self.shared.lock().stats.fetched
    }

    /// The counters of the stats line.
    pub fn stats(&self) -> Stats {
        self.shared.lock().stats()
    }

    /// Re-opens the path, takes a new fingerprint and drops the cache (Cmd/Ctrl+R). Waits for
    /// it — an open and an `fstat` on the stream's connection — so not for a frame:
    /// [`Source::request_reload`] does not wait.
    pub fn reload(&self) -> anyhow::Result<()> {
        self.reload_with(None)
    }

    /// A reload after one of sopen's own writes landed (06 §5): `Live` on the saved file when the
    /// new fingerprint is `baseline`'s `(size, mtime)`, `Changed` when it is not.
    pub fn reload_saved(&self, baseline: Fingerprint) -> anyhow::Result<()> {
        self.reload_with(Some(baseline))
    }

    fn reload_with(&self, expect: Option<Fingerprint>) -> anyhow::Result<()> {
        let mut core = self.shared.lock();
        let id = core.ask_reload(expect, true);
        self.shared.release(core, true);
        let core = self.shared.lock();
        let mut core = self
            .shared
            .readers
            .wait_while(core, |c| !c.reload_results.contains_key(&id) && !c.closed)
            .unwrap_or_else(|e| e.into_inner());
        // Its own result, whatever other reloads ran before or after it.
        match core.reload_results.remove(&id) {
            Some(result) => result.map_err(|reason| anyhow::anyhow!(reason)),
            None => anyhow::bail!("the stream is closed"),
        }
    }

    /// Starts a reload and returns: the waker is called when it is done, and
    /// [`Source::state`] says how it went.
    pub fn request_reload(&self) {
        let mut core = self.shared.lock();
        core.ask_reload(None, false);
        self.shared.release(core, true);
    }

    /// Asks a failed stream to reconnect now (a read of something not cached asks by itself);
    /// at most one attempt per second is made.
    pub fn retry(&self) {
        let mut core = self.shared.lock();
        core.retry = true;
        self.shared.release(core, true);
    }

    /// Whether the viewer is on screen: a hidden one does not poll (06 §5). On by default.
    pub fn set_visible(&self, visible: bool) {
        let mut core = self.shared.lock();
        core.visible = visible;
        self.shared.release(core, true);
    }

    /// Makes every blocked `read_at` look at its cancel flag now (the byte server's shutdown,
    /// 06 §9).
    pub fn interrupt(&self) {
        // Under the lock a reader checks its flag and waits under: none is missed between.
        let _core = self.shared.lock();
        self.shared.readers.notify_all();
    }

    /// A viewer's event on the stats line (06 §11).
    pub fn mark(&self, mark: Mark) {
        let mut core = self.shared.lock();
        match mark {
            Mark::First => {
                core.line("first", "");
                core.first_marked = true;
            }
            Mark::Input(place) => {
                core.first_marked = false;
                core.idle_at = None;
                core.line("input", &format!(" place={place:#x}"));
            }
            Mark::Ready => core.line("ready", ""),
        }
        self.shared.release(core, mark == Mark::First);
    }

    /// The byte server sent `[off, off + len)` to its client (06 §11's `http_sent`).
    pub fn sent_http(&self, off: u64, len: u64) {
        self.shared.lock().http_sent.add(off, off + len);
    }

    /// The byte server ended a response early (06 §9).
    pub fn cut(&self) {
        self.shared.lock().stats.cuts += 1;
    }

    #[cfg(test)]
    pub(crate) fn core(&self) -> MutexGuard<'_, Core> {
        self.shared.lock()
    }

    #[cfg(test)]
    pub(crate) fn probe(&self) -> MutexGuard<'_, Probe> {
        self.shared.probe.lock().unwrap()
    }

    /// Tests: waits for the engine's thread to end.
    #[cfg(test)]
    pub(crate) fn thread(&self) -> Option<std::thread::JoinHandle<()>> {
        self.shared.thread.lock().unwrap().take()
    }
}

/// A position in a Source with its own readahead window (06 §4).
pub struct Reader {
    shared: Arc<Shared>,
    id: u64,
    /// The version it reads (06 §5: blocks of two versions never mix, in a cache or in a
    /// reader's hands).
    epoch: u64,
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.shared.lock().pf.close(self.id);
    }
}

/// What a read finds under the lock.
enum Got {
    Bytes(Bytes),
    Pending,
}

impl Reader {
    /// The file's size at open (or at the last reload).
    pub fn len(&self) -> u64 {
        self.shared.lock().opened.size
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Moves the reader to the version open now, at no position: after a reload, a reader
    /// made before it reads nothing (`ReadError::Reloaded`) until this is called.
    pub fn renew(&mut self) {
        let mut core = self.shared.lock();
        if let Some(kind) = core.pf.kind(self.id) {
            core.pf.close(self.id);
            self.id = core.pf.open(kind);
        }
        self.epoch = core.epoch;
    }

    /// What this reader has been waiting for, and for how long.
    pub fn waiting(&self) -> Option<Waiting> {
        let core = self.shared.lock();
        let ((first, last), since) = core.pf.waiting(self.id)?;
        let off = first * BLOCK;
        Some(Waiting {
            off,
            len: ((last + 1) * BLOCK)
                .min(core.opened.size)
                .saturating_sub(off),
            waited: (self.shared.clock)().saturating_duration_since(since),
        })
    }

    /// One look under the lock: the bytes, or the fetch queued.
    fn look(&self, off: u64, len: usize, retry: bool) -> Result<Got> {
        let mut core = self.shared.lock();
        let end = off.saturating_add(len as u64).min(core.opened.size);
        let got = match core.state.clone() {
            State::Closed => Err(ReadError::Closed),
            _ if core.epoch != self.epoch => Err(ReadError::Reloaded),
            State::Changed => Err(ReadError::Changed),
            _ if len > MAX_READ => Err(ReadError::Failed(format!(
                "a read of {len} bytes is longer than one read may be ({MAX_READ})"
            ))),
            // Nothing to read, at the end of the file or of no length: the wait before it is
            // over, as when the reader moves.
            _ if off >= end => {
                core.pf.retire(self.id);
                Ok((Got::Bytes(Bytes::new()), false))
            }
            State::Failed(reason) => match core.serve(self.id, off, end) {
                Some(bytes) => Ok((Got::Bytes(bytes), false)),
                None => {
                    core.retry |= retry;
                    self.shared.release(core, retry);
                    return Err(ReadError::Failed(reason));
                }
            },
            State::Live | State::Replaced => {
                let kick = core.want(self.id, off, end, (self.shared.clock)());
                match core.serve(self.id, off, end) {
                    Some(bytes) => Ok((Got::Bytes(bytes), kick)),
                    None => Ok((Got::Pending, kick)),
                }
            }
        };
        match got {
            Ok((got, kick)) => {
                self.shared.release(core, kick);
                Ok(got)
            }
            Err(e) => Err(e),
        }
    }

    /// Frames: up to `len` bytes at `off` (fewer at the end of the file) when every block is
    /// cached, else `Pending`: the fetch is queued and the waker is called on arrival. A failed
    /// stream still gives what it has cached; a read of anything else asks for a retry.
    pub fn try_read(&mut self, off: u64, len: usize) -> Poll<Result<Bytes>> {
        match self.look(off, len, true) {
            Ok(Got::Bytes(bytes)) => Poll::Ready(Ok(bytes)),
            Ok(Got::Pending) => Poll::Pending,
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    /// Workers and HTTP connections: blocks until `buf` is full or the file ends, and returns
    /// the count. `Err(Cancelled)` once `cancel` is set, `Err(Changed)` when the source changed,
    /// `Err(Failed)` when it failed, `Err(Closed)` when it closed. A failed stream is asked to
    /// retry, as `try_read` asks, so the caller's next call may find it back.
    pub fn read_at(&mut self, off: u64, buf: &mut [u8], cancel: &AtomicBool) -> Result<usize> {
        let mut done = 0;
        while done < buf.len() {
            if cancel.load(Ordering::SeqCst) {
                // A cancelled read waits for nothing: a Reader kept after it holds nothing.
                self.shared.lock().pf.retire(self.id);
                return Err(ReadError::Cancelled);
            }
            let ask = (buf.len() - done).min(READ_CHUNK);
            match self.look(off + done as u64, ask, true)? {
                Got::Bytes(bytes) if bytes.is_empty() => break,
                Got::Bytes(bytes) => {
                    buf[done..done + bytes.len()].copy_from_slice(&bytes);
                    done += bytes.len();
                }
                Got::Pending => {
                    #[cfg(test)]
                    let seam = self.shared.probe.lock().unwrap().seam.take();
                    #[cfg(test)]
                    if let Some(seam) = seam {
                        seam();
                    }
                    let mut core = self.shared.lock();
                    let (first, last) = (
                        (off + done as u64) / BLOCK,
                        (off + (done + ask) as u64 - 1) / BLOCK,
                    );
                    // Nothing came between the look and this lock: wait for a block or a wake.
                    let ready = !core.live() || (first..=last).all(|b| core.there(self.id, b));
                    if cancel.load(Ordering::SeqCst) {
                        core.pf.retire(self.id);
                        return Err(ReadError::Cancelled);
                    }
                    if !ready {
                        #[cfg(not(test))]
                        let limit = CANCEL_CHECK;
                        #[cfg(test)]
                        let limit = self
                            .shared
                            .probe
                            .lock()
                            .unwrap()
                            .wait
                            .unwrap_or(CANCEL_CHECK);
                        let (_core, _ended) = self
                            .shared
                            .readers
                            .wait_timeout(core, limit)
                            .unwrap_or_else(|e| e.into_inner());
                        #[cfg(test)]
                        {
                            let mut probe = self.shared.probe.lock().unwrap();
                            match _ended.timed_out() {
                                true => probe.timed_out += 1,
                                false => probe.notified += 1,
                            }
                        }
                    }
                }
            }
        }
        Ok(done)
    }
}
