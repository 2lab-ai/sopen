//! What a stream sends next (06 §4), with no I/O: each reader's readahead window, the queue by
//! class, and the cap on what is sent and not yet answered.

use super::cache::{shares, Protect, BLOCK, CACHE_BYTES};
use super::ReaderKind;
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// The cap's bounds (06 §4).
pub const CAP_MIN: u64 = 256 << 10;
pub const CAP_MAX: u64 = 4 << 20;
/// The blocks of every wait being served, together: what the cache holds.
pub const HELD_BLOCKS: u64 = CACHE_BYTES / BLOCK;
/// What a status request (a fence, half a poll) counts against what is in flight.
pub const STATUS_BYTES: u64 = 100;

/// The queue's classes, first to last (06 §4). The poll goes between `Demand` and `Prefetch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// A block an Interactive or Media reader is waiting for.
    Demand,
    /// Those readers' margin and readahead.
    Prefetch,
    /// Everything a Bulk reader asks for, its waiting block included.
    Bulk,
}

/// A reader kind's `(ahead, behind)` maximums (06 §4's table).
pub fn limits(kind: ReaderKind) -> (u64, u64) {
    match kind {
        ReaderKind::Interactive => (1 << 20, 256 << 10),
        ReaderKind::Media { bytes_per_s } => (
            bytes_per_s.saturating_mul(10).clamp(2 << 20, 32 << 20),
            bytes_per_s.saturating_mul(2).max(1 << 20),
        ),
        ReaderKind::Bulk => (1 << 20, 0),
    }
}

/// What an access was, against the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// The reader's first.
    First,
    /// The place it already asked: nothing changes.
    Same,
    /// It moved forward to a start at or inside the last window: the window grows.
    Hit,
    /// Anywhere else: the window is 0 again.
    Jump,
}

/// One reader's readahead window, on Linux's on-demand rule (06 §4): 0 at open and after a
/// jump, `max(1 block, 2 × window)` up to `ahead` at each sequential hit.
#[derive(Debug, Default, Clone)]
pub struct Readahead {
    window: u64,
    last: Option<(u64, u64)>,
}

impl Readahead {
    pub fn window(&self) -> u64 {
        self.window
    }

    pub fn access(&mut self, off: u64, end: u64, ahead: u64) -> Access {
        let Some((last_off, last_end)) = self.last else {
            self.last = Some((off, end));
            return Access::First;
        };
        // A frame asking again for what it shows, or for a part of it.
        if off >= last_off && end <= last_end {
            return Access::Same;
        }
        self.last = Some((off, end));
        if off > last_off && off <= last_end.saturating_add(self.window) {
            self.window = (self.window * 2).max(BLOCK).min(ahead);
            Access::Hit
        } else {
            self.window = 0;
            Access::Jump
        }
    }
}

/// The one cap on everything sent (06 §4): `clamp(2 × rate × rtt_min, 256 KiB, 4 MiB)`.
#[derive(Debug, Default, Clone)]
pub struct Admission {
    in_flight: u64,
    max_in_flight: u64,
    /// Delivered bytes/s: an EWMA over about the last second of read completions.
    rate: Option<f64>,
    rtt_min: Option<Duration>,
    last_at: Option<Instant>,
    /// Bytes of replies that came with no time since the one before: they count in the next
    /// interval that has a length.
    carried: u64,
}

impl Admission {
    pub fn in_flight(&self) -> u64 {
        self.in_flight
    }

    pub fn max_in_flight(&self) -> u64 {
        self.max_in_flight
    }

    pub fn rate(&self) -> Option<f64> {
        self.rate
    }

    pub fn cap(&self) -> u64 {
        match (self.rate, self.rtt_min) {
            (Some(rate), Some(rtt)) => {
                ((2.0 * rate * rtt.as_secs_f64()) as u64).clamp(CAP_MIN, CAP_MAX)
            }
            _ => CAP_MIN,
        }
    }

    /// Whether a read of `len` bytes may go now. On an idle channel it always may.
    pub fn admits(&self, len: u64) -> bool {
        self.in_flight == 0 || self.in_flight + len <= self.cap()
    }

    pub fn sent(&mut self, bytes: u64) {
        self.in_flight += bytes;
        self.max_in_flight = self.max_in_flight.max(self.in_flight);
    }

    pub fn answered(&mut self, bytes: u64) {
        self.in_flight = self.in_flight.saturating_sub(bytes);
    }

    /// A new channel: nothing is out on it.
    pub fn idle(&mut self) {
        self.in_flight = 0;
    }

    /// A read's reply of `bytes`, `took` after it left, at `at`. Replies cross the link one
    /// behind the other, so its sample runs from the reply before it (or from its own start).
    pub fn delivered(&mut self, bytes: u64, took: Duration, at: Instant) {
        let own = at.checked_sub(took).unwrap_or(at);
        let from = self.last_at.map_or(own, |last| last.max(own));
        self.last_at = Some(at);
        let dt = at.saturating_duration_since(from).as_secs_f64();
        self.carried += bytes;
        if self.carried == 0 || dt <= 0.0 {
            return;
        }
        let sample = std::mem::take(&mut self.carried) as f64 / dt;
        self.rate = Some(match self.rate {
            None => sample,
            Some(rate) => rate + (1.0 - (-dt).exp()) * (sample - rate),
        });
    }

    /// A small reply's round trip: a status request, or a read of one block at most.
    pub fn small_reply(&mut self, took: Duration) {
        self.rtt_min = Some(self.rtt_min.map_or(took, |t| t.min(took)));
    }
}

/// The rest of a block whose reply came short (06 §2): a batch of its own, with its own fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rest {
    pub block: u64,
    pub off: u64,
    pub len: u32,
    pub class: Class,
    /// Which fetch of the block it continues: one given up meanwhile is not continued.
    pub attempt: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Block(u64),
    Rest(Rest),
    /// `fstat(handle)` and `stat(path)` (06 §5).
    Poll,
}

/// The next thing to send, and whose it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cand {
    /// `None` for the poll.
    pub class: Option<Class>,
    pub reader: Option<u64>,
    pub item: Item,
    waiting: bool,
}

struct ReaderState {
    kind: ReaderKind,
    ra: Readahead,
    cursor: u64,
    /// The blocks it is waiting for, inclusive, and since when. This is its demand: what of
    /// it is neither visible nor on its way is what goes next, and a newer place replaces it.
    span: Option<(u64, u64)>,
    since: Option<Instant>,
    span_seq: u64,
    prefetch: VecDeque<u64>,
    prefetch_seq: u64,
}

impl ReaderState {
    fn bulk(&self) -> bool {
        matches!(self.kind, ReaderKind::Bulk)
    }
}

/// What an access changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Touch {
    pub access: Access,
    /// A sequential run began (the window left 0).
    pub run: bool,
    /// Something new is queued.
    pub queued: bool,
}

/// The readers of one stream and what they have queued.
#[derive(Default)]
pub struct Prefetcher {
    pub adm: Admission,
    readers: BTreeMap<u64, ReaderState>,
    next_id: u64,
    seq: u64,
    rests: VecDeque<Rest>,
    /// A poll is due (06 §5): it goes behind queued demand and ahead of everything else.
    pub poll_due: bool,
}

fn blocks_of(off: u64, end: u64) -> std::ops::RangeInclusive<u64> {
    off / BLOCK..=(end.max(off + 1) - 1) / BLOCK
}

impl Prefetcher {
    pub fn open(&mut self, kind: ReaderKind) -> u64 {
        self.next_id += 1;
        self.readers.insert(
            self.next_id,
            ReaderState {
                kind,
                ra: Readahead::default(),
                cursor: 0,
                span: None,
                since: None,
                span_seq: 0,
                prefetch: VecDeque::new(),
                prefetch_seq: 0,
            },
        );
        self.next_id
    }

    pub fn close(&mut self, id: u64) {
        self.readers.remove(&id);
    }

    pub fn kind(&self, id: u64) -> Option<ReaderKind> {
        self.readers.get(&id).map(|r| r.kind)
    }

    /// The reader's `(ahead, behind)` now: its maximums, scaled when the windows of every live
    /// reader would not fit (06 §3).
    pub fn window_of(&self, id: u64) -> (u64, u64) {
        let wants: Vec<(u64, u64)> = self.readers.values().map(|r| limits(r.kind)).collect();
        let at = self.readers.keys().position(|k| *k == id);
        at.map_or((0, 0), |i| shares(&wants)[i])
    }

    /// What eviction spares now.
    pub fn protect(&self) -> Protect {
        let wants: Vec<(u64, u64)> = self.readers.values().map(|r| limits(r.kind)).collect();
        let scaled = shares(&wants);
        Protect {
            waiting: self.held().into_iter().map(|(_, span)| span).collect(),
            windows: self
                .readers
                .values()
                .zip(scaled)
                .map(|(r, (ahead, behind))| {
                    (
                        r.cursor.saturating_sub(behind) / BLOCK,
                        r.cursor.saturating_add(ahead) / BLOCK,
                    )
                })
                .collect(),
        }
    }

    /// The waits that are being served, oldest first, and their blocks: as many as fit the
    /// cache together ([`HELD_BLOCKS`]). Their blocks are held until read (06 §3), so what is
    /// held never passes the cache — a held block arriving always finds one to evict that is
    /// not — and a wait beyond that is served, in its turn, once an older one has been read.
    fn held(&self) -> Vec<(u64, (u64, u64))> {
        let mut waits: Vec<(u64, u64, (u64, u64))> = self
            .readers
            .iter()
            .filter_map(|(id, r)| Some((r.span_seq, *id, r.span?)))
            .collect();
        waits.sort_unstable();
        let mut blocks = 0;
        waits
            .into_iter()
            .take_while(|(_, _, (first, last))| {
                blocks += last - first + 1;
                blocks <= HELD_BLOCKS
            })
            .map(|(_, id, span)| (id, span))
            .collect()
    }

    /// Whether a reader is waiting for `block`.
    pub fn waits(&self, block: u64) -> bool {
        self.readers
            .values()
            .any(|r| r.span.is_some_and(|(a, b)| (a..=b).contains(&block)))
    }

    /// How many readers are waiting for blocks.
    pub fn waiting_readers(&self) -> usize {
        self.readers.values().filter(|r| r.span.is_some()).count()
    }

    /// Since when reader `id` has been waiting, and for which blocks.
    pub fn waiting(&self, id: u64) -> Option<((u64, u64), Instant)> {
        let r = self.readers.get(&id)?;
        Some((r.span?, r.since?))
    }

    /// Reader `id` reads `[off, end)` (clipped to `size`). `cached` says what is visible, and
    /// `have` what is visible or on its way. Its waiting blocks replace its older unsent ones;
    /// a move resets or grows its window and replaces its queued prefetch (06 §4).
    #[allow(clippy::too_many_arguments)]
    pub fn access(
        &mut self,
        id: u64,
        off: u64,
        end: u64,
        size: u64,
        now: Instant,
        cached: &dyn Fn(u64) -> bool,
        have: &dyn Fn(u64) -> bool,
    ) -> Option<Touch> {
        let (ahead, _) = self.window_of(id);
        let seq = self.seq + 1;
        let r = self.readers.get_mut(&id)?;
        let before = r.ra.window();
        let access = r.ra.access(off, end, ahead);
        let need: Vec<u64> = blocks_of(off, end).filter(|b| !cached(*b)).collect();
        let mut queued = false;
        if need.is_empty() {
            r.span = None;
            r.since = None;
        } else {
            let span = (*blocks_of(off, end).start(), *blocks_of(off, end).end());
            if r.span != Some(span) {
                r.since = Some(now);
                r.span_seq = seq;
                // A new wait: the engine hears of it once, not at every look.
                queued = need.iter().any(|b| !have(*b));
            }
            r.span = Some(span);
        }
        if access != Access::Same {
            r.cursor = off;
            let len = end - off;
            let window = r.ra.window();
            let interactive = matches!(r.kind, ReaderKind::Interactive);
            let reach = if interactive { window.max(len) } else { window };
            let mut targets = VecDeque::new();
            let last = *blocks_of(off, end).end();
            let fwd_end = end.saturating_add(reach).min(size);
            if fwd_end > (last + 1) * BLOCK {
                targets.extend(last + 1..=(fwd_end - 1) / BLOCK);
            }
            if interactive && off > 0 {
                let first = off / BLOCK;
                targets.extend((off.saturating_sub(len) / BLOCK..first).rev());
            }
            targets.retain(|b| !cached(*b));
            queued |= targets.iter().any(|b| !have(*b));
            r.prefetch = targets;
            r.prefetch_seq = seq;
        }
        // A waited block whose reply came short: its rest goes as what the reader's wait is.
        let class = if r.bulk() { Class::Bulk } else { Class::Demand };
        for rest in &mut self.rests {
            if need.contains(&rest.block) && class < rest.class {
                rest.class = class;
                queued = true;
            }
        }
        self.seq = seq;
        Some(Touch {
            access,
            run: access == Access::Hit && before == 0,
            queued,
        })
    }

    /// Reader `id` has read what it waited for.
    pub fn satisfied(&mut self, id: u64) {
        if let Some(r) = self.readers.get_mut(&id) {
            r.span = None;
            r.since = None;
        }
    }

    pub fn push_rest(&mut self, rest: Rest) {
        self.rests.push_back(rest);
    }

    pub fn drop_rest(&mut self, block: u64) {
        self.rests.retain(|r| r.block != block);
    }

    /// Whether a block's rest is queued and not sent.
    pub fn rest_queued(&self, block: u64) -> bool {
        self.rests.iter().any(|r| r.block == block)
    }

    /// Everything queued is dropped (a change, a failure, a reload), every wait with it: a
    /// reader asks again. The readers stay.
    pub fn clear(&mut self, forget_places: bool) {
        self.rests.clear();
        self.poll_due = false;
        for r in self.readers.values_mut() {
            r.prefetch.clear();
            r.span = None;
            r.since = None;
            if forget_places {
                r.ra = Readahead::default();
            }
        }
    }

    /// The next thing to send, by class and oldest first within a class (06 §4), or `None`.
    /// `have` says which blocks are visible or on their way: those are dropped from the queue.
    pub fn candidate(&mut self, have: &dyn Fn(u64) -> bool) -> Option<Cand> {
        // Only the front of each queue is looked at: what is there already goes as it comes up.
        for r in self.readers.values_mut() {
            while r.prefetch.front().is_some_and(|b| have(*b)) {
                r.prefetch.pop_front();
            }
        }
        let held = self.held();
        let rest = |rests: &VecDeque<Rest>, class: Class| {
            rests.iter().find(|r| r.class == class).map(|r| Cand {
                class: Some(class),
                reader: None,
                item: Item::Rest(*r),
                waiting: false,
            })
        };
        // The oldest wait being served that still lacks a block, and that block.
        let waiting = |readers: &BTreeMap<u64, ReaderState>, bulk: bool| {
            held.iter()
                .filter(|(id, _)| readers[id].bulk() == bulk)
                .find_map(|(id, (first, last))| {
                    let block = (*first..=*last).find(|b| !have(*b))?;
                    Some(Cand {
                        class: Some(if bulk { Class::Bulk } else { Class::Demand }),
                        reader: Some(*id),
                        item: Item::Block(block),
                        waiting: true,
                    })
                })
        };
        let ahead = |readers: &BTreeMap<u64, ReaderState>, bulk: bool| {
            readers
                .iter()
                .filter(|(_, r)| r.bulk() == bulk && !r.prefetch.is_empty())
                .min_by_key(|(_, r)| r.prefetch_seq)
                .map(|(id, r)| Cand {
                    class: Some(if bulk { Class::Bulk } else { Class::Prefetch }),
                    reader: Some(*id),
                    item: Item::Block(r.prefetch[0]),
                    waiting: false,
                })
        };
        let poll = self.poll_due.then_some(Cand {
            class: None,
            reader: None,
            item: Item::Poll,
            waiting: false,
        });
        rest(&self.rests, Class::Demand)
            .or_else(|| waiting(&self.readers, false))
            .or(poll)
            .or_else(|| rest(&self.rests, Class::Prefetch))
            .or_else(|| ahead(&self.readers, false))
            // Bulk goes out only while nothing of the classes above is queued.
            .or_else(|| rest(&self.rests, Class::Bulk))
            .or_else(|| waiting(&self.readers, true))
            .or_else(|| ahead(&self.readers, true))
    }

    /// `cand` was sent.
    pub fn take(&mut self, cand: &Cand) {
        match cand.item {
            Item::Poll => self.poll_due = false,
            Item::Rest(rest) => self.drop_rest(rest.block),
            // A waited block needs nothing taken: once sent it is on its way.
            Item::Block(_) if cand.waiting => {}
            Item::Block(block) => {
                if let Some(r) = cand.reader.and_then(|id| self.readers.get_mut(&id)) {
                    r.prefetch.retain(|b| *b != block);
                }
            }
        }
    }

    /// Whether `a` and `b` may travel in one batch: the same reader's reads of one class,
    /// admitted together. A rest is a batch of its own.
    pub fn same_batch(a: &Cand, b: &Cand) -> bool {
        matches!((a.item, b.item), (Item::Block(_), Item::Block(_)))
            && a.class == b.class
            && a.reader == b.reader
    }
}
