//! The stream's gates (06 §12: V4, V7, V8, V9, and the Source-level halves of V5). They assert
//! bytes, requests and their order — never wall-clock time: the engine is stepped by hand on the
//! `Fake`'s simulated clock, and the few tests that run the engine's thread assert only what was
//! read and what was sent.

use super::cache::Protect;
use super::prefetch::{Access, Admission, Item, Readahead, STATUS_BYTES};
use super::source::{Engine, Flow, Settings, Traced};
use super::*;
use crate::remote::{
    fake, pattern, Asked, Change, Clock, FakeControl, FakeFile, Fault, Fingerprint, Link, Reply,
    SentRecord, SftpRemote, STAT_REPLY,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicUsize;
use std::sync::{mpsc, Mutex};
use std::time::UNIX_EPOCH;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// The owner's file (06 §12 V1).
const MODEL: u64 = 24_000_000_000;
/// Only catches a hang.
const HANG: Duration = Duration::from_secs(120);
/// No poll within a test that does not ask for one.
const NEVER: Duration = Duration::from_secs(1_000_000);
const TWO_S: Duration = Duration::from_secs(2);

fn sim(latency_ms: u64, rate: u64) -> Link {
    Link {
        latency: Duration::from_millis(latency_ms),
        rate,
        clip: None,
        clock: Clock::Simulated,
    }
}

fn clipped(clip: u32) -> Link {
    Link {
        clip: Some(clip),
        ..Link::gate()
    }
}

/// A Source on the Fake with its engine stepped by hand.
struct Rig {
    source: Option<Source>,
    engine: Engine,
    control: FakeControl,
    wakes: Arc<AtomicUsize>,
    lines: Arc<Mutex<Vec<String>>>,
    /// How often the Source asked for a new connection, and why it gets none.
    reopens: Arc<AtomicUsize>,
    down: Arc<Mutex<Option<String>>>,
}

fn rig_with(file: FakeFile, link: Link, poll_every: Duration, throttle: Option<Throttle>) -> Rig {
    let (transport, control) = fake("/m.bin", file, link);
    let base = transport.now() - control.elapsed();
    let wakes = Arc::new(AtomicUsize::new(0));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let reopens = Arc::new(AtomicUsize::new(0));
    let down = Arc::new(Mutex::new(None::<String>));
    let (count, sink, clock) = (wakes.clone(), lines.clone(), control.clone());
    let (opens, refused, again) = (reopens.clone(), down.clone(), control.clone());
    let settings = Settings {
        poll_every,
        stats: Some(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string())
        })),
        throttle,
        clock: Arc::new(move || base + clock.elapsed()),
    };
    let opening = Opening {
        label: "host:/m.bin".into(),
        began: Instant::now(),
        sniff: 8192,
    };
    let (source, engine) = Source::build(
        transport,
        Box::new(move || {
            opens.fetch_add(1, Ordering::SeqCst);
            match refused.lock().unwrap().clone() {
                Some(why) => Err(anyhow::anyhow!(why)),
                None => again.open(),
            }
        }),
        Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }),
        opening,
        settings,
    );
    Rig {
        source: Some(source),
        engine,
        control,
        wakes,
        lines,
        reopens,
        down,
    }
}

fn rig(size: u64, link: Link) -> Rig {
    rig_with(FakeFile::pattern(size), link, NEVER, None)
}

impl Rig {
    fn source(&self) -> &Source {
        self.source.as_ref().expect("an open source")
    }

    fn reader(&self, kind: ReaderKind) -> Reader {
        self.source().reader(kind)
    }

    fn state(&self) -> State {
        self.source().state()
    }

    fn trace(&self) -> Vec<Traced> {
        self.source().core().trace.clone()
    }

    fn sent(&self) -> Vec<SentRecord> {
        self.control.sent()
    }

    fn wakes(&self) -> usize {
        self.wakes.load(Ordering::SeqCst)
    }

    fn reopens(&self) -> usize {
        self.reopens.load(Ordering::SeqCst)
    }

    /// Whether another block may be sent now.
    fn room(&self) -> bool {
        self.source().core().pf.adm.admits(BLOCK)
    }

    fn pending(&mut self) -> usize {
        self.engine.transport().map_or(0, |r| r.pending())
    }

    /// One turn with something on its way: a stuck reader fails here instead of hanging.
    fn turn(&mut self) -> bool {
        self.engine.pump();
        assert!(self.pending() > 0, "nothing is on its way");
        self.engine.step(None)
    }

    /// Takes exactly one completion (a wake left over from a read ends a wait early).
    fn one(&mut self) {
        for _ in 0..1000 {
            if self.turn() {
                return;
            }
        }
        panic!("no completion came");
    }

    /// Sends what may go, then takes completions until nothing is out and nothing more goes.
    fn drain(&mut self) {
        for _ in 0..1_000_000 {
            self.engine.pump();
            if self.pending() == 0 {
                return;
            }
            self.engine.step(None);
        }
        panic!("the stream did not settle");
    }

    /// Lets `d` of simulated time pass, stepping the engine through it.
    fn run_for(&mut self, d: Duration) {
        let until = self.control.elapsed() + d;
        for _ in 0..1_000_000 {
            let left = until.saturating_sub(self.control.elapsed());
            if left.is_zero() {
                return;
            }
            self.engine.step(Some(left));
        }
        panic!("simulated time did not pass");
    }

    /// Simulated time passing with no channel to wait on.
    fn advance(&self, d: Duration) {
        let mut idle = self.control.open().expect("the path is there");
        assert!(idle.next(Some(d)).is_none());
    }

    /// `len` bytes at `off`, stepping the engine until they are there.
    fn read(&mut self, r: &mut Reader, off: u64, len: usize) -> bytes::Bytes {
        for _ in 0..1_000_000 {
            match r.try_read(off, len) {
                Poll::Ready(got) => return got.expect("a read"),
                Poll::Pending => {
                    self.turn();
                }
            }
        }
        panic!("{len} bytes at {off} never came");
    }

    /// Reads `count` chunks of `chunk` bytes from `from`: each must come whole, with the
    /// pattern's bytes.
    fn stream_exact(&mut self, r: &mut Reader, from: u64, chunk: usize, count: u64) {
        for i in 0..count {
            let off = from + i * chunk as u64;
            let got = self.read(r, off, chunk);
            assert_eq!(got.len(), chunk, "a read of {chunk} bytes at {off}");
            assert!(got[..] == pattern(off, chunk)[..], "bytes at {off}");
        }
    }

    /// Reads `[from, to)` in `chunk`s (the last one what is left), each held to its length.
    fn stream(&mut self, r: &mut Reader, from: u64, to: u64, chunk: usize) {
        let mut off = from;
        while off < to {
            let want = chunk.min((to - off) as usize);
            self.stream_exact(r, off, want, 1);
            off += want as u64;
        }
    }

    /// 06 §4's rule at every request sent, against the Fake's own count of what was out: the
    /// engine's `in_flight` is the Fake's `outstanding`, and a read went only on an idle
    /// channel or within the cap.
    fn check_admission(&self) {
        let (sent, trace) = (self.sent(), self.trace());
        assert_eq!(sent.len(), trace.len(), "every request traced");
        for (s, t) in sent.iter().zip(&trace) {
            assert_eq!(s.ticket.0, t.ticket);
            assert_eq!(s.outstanding, t.in_flight, "{s:?} {t:?}");
            if t.class.is_some() {
                assert!(
                    t.in_flight == 0 || t.in_flight + t.bytes <= t.cap,
                    "admitted over the cap: {t:?}"
                );
            }
        }
        // A batch travels with its fence: behind a read comes a read of its own class, or
        // the fence — never a read of another class, and never the poll.
        for (i, pair) in trace.windows(2).enumerate() {
            if pair[0].channel != pair[1].channel {
                continue;
            }
            if let (Some(a), b) = (pair[0].class, pair[1].class) {
                assert!(
                    b.is_none_or(|b| b == a),
                    "between a read and its fence: {pair:?}"
                );
                if b.is_none() {
                    assert_eq!(sent[i + 1].asked, Asked::Fstat, "{pair:?}");
                }
            }
        }
        if let Some(last) = trace.last() {
            assert_eq!(last.class, None, "the last batch has its fence");
        }
    }
}

fn is_read(s: &SentRecord) -> bool {
    matches!(s.asked, Asked::Read { .. })
}

fn read_of(block: u64) -> Asked {
    Asked::Read {
        off: block * BLOCK,
        len: BLOCK as u32,
    }
}

fn media() -> ReaderKind {
    ReaderKind::Media {
        bytes_per_s: 4 << 20,
    }
}

// ---- the cache (06 §3) ----

#[test]
fn the_cache_keeps_its_bound_and_spares_what_readers_need() {
    let block = |n: u8| bytes::Bytes::from(vec![n; 8]);
    let none = Protect::default();
    let mut cache = BlockCache::with_blocks(4);
    for b in 0..4 {
        cache.insert(b, block(b as u8), false, &none);
    }
    // Least recently used first.
    cache.insert(4, block(4), false, &none);
    assert!(!cache.contains(0) && cache.len() == 4);
    // A read makes a block recent; a Bulk reader's read does not.
    assert_eq!(cache.get(1, true).unwrap()[0], 1);
    assert_eq!(cache.get(2, false).unwrap()[0], 2);
    cache.insert(5, block(5), false, &none);
    assert!(cache.contains(1) && !cache.contains(2));
    // A block inside a reader's window is spared while another can go.
    let window = Protect {
        windows: vec![(3, 3)],
    };
    cache.insert(6, block(6), false, &window);
    assert!(cache.contains(3) && !cache.contains(4));
    // The bound is hard: with every block inside a window, the least recently used goes.
    let all = Protect {
        windows: vec![(0, 100)],
    };
    cache.insert(7, block(7), false, &all);
    assert!(!cache.contains(3) && cache.contains(1) && cache.len() == 4);
    assert_eq!(cache.peek(1).unwrap()[0], 1);
    assert!(cache.peek(3).is_none());
    // A Bulk reader's block goes in at the cold end: the next to go.
    cache.insert(9, block(9), true, &none);
    cache.insert(10, block(10), false, &none);
    assert!(!cache.contains(9) && cache.contains(10));
    assert_eq!(cache.bytes(), 32);
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(CACHE_BYTES / BLOCK, 1024);
}

#[test]
fn windows_share_48_mib_with_floors() {
    let media = limits(ReaderKind::Media {
        bytes_per_s: 10_000_000,
    });
    assert_eq!(media, (32 * MIB, 20_000_000));
    assert_eq!(
        limits(ReaderKind::Media { bytes_per_s: 1000 }),
        (2 * MIB, MIB)
    );
    assert_eq!(limits(ReaderKind::Interactive), (MIB, 256 * KIB));
    assert_eq!(limits(ReaderKind::Bulk), (MIB, 0));
    // They fit: nothing is scaled.
    let fits = [limits(ReaderKind::Interactive), limits(ReaderKind::Bulk)];
    assert_eq!(shares(&fits), fits);
    // Two media readers and a viewer would pass 48 MiB: the large windows come down to an
    // equal share, the small one keeps its own.
    let scaled = shares(&[media, media, limits(ReaderKind::Interactive)]);
    assert_eq!(scaled[2], (MIB, 256 * KIB));
    assert_eq!(scaled[0].0 + scaled[0].1, PROTECTED_BYTES / 3);
    assert!(scaled[0].0 > scaled[0].1, "in proportion: {scaled:?}");
    // Never below the floors.
    let many = vec![media; 400];
    assert!(shares(&many)
        .iter()
        .all(|w| *w == (FLOOR_AHEAD, FLOOR_BEHIND)));
}

// ---- readahead and the cap (06 §4) ----

#[test]
fn the_window_is_zero_until_reads_run_in_sequence() {
    let mut ra = Readahead::default();
    assert_eq!(ra.access(0, 480, MIB), Access::First);
    assert_eq!(ra.window(), 0, "no blind readahead at open");
    assert_eq!(ra.access(0, 480, MIB), Access::Same);
    assert_eq!(
        ra.access(16, 400, MIB),
        Access::Same,
        "a part of the screen"
    );
    let mut windows = Vec::new();
    for row in 1..=6u64 {
        assert_eq!(ra.access(row * 480, (row + 1) * 480, MIB), Access::Hit);
        windows.push(ra.window());
    }
    assert_eq!(
        windows,
        [BLOCK, 2 * BLOCK, 4 * BLOCK, 8 * BLOCK, 16 * BLOCK, MIB],
        "doubling up to the reader's ahead"
    );
    // A start inside the window is still a hit; anything else resets it.
    assert_eq!(
        ra.access(7 * 480 + 500_000, 8 * 480 + 500_000, MIB),
        Access::Hit
    );
    assert_eq!(ra.access(GIB, GIB + 480, MIB), Access::Jump);
    assert_eq!(ra.window(), 0);
    assert_eq!(ra.access(GIB - 480, GIB, MIB), Access::Jump, "backwards");
}

#[test]
fn the_cap_follows_rate_and_round_trip_within_its_clamp() {
    let mut adm = Admission::default();
    assert_eq!(adm.cap(), CAP_MIN, "until there is a rate");
    assert!(adm.admits(10 * MIB), "an idle channel always sends");
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    adm.small_reply(ms(40));
    adm.small_reply(ms(20));
    adm.small_reply(ms(30));
    // 5 MB/s at 20 ms: 200 kB, under the floor.
    adm.delivered(50_000, ms(10), t0 + ms(10));
    assert_eq!(adm.rate(), Some(5e6));
    assert_eq!(adm.cap(), CAP_MIN);
    // 50 MB/s for two seconds: 2 × 50 MB/s × 20 ms = 2 MB.
    for i in 1..=200u32 {
        adm.delivered(500_000, ms(10), t0 + ms(10) + ms(10) * i);
    }
    let cap = adm.cap();
    assert!((1_500_000..=2_000_000).contains(&cap), "{cap}");
    // A second at 0.5 MB/s: the average falls, and the cap with it.
    for i in 1..=100u32 {
        adm.delivered(5_000, ms(10), t0 + ms(2010) + ms(10) * i);
    }
    assert!(adm.cap() < cap);
    for i in 1..=6u32 {
        adm.delivered(4_000_000_000, ms(1000), t0 + ms(3010) + ms(1000) * i);
    }
    assert_eq!(adm.cap(), CAP_MAX);
    adm.sent(CAP_MAX);
    assert!(!adm.admits(1));
    adm.answered(BLOCK);
    assert!(adm.admits(BLOCK) && !adm.admits(BLOCK + 1));
    assert_eq!(adm.max_in_flight(), CAP_MAX);
}

/// 06 §4: within the bulk class a waiting block goes before readahead, whatever their ages —
/// a pass is stalled on the one, and only ahead of itself with the other. Both go behind
/// every foreground item.
#[test]
fn a_bulk_readers_waiting_block_goes_before_older_bulk_readahead() {
    let mut pf = Prefetcher::default();
    let now = Instant::now();
    let none = |_: u64| false;
    let nothing = |_: u64| None;
    let block_of = |pf: &mut Prefetcher| match pf.candidate(&none) {
        Some(cand) => (cand.class, cand.item),
        None => panic!("nothing queued"),
    };
    // An older pass, two sequential reads in: readahead queued, and it waits for nothing
    // once its own block is in.
    let old = pf.open(ReaderKind::Bulk);
    pf.access(old, 0, BLOCK, GIB, now, &nothing, &none, &none);
    pf.access(old, BLOCK, 2 * BLOCK, GIB, now, &nothing, &none, &none);
    pf.satisfied(old);
    assert_eq!(
        block_of(&mut pf),
        (Some(Class::Bulk), Item::Block(2)),
        "its readahead"
    );
    // A newer pass waits for a block: that goes first.
    let new = pf.open(ReaderKind::Bulk);
    pf.access(
        new,
        500 * BLOCK,
        501 * BLOCK,
        GIB,
        now,
        &nothing,
        &none,
        &none,
    );
    assert_eq!(block_of(&mut pf), (Some(Class::Bulk), Item::Block(500)));
    // A viewer's readahead, newer than both, goes before either.
    let viewer = pf.open(ReaderKind::Interactive);
    let seen = |b: u64| b == 900;
    let grab = |b: u64| seen(b).then(bytes::Bytes::new);
    pf.access(
        viewer,
        900 * BLOCK,
        900 * BLOCK + 480,
        GIB,
        now,
        &grab,
        &seen,
        &seen,
    );
    pf.access(
        viewer,
        900 * BLOCK + 480,
        900 * BLOCK + 960,
        GIB,
        now,
        &grab,
        &seen,
        &seen,
    );
    assert_eq!(pf.waiting_readers(), 1, "only the newer pass waits");
    assert_eq!(
        block_of(&mut pf),
        (Some(Class::Prefetch), Item::Block(901)),
        "foreground readahead before any bulk"
    );
}

// ---- the first screen ----

/// One block and its fence, and then nothing: the readahead window is 0 at open, and the
/// screen's margin lies in the same block (06 §12 V1's gate, at the Source).
#[test]
fn a_first_screen_fetches_one_block_validates_it_and_stops() {
    let mut rig = rig(MODEL, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    assert_eq!(r.len(), MODEL);
    assert!(r.try_read(0, 480).is_pending());
    assert_eq!(rig.sent().len(), 0, "the engine sends, not the frame");
    rig.engine.pump();
    let asked: Vec<Asked> = rig.sent().iter().map(|s| s.asked).collect();
    assert_eq!(
        asked,
        [read_of(0), Asked::Fstat],
        "the read, at once its fence"
    );
    rig.one();
    assert!(
        r.try_read(0, 480).is_pending(),
        "the bytes are in, and not shown before their fence returned"
    );
    assert_eq!(rig.wakes(), 0);
    rig.one();
    assert_eq!(rig.wakes(), 1, "the frame is woken");
    let got = rig.read(&mut r, 0, 480);
    assert!(got[..] == pattern(0, 480)[..]);
    rig.run_for(Duration::from_secs(60));
    assert_eq!(rig.sent().len(), 2, "nothing more is sent");
    let stats = rig.source().stats();
    assert_eq!(stats.fetched.total, BLOCK);
    assert_eq!(stats.fetched.demand, BLOCK);
    assert_eq!((stats.delivered, stats.requests), (480, 2));
    assert_eq!(rig.source().fetched(), stats.fetched);
    assert_eq!(rig.source().size(), MODEL);
    assert_eq!(rig.source().opened().sha256, None);
    rig.check_admission();
    // The end of the file: fewer bytes, then none.
    let end = rig.read(&mut r, MODEL - 100, 480);
    assert!(end[..] == pattern(MODEL - 100, 100)[..]);
    assert!(rig.read(&mut r, MODEL, 480).is_empty());
    let last = *rig.sent().iter().rfind(|s| is_read(s)).unwrap();
    let block = (MODEL - 1) / BLOCK;
    assert_eq!(
        last.asked,
        Asked::Read {
            off: block * BLOCK,
            len: (MODEL - block * BLOCK) as u32
        },
        "the last block is asked to the size at open"
    );
}

/// A far jump costs its own block and, where the margin crosses into the next, that one
/// (06 §12 V3's gate at the Source); the margin goes as prefetch, behind the demand.
#[test]
fn a_jump_fetches_its_block_and_its_margin() {
    let mut rig = rig(MODEL, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 0, 480);
    rig.drain();
    let before = rig.source().fetched().total;
    // 20 GiB, 100 bytes before a block's end: the screen straddles two blocks, and its
    // margins lie in them.
    let at = 20 * GIB - 100;
    let got = rig.read(&mut r, at, 480);
    assert!(got[..] == pattern(at, 480)[..]);
    rig.drain();
    assert_eq!(rig.source().fetched().total - before, 2 * BLOCK);
    assert_eq!(rig.source().stats().jumps, 1);
    // In the middle of a block: the block alone.
    rig.read(&mut r, 10 * GIB + 30_000, 480);
    rig.drain();
    assert_eq!(rig.source().fetched().total - before, 3 * BLOCK);
    // At a block's start the margin before it is the block before: prefetch, after the demand.
    let n = rig.trace().len();
    rig.read(&mut r, 5 * GIB, 480);
    rig.drain();
    let classes: Vec<Option<Class>> = rig.trace()[n..].iter().map(|t| t.class).collect();
    assert_eq!(
        classes,
        [Some(Class::Demand), None, Some(Class::Prefetch), None],
        "each batch with its fence"
    );
    let f = rig.source().fetched();
    assert_eq!((f.demand, f.prefetch), (5 * BLOCK, BLOCK));
    rig.check_admission();
}

// ---- V4: admission ----

/// V4: one reader streaming. Every read goes within the cap, the engine's count of what is out
/// is the Fake's, and each batch's fence is the request right behind its reads.
#[test]
fn v4_one_reader_streaming_stays_within_the_cap() {
    let mut rig = rig(MODEL, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.stream(&mut r, 0, 8 * MIB, 4096);
    rig.drain();
    rig.check_admission();
    let (sent, trace) = (rig.sent(), rig.trace());
    for (s, t) in sent.iter().zip(&trace) {
        if t.class == Some(Class::Demand) {
            assert!(s.outstanding <= t.cap, "bytes ahead of a demand: {s:?}");
        }
    }
    // A fence follows every batch's last read: nothing else gets behind a read.
    let mut reads = 0;
    for pair in sent.windows(2) {
        if is_read(&pair[0]) {
            reads += 1;
            assert!(
                is_read(&pair[1]) || pair[1].asked == Asked::Fstat,
                "a batch ends in its fence"
            );
        }
    }
    assert_eq!(sent.last().unwrap().asked, Asked::Fstat);
    let stats = rig.source().stats();
    assert_eq!(stats.runs, 1, "one sequential run");
    assert_eq!(stats.jumps, 0);
    assert!(stats.fetched.prefetch > 6 * MIB, "readahead carried it");
    // 06 §12 V2's bound: what was fetched beyond what was read is one run's window.
    assert!(stats.fetched.total <= stats.delivered + MIB + BLOCK);
    assert_eq!(stats.fetched.total, reads * BLOCK);
    let offsets: Vec<u64> = sent
        .iter()
        .filter_map(|s| match s.asked {
            Asked::Read { off, .. } => Some(off),
            _ => None,
        })
        .collect();
    let distinct: std::collections::BTreeSet<u64> = offsets.iter().copied().collect();
    assert_eq!(distinct.len(), offsets.len(), "no block fetched twice");
    assert!(offsets.iter().all(|off| off % BLOCK == 0));
    assert!(stats.max_in_flight <= CAP_MAX + STATUS_BYTES);
}

/// V4: two viewers and a bulk pass on one Source. A demand waits behind at most the cap and
/// the other viewer's queued block, and no bulk request is sent while demand is queued.
#[test]
fn v4_bulk_is_never_sent_while_demand_waits() {
    let mut rig = rig(MODEL, sim(20, 500_000));
    let mut bulk = rig.reader(ReaderKind::Bulk);
    let mut a = rig.reader(ReaderKind::Interactive);
    let mut b = rig.reader(ReaderKind::Interactive);
    rig.stream(&mut bulk, 0, 8 * BLOCK, BLOCK as usize);
    let _ = bulk.try_read(8 * BLOCK, BLOCK as usize);
    rig.engine.pump();
    let n = rig.trace().len();
    assert!(!rig.room(), "the bulk pass fills the cap");
    // A second pass waits for a block first: a Bulk reader's waiting block is not demand.
    let mut pass = rig.reader(ReaderKind::Bulk);
    let at_pass = 15 * GIB + 1000;
    assert!(pass.try_read(at_pass, 100).is_pending());
    let (at_a, at_b) = (10 * GIB + 1000, 20 * GIB + 1000);
    assert!(a.try_read(at_a, 480).is_pending());
    assert!(b.try_read(at_b, 480).is_pending());
    rig.engine.pump();
    assert_eq!(rig.trace().len(), n, "nothing jumps the cap");
    let got_a = rig.read(&mut a, at_a, 480);
    let got_b = rig.read(&mut b, at_b, 480);
    assert!(got_a[..] == pattern(at_a, 480)[..] && got_b[..] == pattern(at_b, 480)[..]);
    let (sent, trace) = (rig.sent(), rig.trace());
    let place = |block: u64| trace.iter().position(|t| t.block == Some(block)).unwrap();
    let (ia, ib) = (place(at_a / BLOCK), place(at_b / BLOCK));
    assert!(n <= ia && ia < ib, "oldest demand first");
    for t in &trace[n..=ib] {
        assert!(
            matches!(t.class, Some(Class::Demand) | None),
            "sent while demand was queued: {t:?}"
        );
    }
    assert_eq!(sent[ia + 1].asked, Asked::Fstat);
    assert_eq!(sent[ib + 1].asked, Asked::Fstat);
    assert!(sent[ia].outstanding <= trace[ia].cap);
    assert!(sent[ib].outstanding <= trace[ib].cap + BLOCK);
    // The bulk passes get the link back.
    let got = rig.read(&mut pass, at_pass, 100);
    assert!(got[..] == pattern(at_pass, 100)[..]);
    let waited = rig
        .trace()
        .iter()
        .position(|t| t.block == Some(at_pass / BLOCK))
        .unwrap();
    assert!(
        waited > ib,
        "the pass's waiting block went after both demands"
    );
    assert_eq!(rig.trace()[waited].class, Some(Class::Bulk));
    rig.stream(&mut bulk, 8 * BLOCK, 12 * BLOCK, BLOCK as usize);
    assert!(rig.trace()[ib..]
        .iter()
        .any(|t| t.class == Some(Class::Bulk)));
    let f = rig.source().fetched();
    assert_eq!(f.demand, 2 * BLOCK);
    assert!(f.bulk >= 12 * BLOCK && f.prefetch == 0);
    rig.check_admission();
}

/// V4: ten jumps within 100 ms while the cap is full. Only the last place's demand is sent
/// beyond what was in flight: a newer demand replaces the reader's unsent older one.
#[test]
fn v4_ten_jumps_send_only_the_last_place() {
    let mut rig = rig(MODEL, sim(20, 100_000));
    let mut player = rig.reader(media());
    let mut viewer = rig.reader(ReaderKind::Interactive);
    rig.stream(&mut player, 0, 6 * BLOCK, BLOCK as usize);
    rig.engine.pump();
    assert!(!rig.room(), "the cap is full");
    let n = rig.trace().len();
    // Each place starts a block: its margin before it is the block before, as prefetch.
    let place = |i: u64| (i + 1) * GIB;
    for i in 0..10 {
        assert!(viewer.try_read(place(i), 480).is_pending());
        rig.run_for(Duration::from_millis(10));
    }
    assert_eq!(
        rig.trace().len(),
        n,
        "the cap stayed full through the 100 ms: nothing was sent"
    );
    let got = rig.read(&mut viewer, place(9), 480);
    assert!(got[..] == pattern(place(9), 480)[..]);
    rig.drain();
    let trace = rig.trace();
    let demand: Vec<u64> = trace[n..]
        .iter()
        .filter(|t| t.class == Some(Class::Demand))
        .filter_map(|t| t.block)
        .collect();
    assert_eq!(demand, [place(9) / BLOCK], "only the last place");
    for i in 0..9 {
        let block = place(i) / BLOCK;
        assert!(
            trace
                .iter()
                .all(|t| t.block != Some(block) && t.block != Some(block - 1)),
            "place {i} or its margin was sent"
        );
    }
    assert_eq!(
        rig.source().stats().jumps,
        9,
        "the first place was its first read"
    );
    rig.check_admission();
}

/// V4's ten jumps as 06 §4's admission rule runs them on the gate link from an idle start:
/// a read goes whenever what is out plus its block fits the cap, so the first places go at
/// once (256 KiB admits three blocks and their fences), and a later one as each reply frees
/// room. What the rule guarantees is that
/// nothing queues: every demand sent was the reader's latest place when it went, a place
/// passed while the cap was full is never sent, and the last place is.
#[test]
fn v4_ten_jumps_on_an_idle_link_never_queue_a_passed_place() {
    let mut rig = rig(MODEL, Link::gate());
    let mut viewer = rig.reader(ReaderKind::Interactive);
    let place = |i: u64| (i + 1) * GIB + 1000;
    for i in 0..10 {
        let _ = viewer.try_read(place(i), 480);
        rig.run_for(Duration::from_millis(10));
    }
    let got = rig.read(&mut viewer, place(9), 480);
    assert!(got[..] == pattern(place(9), 480)[..]);
    rig.drain();
    let (sent, trace) = (rig.sent(), rig.trace());
    let mut places = Vec::new();
    for (s, t) in sent.iter().zip(&trace) {
        if t.class.is_none() {
            continue;
        }
        assert_eq!(t.class, Some(Class::Demand), "no margin of a place passed");
        // The latest place asked when this went: one every 10 ms.
        let latest = (s.at.as_millis() as u64 / 10).min(9);
        assert_eq!(t.block, Some(place(latest) / BLOCK), "sent at {:?}", s.at);
        places.push(latest);
    }
    // On this link nearly every place finds room when it is asked: only the one asked with
    // the cap full is passed over.
    assert_eq!(places, [0, 1, 2, 3, 4, 5, 7, 8, 9]);
    assert_eq!(rig.source().fetched().total, 9 * BLOCK);
    rig.check_admission();
}

/// V4, fences with the cap full: prefetch fills the cap and more is queued; a demand batch is
/// admitted when room frees, its fence goes right behind its read and ahead of the queued
/// prefetch, and the block is visible when that fence returns — not when the prefetch is.
#[test]
fn v4_a_demand_fence_is_never_held_behind_queued_prefetch() {
    let mut rig = rig(MODEL, sim(20, 500_000));
    let mut player = rig.reader(media());
    let mut viewer = rig.reader(ReaderKind::Interactive);
    rig.stream(&mut player, 0, 6 * BLOCK, BLOCK as usize);
    rig.engine.pump();
    assert!(!rig.room(), "the cap is full");
    let n = rig.trace().len();
    let at = 20 * GIB + 1000;
    assert!(viewer.try_read(at, 480).is_pending());
    rig.engine.pump();
    assert_eq!(rig.trace().len(), n, "it waits for room");
    let got = rig.read(&mut viewer, at, 480);
    assert!(got[..] == pattern(at, 480)[..]);
    let (sent, trace) = (rig.sent(), rig.trace());
    let shown = trace.len();
    let first = trace[n..].iter().position(|t| t.class.is_some()).unwrap() + n;
    assert_eq!(
        trace[first].class,
        Some(Class::Demand),
        "ahead of the queue"
    );
    assert_eq!(trace[first].block, Some(at / BLOCK));
    assert_eq!(
        sent[first + 1].asked,
        Asked::Fstat,
        "its fence right behind it"
    );
    // It was shown with prefetch still queued: that prefetch goes out only now.
    rig.drain();
    let later = &rig.trace()[shown..];
    assert!(
        later.iter().any(|t| t.class == Some(Class::Prefetch)),
        "prefetch was still queued when the demand was shown"
    );
    rig.check_admission();
}

/// V4: a poll due while prefetch and bulk are queued goes out ahead of them, and behind a
/// queued demand.
#[test]
fn v4_the_poll_goes_ahead_of_queued_prefetch_and_bulk() {
    let mut rig = rig_with(FakeFile::pattern(MODEL), sim(20, 500_000), TWO_S, None);
    let mut player = rig.reader(media());
    let mut viewer = rig.reader(ReaderKind::Interactive);
    let mut bulk = rig.reader(ReaderKind::Bulk);
    rig.stream(&mut player, 0, 6 * BLOCK, BLOCK as usize);
    assert!(bulk.try_read(15 * GIB, 100).is_pending());
    let now = rig.control.elapsed();
    assert!(now < Duration::from_millis(1900), "{now:?}");
    rig.run_for(Duration::from_millis(1950) - now);
    let at = 20 * GIB + 1000;
    assert!(viewer.try_read(at, 480).is_pending());
    rig.run_for(Duration::from_secs(1));
    let (sent, trace) = (rig.sent(), rig.trace());
    let path = sent
        .iter()
        .position(|s| s.asked == Asked::StatPath)
        .expect("a poll");
    assert_eq!(
        sent[path - 1].asked,
        Asked::Fstat,
        "fstat(handle), stat(path)"
    );
    assert_eq!(trace[path - 1].class, None);
    let demand = trace
        .iter()
        .position(|t| t.block == Some(at / BLOCK))
        .unwrap();
    assert!(demand < path, "behind the queued demand");
    // From the moment it was due, nothing but demand and fences went before it.
    for (s, t) in sent.iter().zip(&trace).take(path - 1) {
        if s.at >= TWO_S {
            assert!(
                matches!(t.class, Some(Class::Demand) | None),
                "sent ahead of a due poll: {s:?} {t:?}"
            );
        }
    }
    assert!(
        trace[path..]
            .iter()
            .any(|t| t.class == Some(Class::Prefetch)),
        "prefetch was queued behind it"
    );
    assert!(
        trace[..path].iter().all(|t| t.class != Some(Class::Bulk)),
        "bulk waits for the foreground queue"
    );
    assert_eq!(rig.state(), State::Live);
    rig.check_admission();
}

/// V4: the link drops from 5 MB/s to 0.5 MB/s mid-stream. Nothing is recalled, and after the
/// rate updates nothing is sent until what is out plus a block fits the lowered cap.
#[test]
fn v4_when_the_cap_falls_nothing_is_sent_until_it_fits() {
    let mut rig = rig(MODEL, sim(100, 5_000_000));
    let mut player = rig.reader(media());
    rig.stream(&mut player, 0, 40 * MIB, BLOCK as usize);
    rig.engine.pump();
    let high = rig.source().core().pf.adm.cap();
    assert!(high >= 3 * CAP_MIN, "the cap rose above its floor: {high}");
    let out = rig.source().core().pf.adm.in_flight();
    assert!(out > CAP_MIN, "more is out than the floor admits: {out}");
    let n = rig.trace().len();
    rig.control.set_link(|l| l.rate = 500_000);
    // The checkpoint: work is queued, and more is out than the lowered cap itself — which only
    // a fallen cap allows. A pump there sends no read; nothing sent is recalled either.
    let mut held = 0;
    let mut off = 40 * MIB;
    while off < 46 * MIB {
        rig.stream(&mut player, off, off + BLOCK, BLOCK as usize);
        off += BLOCK;
        let (queued, flying, cap) = {
            let mut core = rig.source().core();
            (core.queued(), core.pf.adm.in_flight(), core.pf.adm.cap())
        };
        if queued && flying > cap {
            let before = rig.trace().len();
            rig.engine.pump();
            assert_eq!(
                rig.trace().len(),
                before,
                "sent with {flying} out against a cap of {cap}"
            );
            held += 1;
        }
    }
    assert!(held > 0, "the lowered cap never held queued work back");
    let low = rig.source().core().pf.adm.cap();
    assert!(low < high / 2, "the cap fell: {high} to {low}");
    // Every read after the drop went only when what was out plus its block fit the cap of
    // that moment (check_admission) — and those caps came down below what had been out.
    let trace = rig.trace();
    let after: Vec<&Traced> = trace[n..].iter().filter(|t| t.class.is_some()).collect();
    assert!(after.len() > 8);
    let bound = after
        .iter()
        .filter(|t| t.cap < high / 2 && t.in_flight > 0)
        .count();
    assert!(bound > 0, "the lowered cap held reads back");
    for t in &after {
        if t.cap < out {
            assert!(t.in_flight + t.bytes <= t.cap, "{t:?}");
        }
    }
    // And admission resumes once what is out fits again.
    let before = rig.trace().len();
    rig.drain();
    assert!(
        rig.trace()[before..].iter().any(|t| t.class.is_some()),
        "reads go again"
    );
    rig.check_admission();
}

// ---- V5 at the Source: short replies ----

/// A clipped block is read in parts, each a batch with its own fence, and is shown whole.
#[test]
fn v5_a_short_reply_is_followed_by_its_rest_with_its_own_fence() {
    let mut rig = rig_with(FakeFile::pattern(MIB), clipped(20_000), NEVER, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    assert!(r.try_read(100, 480).is_pending());
    for _ in 0..7 {
        rig.one();
        assert!(
            r.try_read(100, 480).is_pending(),
            "no part of the block is shown before the last part's fence"
        );
    }
    rig.one();
    let got = rig.read(&mut r, 100, 480);
    assert!(got[..] == pattern(100, 480)[..]);
    let asked: Vec<Asked> = rig.sent().iter().map(|s| s.asked).collect();
    let part = |off: u64, len: u32| Asked::Read { off, len };
    assert_eq!(
        asked,
        [
            part(0, 65_536),
            Asked::Fstat,
            part(20_000, 45_536),
            Asked::Fstat,
            part(40_000, 25_536),
            Asked::Fstat,
            part(60_000, 5_536),
            Asked::Fstat,
        ]
    );
    assert!(rig.trace().iter().all(|t| t.class != Some(Class::Prefetch)));
    assert_eq!(rig.source().fetched().demand, BLOCK);
    rig.check_admission();
}

/// V5: a reply short of the block, then the end of the file where the rest should be: the
/// block is not used and the Source is `Changed`.
#[test]
fn v5_a_short_reply_then_the_end_of_the_file_is_a_change() {
    let mut rig = rig_with(FakeFile::pattern(MIB), clipped(20_000), NEVER, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    // Request 2 is the rest of the block: the file is cut right where the first part ended
    // before it is answered, so it gets no bytes.
    rig.control.change_before(2, Change::Truncate(20_000));
    assert!(r.try_read(0, 480).is_pending());
    rig.engine.pump();
    rig.one();
    rig.one();
    assert_eq!(rig.state(), State::Live, "the first part and its fence");
    rig.engine.pump();
    rig.one();
    assert_eq!(
        rig.state(),
        State::Changed,
        "at the empty reply, before any fence"
    );
    assert_eq!(r.try_read(0, 480), Poll::Ready(Err(ReadError::Changed)));
    assert!(rig.source().core().cache.is_empty());
    rig.drain();
    assert_eq!(rig.sent().len(), 4, "and nothing more is read");
}

/// A demand for a block whose reply came short as prefetch: its rest goes as demand, and the
/// block is not read again from its start.
#[test]
fn a_waited_block_with_a_short_reply_sends_its_rest_as_demand() {
    let mut rig = rig_with(FakeFile::pattern(MODEL), clipped(40_000), NEVER, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    // The screen ends a block: its margin is the next block, as prefetch.
    let at = 3 * BLOCK - 480;
    assert!(r.try_read(at, 480).is_pending());
    // Until the prefetched block's first part is in and its rest is queued, not sent.
    while !rig.source().core().pf.rest_queued(3) {
        rig.one();
    }
    let mut other = rig.reader(ReaderKind::Interactive);
    assert!(other.try_read(3 * BLOCK + 1000, 100).is_pending());
    let got = rig.read(&mut other, 3 * BLOCK + 1000, 100);
    assert!(got[..] == pattern(3 * BLOCK + 1000, 100)[..]);
    let trace = rig.trace();
    let of_block: Vec<&Traced> = trace.iter().filter(|t| t.block == Some(3)).collect();
    assert_eq!(
        of_block.len(),
        2,
        "its first part and its rest: {of_block:?}"
    );
    assert_eq!(of_block[0].class, Some(Class::Prefetch));
    assert_eq!(
        (of_block[1].class, of_block[1].bytes),
        (Some(Class::Demand), BLOCK - 40_000)
    );
    let got = rig.read(&mut r, at, 480);
    assert!(got[..] == pattern(at, 480)[..]);
    rig.check_admission();
}

// ---- V7: the remote changes ----

fn write(off: u64, same_second: bool) -> Change {
    Change::Write {
        off,
        bytes: vec![0xEE; 4096],
        same_second,
    }
}

/// V7: the file is rewritten in place right after a successful poll, then a cache miss. The
/// block read across the change is never shown, the Source turns `Changed`, and no block of the
/// new version enters the cache.
#[test]
fn v7_a_change_right_after_a_poll_is_caught_by_the_next_reads_fence() {
    let mut rig = rig_with(FakeFile::pattern(MODEL), Link::gate(), TWO_S, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 1000, 480);
    rig.run_for(Duration::from_millis(2100));
    let polls = rig
        .sent()
        .iter()
        .filter(|s| s.asked == Asked::StatPath)
        .count();
    assert_eq!((polls, rig.state()), (1, State::Live), "a successful poll");
    rig.drain();
    rig.control.change(write(5 * BLOCK + 100, false));
    let wakes = rig.wakes();
    assert!(r.try_read(5 * BLOCK, 480).is_pending());
    let mut steps = 0;
    while rig.state() == State::Live {
        assert!(r.try_read(5 * BLOCK, 480).is_pending(), "never shown");
        rig.one();
        steps += 1;
    }
    assert_eq!(steps, 2, "the miss's read, then its fence");
    assert_eq!(rig.state(), State::Changed);
    assert_eq!(
        rig.state().status().unwrap(),
        "the file changed on the remote — Cmd/Ctrl+R reloads"
    );
    assert_eq!(
        r.try_read(5 * BLOCK, 480),
        Poll::Ready(Err(ReadError::Changed))
    );
    assert_eq!(
        r.try_read(1000, 480),
        Poll::Ready(Err(ReadError::Changed)),
        "blocks from before and after must not mix: the whole cache went"
    );
    assert!(rig.source().core().cache.is_empty());
    assert!(rig.wakes() > wakes, "the frame hears of it");
    rig.drain();
    let n = rig.sent().len();
    rig.run_for(Duration::from_secs(10));
    assert_eq!(rig.sent().len(), n, "it reads no more, and polls no more");
    assert!(rig.source().core().cache.is_empty());
    let cancel = AtomicBool::new(false);
    assert_eq!(r.read_at(0, &mut [0; 16], &cancel), Err(ReadError::Changed));
}

/// 06 §5: polling covers a change while nothing is being read — the handle's `fstat` moved.
#[test]
fn v7_a_change_while_nothing_is_read_is_seen_by_the_poll() {
    let mut rig = rig_with(FakeFile::pattern(MIB), Link::gate(), TWO_S, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 0, 480);
    rig.drain();
    rig.control.change(write(9 * BLOCK, false));
    let n = rig.sent().len();
    rig.run_for(Duration::from_millis(2100));
    assert_eq!(rig.state(), State::Changed);
    let asked: Vec<Asked> = rig.sent()[n..].iter().map(|s| s.asked).collect();
    assert_eq!(asked, [Asked::Fstat, Asked::StatPath], "the poll alone");
    assert_eq!(r.try_read(0, 480), Poll::Ready(Err(ReadError::Changed)));
    assert!(rig.source().core().cache.is_empty());
}

/// V7's generations, fed to the Source directly — on the Fake a fence sent after a change can
/// never match again, so only this shows it: completions sent before a detected change are
/// dropped unread even when their fence returns the fingerprint, and a late error does not
/// turn `Changed` into `Failed`.
#[test]
fn v7_completions_of_an_older_generation_are_dropped_unread() {
    let mut rig = rig(MODEL, Link::gate());
    let mut readers: Vec<Reader> = (0..3)
        .map(|_| rig.reader(ReaderKind::Interactive))
        .collect();
    for (i, r) in readers.iter_mut().enumerate() {
        assert!(r.try_read((i as u64 + 1) * GIB + 1000, 480).is_pending());
    }
    rig.engine.pump();
    let sent = rig.sent();
    assert_eq!(sent.len(), 6, "three reads, each with its fence");
    let opened = rig.engine.transport().unwrap().opened();
    let now = rig.engine.transport().unwrap().now();
    let done = |i: usize, reply: anyhow::Result<Reply>| Completion {
        ticket: sent[i].ticket,
        asked: sent[i].asked,
        reply,
        took: Duration::from_millis(30),
        at: now,
    };
    let block = |i: u64| Ok(Reply::Data(pattern((i + 1) * GIB, 65_536).into()));
    let lost = || Err(anyhow::anyhow!("/m.bin: the connection closed"));
    let moved = Stat {
        mtime: opened.mtime + Duration::from_secs(1),
        ..opened
    };
    let gen = rig.source().core().gen;
    // The first batch's fence shows the change.
    rig.source().core().apply(done(0, block(0)));
    rig.source()
        .core()
        .apply(done(1, Ok(Reply::Stat(Some(moved)))));
    assert_eq!(rig.state(), State::Changed);
    assert_eq!(rig.source().core().gen, gen + 1);
    // The second batch was sent before it: its bytes, and a fence that says "unchanged",
    // arrive after — and are dropped.
    rig.source().core().apply(done(2, block(1)));
    assert_eq!(rig.source().core().held(), (0, 0), "its bytes are dropped");
    rig.source()
        .core()
        .apply(done(3, Ok(Reply::Stat(Some(opened)))));
    assert!(
        rig.source().core().cache.is_empty(),
        "nothing refills the cache"
    );
    assert_eq!(
        rig.source().core().held(),
        (0, 0),
        "dropped unread: none of its bytes are kept"
    );
    assert_eq!(rig.state(), State::Changed);
    // A late error is of the old generation too.
    rig.source().core().apply(done(4, lost()));
    rig.source().core().apply(done(5, lost()));
    assert_eq!(rig.state(), State::Changed, "not Failed");
    assert_eq!(rig.source().core().pf.adm.in_flight(), 0, "all accounted");
    for r in &mut readers {
        assert_eq!(r.try_read(GIB, 16), Poll::Ready(Err(ReadError::Changed)));
    }
}

/// V7: a clipped read whose file changes in place between the first reply and its rest. The
/// block is never shown and the Source turns `Changed` — the rest's own fence sees the write.
#[test]
fn v7_a_change_between_a_short_reply_and_its_rest_shows_no_mixed_block() {
    let mut rig = rig_with(FakeFile::pattern(MIB), clipped(40_000), NEVER, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    // Requests: 0 the block, 1 its fence, 2 the rest, 3 the rest's fence. The write lands
    // before the rest is answered, over bytes the first part brought and bytes the rest will.
    rig.control.change_before(
        2,
        Change::Write {
            off: 30_000,
            bytes: vec![0xEE; 20_000],
            same_second: false,
        },
    );
    assert!(r.try_read(100, 480).is_pending());
    let mut steps = 0;
    while rig.state() == State::Live {
        assert!(r.try_read(100, 480).is_pending(), "never shown");
        rig.one();
        steps += 1;
    }
    assert_eq!(steps, 4, "the block, its fence, the rest, the rest's fence");
    assert_eq!(rig.state(), State::Changed);
    assert!(rig.source().core().cache.is_empty());
    assert_eq!(r.try_read(100, 480), Poll::Ready(Err(ReadError::Changed)));
    let asked: Vec<Asked> = rig.sent().iter().map(|s| s.asked).collect();
    assert_eq!(asked[1], Asked::Fstat);
    assert_eq!(asked[3], Asked::Fstat, "the rest carried its own fence");
    assert_eq!(asked.len(), 4);
}

/// V7: another file renamed over the path. The viewer keeps reading the version opened,
/// consistently, and says it was replaced; Cmd/Ctrl+R then reads the new file.
#[test]
fn v7_a_rename_over_keeps_the_version_opened_until_reload() {
    let mut rig = rig_with(FakeFile::pattern(MODEL), Link::gate(), TWO_S, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 1000, 480);
    let new: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    rig.control
        .change(Change::Replace(FakeFile::bytes(new.clone())));
    rig.run_for(Duration::from_millis(2100));
    assert_eq!(rig.state(), State::Replaced);
    assert_eq!(
        rig.state().status().unwrap(),
        "the remote file was replaced; showing the version opened — Cmd/Ctrl+R reloads"
    );
    // Old blocks stay, and new reads still come from the version opened.
    let old = rig.read(&mut r, 1000, 480);
    assert!(old[..] == pattern(1000, 480)[..]);
    let more = rig.read(&mut r, 7 * GIB + 1000, 480);
    assert!(more[..] == pattern(7 * GIB + 1000, 480)[..]);
    assert_eq!(rig.state(), State::Replaced);
    assert_eq!(rig.source().size(), MODEL);
    // Reload: the path's file, a new fingerprint, the cache dropped.
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!(rig.state(), State::Live);
    assert_eq!((rig.source().size(), r.len()), (200_000, 200_000));
    assert!(rig.source().core().cache.is_empty());
    r.renew();
    let got = rig.read(&mut r, 1000, 480);
    assert!(got[..] == new[1000..1480]);
    rig.drain();
    rig.check_admission();
    // The path removed: the handle still reads, and the poll says the name is gone.
    rig.control.change(Change::Remove);
    rig.run_for(Duration::from_millis(2100));
    assert_eq!(rig.state(), State::Replaced);
    let got = rig.read(&mut r, 100_000, 480);
    assert!(got[..] == new[100_000..100_480]);
}

/// V7, no mixed revisions across a reload: a reader reads one version. After a reload — the
/// owner's, or the one after sopen's own save — a reader made before it gets `Reloaded` for
/// every read, cached or not, until it is renewed; so a worker or an HTTP response that began
/// on the old file never continues with bytes of the new one.
#[test]
fn v7_a_reader_never_continues_into_the_reloaded_version() {
    let mut rig = rig(MIB, Link::gate());
    let mut r = rig.reader(ReaderKind::Media { bytes_per_s: 1000 });
    let old = rig.read(&mut r, 0, 1000);
    assert!(old[..] == pattern(0, 1000)[..]);
    let new = vec![0x55; MIB as usize];
    rig.control
        .change(Change::Replace(FakeFile::bytes(new.clone())));
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!(rig.state(), State::Live);
    // A reader of the new version fills the cache with it.
    let mut fresh = rig.reader(ReaderKind::Interactive);
    let got = rig.read(&mut fresh, 1000, 1000);
    assert!(got[..] == new[1000..2000]);
    let n = rig.sent().len();
    let cancel = AtomicBool::new(false);
    assert_eq!(
        r.try_read(1000, 1000),
        Poll::Ready(Err(ReadError::Reloaded))
    );
    assert_eq!(
        r.read_at(1000, &mut [0; 64], &cancel),
        Err(ReadError::Reloaded)
    );
    assert_eq!(
        r.try_read(5 * BLOCK, 1000),
        Poll::Ready(Err(ReadError::Reloaded))
    );
    rig.drain();
    assert_eq!(rig.sent().len(), n, "and it asks for nothing");
    r.renew();
    let got = rig.read(&mut r, 1000, 1000);
    assert!(got[..] == new[1000..2000]);
    // A reload that fails leaves no version to read: the old readers stay refused.
    rig.control.change(Change::Remove);
    rig.source().request_reload();
    rig.engine.pump();
    assert!(matches!(rig.state(), State::Failed(_)));
    assert_eq!(
        r.try_read(1000, 1000),
        Poll::Ready(Err(ReadError::Reloaded))
    );
}

/// V7: Cmd/Ctrl+R after a change in place reads the file as it is now.
#[test]
fn v7_reload_after_a_change_reads_the_new_version() {
    let mut rig = rig(MIB, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 0, 480);
    rig.control.change(write(100, false));
    assert!(r.try_read(BLOCK + 1000, 480).is_pending());
    rig.drain();
    assert_eq!(rig.state(), State::Changed);
    let before = rig.source().opened();
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!(rig.state(), State::Live);
    assert!(
        rig.source().opened().mtime > before.mtime,
        "a new fingerprint"
    );
    r.renew();
    // A second reload with reads still out: they are never answered on the new channel, and
    // nothing of them counts against it.
    assert!(r.try_read(3 * BLOCK - 100, 480).is_pending());
    rig.engine.pump();
    assert!(rig.pending() >= 2, "reads in flight");
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!(
        rig.source().core().pf.adm.in_flight(),
        0,
        "a new channel is idle"
    );
    assert_eq!(rig.source().core().channel, 3);
    r.renew();
    let got = rig.read(&mut r, 0, 480);
    assert!(got[..100] == pattern(0, 100)[..] && got[100..] == [0xEE; 380]);
    assert_eq!(rig.reopens(), 0, "re-opened on the connection it had");
    rig.drain();
    rig.check_admission();
}

/// V7's documented gap, pinned: a same-size write within the same second as the file's mtime
/// moves nothing a stat carries, so it is not seen — old and new blocks can mix.
#[test]
fn v7_a_same_second_same_size_write_is_not_seen() {
    let mut rig = rig_with(FakeFile::pattern(MIB), Link::gate(), TWO_S, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 0, 480);
    rig.drain();
    rig.control.change(write(0, true));
    rig.control.change(write(4 * BLOCK + 1000, true));
    rig.run_for(Duration::from_millis(2100));
    let new = rig.read(&mut r, 4 * BLOCK + 1000, 480);
    assert!(new[..] == [0xEE; 480], "the new version's block");
    let old = rig.read(&mut r, 0, 480);
    assert!(old[..] == pattern(0, 480)[..], "beside the old version's");
    assert_eq!(
        rig.state(),
        State::Live,
        "SFTP v3's whole seconds: not seen"
    );
}

// ---- V8: close ----

/// V8: the window closes while 4 MiB of prefetch is in flight. Nothing is requested after the
/// close.
#[test]
fn v8_nothing_is_requested_after_the_close() {
    let mut rig = rig(MODEL, sim(50, 100_000_000));
    let mut player = rig.reader(media());
    let mut off = 0;
    while rig.source().core().pf.adm.cap() < CAP_MAX {
        rig.stream(&mut player, off, off + MIB, BLOCK as usize);
        off += MIB;
        assert!(off < 2 * GIB, "the cap never reached its ceiling");
    }
    rig.stream(&mut player, off, off + 8 * MIB, BLOCK as usize);
    rig.engine.pump();
    let out = rig.source().core().pf.adm.in_flight();
    assert!(
        out > CAP_MAX - BLOCK - STATUS_BYTES && out <= CAP_MAX + STATUS_BYTES,
        "4 MiB of prefetch in flight: {out}"
    );
    assert_eq!(
        rig.pending() as u64,
        out / BLOCK + out % BLOCK / STATUS_BYTES
    );
    rig.check_admission();
    let n = rig.sent().len();
    let clone = rig.source().clone();
    drop(rig.source.take());
    assert_eq!(clone.state(), State::Live, "a clone keeps it open");
    drop(clone);
    assert!(rig
        .lines
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .starts_with("stream close "));
    let cancel = AtomicBool::new(false);
    assert_eq!(player.try_read(0, 16), Poll::Ready(Err(ReadError::Closed)));
    assert_eq!(
        player.read_at(0, &mut [0; 16], &cancel),
        Err(ReadError::Closed)
    );
    assert!(matches!(rig.engine.pump(), Flow::Closed));
    assert!(rig.engine.transport().is_none(), "the transport is closed");
    assert!(!rig.engine.step(Some(Duration::from_secs(5))));
    assert_eq!(rig.sent().len(), n, "nothing requested after the close");
}

fn threaded(file: FakeFile, link: Link, reopen: Reopen) -> (Source, FakeControl) {
    let (transport, control) = fake("/m.bin", file, link);
    let source = Source::open(transport, reopen, Arc::new(|| {})).unwrap();
    (source, control)
}

fn no_reopen() -> Reopen {
    Box::new(|| anyhow::bail!("this test has no second connection"))
}

/// V8: the last drop does not wait for the engine — here stuck inside a reconnect the test
/// holds — and once the engine goes on, it sends nothing.
#[test]
fn v8_dropping_the_source_does_not_wait_for_the_engine() {
    bounded(
        "v8_dropping_the_source_does_not_wait_for_the_engine",
        || {
            let (entered_tx, entered) = mpsc::channel();
            let (release, held) = mpsc::channel::<()>();
            let (transport, control) = fake("/m.bin", FakeFile::pattern(MIB), Link::instant());
            let again = control.clone();
            control.fault(0, Fault::Lost);
            let reopen: Reopen = Box::new(move || {
                entered_tx.send(()).unwrap();
                let _ = held.recv();
                again.open()
            });
            let source = Source::open(transport, reopen, Arc::new(|| {})).unwrap();
            let mut r = source.reader(ReaderKind::Interactive);
            let cancel = AtomicBool::new(false);
            let lost = r.read_at(0, &mut [0; 16], &cancel);
            assert!(matches!(lost, Err(ReadError::Failed(_))), "{lost:?}");
            // The next read of something not cached asks for the retry.
            assert!(matches!(
                r.try_read(0, 16),
                Poll::Ready(Err(ReadError::Failed(_)))
            ));
            entered.recv_timeout(HANG).expect("the engine reconnects");
            let thread = source.thread().unwrap();
            let n = control.sent().len();
            // The drop must return while the engine is held inside the reconnect. Should it wait for
            // the engine instead, the bound fails this test, and the unwinding lets go of the
            // reconnect (`release` drops), so nothing stays blocked behind it.
            bounded("dropping the source", move || drop(source));
            assert!(
                !thread.is_finished(),
                "the engine is still inside the reconnect"
            );
            assert_eq!(r.try_read(0, 16), Poll::Ready(Err(ReadError::Closed)));
            drop(release);
            bounded("the engine's thread ending", move || thread.join()).unwrap();
            assert_eq!(control.sent().len(), n, "nothing requested after the close");
        },
    );
}

// ---- V9: the connection is lost ----

/// V9: the channel closes while scrolling. The next read says so and the screen stays; once
/// the transport is back a key continues from the cache, with the same fingerprint.
#[test]
fn v9_a_lost_connection_keeps_the_screen_and_continues_from_the_cache() {
    let mut rig = rig(MODEL, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 0, 480);
    rig.read(&mut r, 480, 480);
    rig.drain();
    let cached = rig.source().core().cache.len();
    let opened = rig.source().opened();
    rig.control.fault(rig.sent().len(), Fault::Lost);
    *rig.down.lock().unwrap() = Some("ssh: connect to host: Connection refused".into());
    let at = 9 * GIB + 1000;
    assert!(r.try_read(at, 480).is_pending());
    rig.engine.pump();
    rig.one();
    let State::Failed(reason) = rig.state() else {
        panic!("{:?}", rig.state())
    };
    assert!(reason.contains("the connection closed"), "{reason}");
    assert_eq!(rig.state().status(), Some(reason.clone()));
    // The screen stays: what is cached still reads.
    let screen = r.try_read(0, 480);
    assert!(matches!(&screen, Poll::Ready(Ok(b)) if b[..] == pattern(0, 480)[..]));
    rig.engine.pump();
    assert_eq!(rig.reopens(), 0, "a cached read retries nothing");
    // The next read of anything else says so, and retries — at most once per second.
    assert_eq!(
        r.try_read(at, 480),
        Poll::Ready(Err(ReadError::Failed(reason.clone())))
    );
    rig.engine.pump();
    assert_eq!(rig.reopens(), 1);
    assert!(
        rig.engine.transport().is_none(),
        "the dead transport is closed"
    );
    let State::Failed(still) = rig.state() else {
        panic!("{:?}", rig.state())
    };
    assert!(still.contains("Connection refused"), "{still}");
    for _ in 0..5 {
        assert!(matches!(r.try_read(at, 480), Poll::Ready(Err(_))));
        rig.engine.pump();
    }
    assert_eq!(rig.reopens(), 1, "within the same second");
    rig.advance(Duration::from_secs(1));
    assert!(matches!(r.try_read(at, 480), Poll::Ready(Err(_))));
    rig.engine.pump();
    assert_eq!(rig.reopens(), 2);
    // The transport is back.
    *rig.down.lock().unwrap() = None;
    rig.advance(Duration::from_secs(1));
    assert!(matches!(r.try_read(at, 480), Poll::Ready(Err(_))));
    let wakes = rig.wakes();
    rig.engine.pump();
    assert_eq!(rig.state(), State::Live);
    assert!(rig.wakes() > wakes, "the frame hears of it");
    assert_eq!(rig.source().opened(), opened, "the same fingerprint");
    assert_eq!(rig.source().core().cache.len(), cached, "the cache is kept");
    let got = rig.read(&mut r, at, 480);
    assert!(got[..] == pattern(at, 480)[..]);
    let again = rig.read(&mut r, 0, 480);
    assert!(again[..] == pattern(0, 480)[..]);
    rig.drain();
    let block0 = rig.sent().iter().filter(|s| s.asked == read_of(0)).count();
    assert_eq!(block0, 1, "what was cached is not fetched again");
    rig.check_admission();
}

/// After a reconnect only `(size, mtime)` can be compared (06 §5): anything else is `Changed`.
#[test]
fn v9_a_file_that_changed_while_the_connection_was_down_is_changed() {
    let mut rig = rig(MIB, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 0, 480);
    rig.drain();
    rig.control.fault(rig.sent().len(), Fault::Lost);
    assert!(r.try_read(2 * BLOCK + 1000, 480).is_pending());
    rig.drain();
    assert!(matches!(rig.state(), State::Failed(_)));
    rig.control.change(Change::Truncate(500_000));
    rig.source().retry();
    rig.engine.pump();
    assert_eq!(rig.state(), State::Changed);
    assert!(rig.source().core().cache.is_empty());
    assert_eq!(r.try_read(0, 480), Poll::Ready(Err(ReadError::Changed)));
    assert_eq!(
        rig.source().size(),
        MIB,
        "the version opened, until a reload"
    );
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!((rig.state(), rig.source().size()), (State::Live, 500_000));
}

/// 06 §5: a failed prefetch is never shown; the block is only fetched again as demand. A
/// status error on a read someone waits for fails the Source.
#[test]
fn a_failed_prefetch_is_never_shown() {
    let mut rig = rig(MODEL, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    // Requests: 0 the screen's block, 1 its fence, 2 the margin's block (prefetch), 3 its fence.
    rig.control.fault(2, Fault::Status("Permission denied"));
    let at = 6 * BLOCK - 480;
    rig.read(&mut r, at, 480);
    rig.drain();
    assert_eq!(rig.sent()[2].asked, read_of(6));
    assert_eq!(rig.state(), State::Live, "not shown");
    assert!(!rig.source().core().cache.contains(6));
    rig.control
        .fault(rig.sent().len(), Fault::Status("Permission denied"));
    assert!(r.try_read(6 * BLOCK, 480).is_pending());
    rig.drain();
    let State::Failed(reason) = rig.state() else {
        panic!("{:?}", rig.state())
    };
    assert_eq!(reason, "cannot read /m.bin: Permission denied");
    let again = *rig.trace().iter().rfind(|t| t.block == Some(6)).unwrap();
    assert_eq!(again.class, Some(Class::Demand), "retried only as demand");
}

/// 06 §5: a block whose fence got an error is not shown, and the reader waiting for it hears
/// the failure.
#[test]
fn a_block_whose_fence_failed_is_not_shown() {
    let mut rig = rig(MIB, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.control.fault(1, Fault::Status("Failure"));
    assert!(r.try_read(1000, 480).is_pending());
    rig.drain();
    assert_eq!(
        rig.state(),
        State::Failed("cannot read /m.bin: Failure".into())
    );
    assert!(rig.source().core().cache.is_empty());
    assert!(matches!(
        r.try_read(1000, 480),
        Poll::Ready(Err(ReadError::Failed(_)))
    ));
    assert_eq!(
        rig.source().fetched().total,
        BLOCK,
        "it arrived, and was not used"
    );
}

/// 06 §6: sshd refusing another session is said in the line's words.
#[test]
fn a_refused_session_is_said_in_words() {
    assert!(session_refused(
        "mux_client_request_session: session request failed: Session open refused by peer"
    ));
    assert!(session_refused(
        "channel 3: open failed: administratively prohibited: open failed"
    ));
    assert!(!session_refused("ssh: connect to host: Connection refused"));
    let plain = anyhow::anyhow!("cannot start sftp on host: no route");
    assert_eq!(explain(&plain), "cannot start sftp on host: no route");
    assert!(SESSION_REFUSED.starts_with("the remote refused another SSH session (sshd allows"));
    assert!(SESSION_REFUSED.ends_with("close a window there and try again"));
    // A reconnect that sshd refuses: the Source says the line.
    let mut rig = rig(MIB, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.control.fault(0, Fault::Lost);
    *rig.down.lock().unwrap() =
        Some("cannot start sftp on host: Session open refused by peer".into());
    assert!(r.try_read(0, 480).is_pending());
    rig.drain();
    assert!(matches!(r.try_read(0, 480), Poll::Ready(Err(_))));
    rig.engine.pump();
    assert_eq!(rig.state(), State::Failed(SESSION_REFUSED.to_string()));
}

// ---- review round 1 ----

/// Runs `f` on a thread of its own and fails, with `what`, when it has not ended within
/// `HANG`: a blocking call under test can only fail, never hang the run.
fn bounded<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (done_tx, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = done_tx.send(f());
    });
    match done.recv_timeout(HANG) {
        Ok(got) => got,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what}: not done in {HANG:?}"),
        // It ended without a result: it panicked, and this is its panic.
        Err(mpsc::RecvTimeoutError::Disconnected) => match worker.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => unreachable!("it ended without a result or a panic"),
        },
    }
}

/// Spins until `cond`, failing with `what` after `HANG`.
fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(started.elapsed() < HANG, "{what}: not in {HANG:?}");
        std::thread::yield_now();
    }
}

/// 06 §5, every part of a block needs a fence that returned the fingerprint: a prefetched
/// block whose first part came short, and whose fence then failed, is given up — its bytes,
/// its queued rest, and the rest already sent, whose own fence succeeds. Nothing of it is
/// cached or served, and a later demand reads the block again from its start.
#[test]
fn a_prefetch_part_whose_fence_failed_is_never_shown() {
    let mut rig = rig_with(FakeFile::pattern(MODEL), clipped(40_000), NEVER, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    // Requests: 0 the screen's block (short), 1 its fence, 2 the margin's block 3 as prefetch
    // (short), 3 its fence — which fails — 4 the screen's rest, 5 that fence, 6 block 3's
    // rest, already sent when the failure arrives, 7 the rest's fence, which succeeds.
    rig.control.fault(3, Fault::Status("Failure"));
    let at = 3 * BLOCK - 480;
    let got = rig.read(&mut r, at, 480);
    assert!(got[..] == pattern(at, 480)[..]);
    rig.drain();
    let part = |off: u64, len: u64| Asked::Read {
        off,
        len: len as u32,
    };
    let asked: Vec<Asked> = rig.sent().iter().map(|s| s.asked).collect();
    assert_eq!(asked[2..4], [read_of(3), Asked::Fstat]);
    assert_eq!(
        asked[6..],
        [part(3 * BLOCK + 40_000, BLOCK - 40_000), Asked::Fstat]
    );
    assert_eq!(rig.state(), State::Live, "a failed prefetch is not shown");
    assert!(
        !rig.source().core().cache.contains(3),
        "a block with a part no fence vouched for was cached"
    );
    assert_eq!(rig.source().core().held(), (0, 0), "nothing of it is kept");
    // A demand reads it again, whole, and cleanly.
    let mut other = rig.reader(ReaderKind::Interactive);
    assert!(other.try_read(3 * BLOCK + 1000, 100).is_pending());
    let got = rig.read(&mut other, 3 * BLOCK + 1000, 100);
    assert!(got[..] == pattern(3 * BLOCK + 1000, 100)[..]);
    assert_eq!(rig.sent()[8].asked, read_of(3), "from its start");
    assert_eq!(rig.trace()[8].class, Some(Class::Demand));
    rig.check_admission();
}

/// Overlapping reloads: each request is its own, and they run in the order asked. A reload
/// that carries a save's baseline is never merged away by a plain one asked while it waits.
#[test]
fn a_saved_reload_is_not_replaced_by_a_plain_one_asked_while_it_waits() {
    let mut rig = rig(MIB, Link::gate());
    let states = Arc::new(Mutex::new(Vec::new()));
    let (seen, watched) = (states.clone(), rig.source().clone());
    rig.engine.at_handoff = Some(Box::new(move || {
        seen.lock().unwrap().push(watched.state());
    }));
    // Not the file there: the saved reload must end `Changed`.
    let wrong = Fingerprint {
        size: 5,
        mtime: UNIX_EPOCH,
        sha256: None,
    };
    let saved = rig.source().clone();
    let (done_tx, done) = mpsc::channel();
    let caller = std::thread::spawn(move || {
        let _ = done_tx.send(saved.reload_saved(wrong).map_err(|e| format!("{e:#}")));
    });
    eventually("the saved reload is queued", || {
        rig.source().core().reloads_queued() == 1
    });
    rig.source().request_reload();
    let mut result = None;
    eventually("the saved reload returns", || {
        rig.engine.pump();
        result = done.try_recv().ok();
        result.is_some()
    });
    caller.join().unwrap();
    assert_eq!(result, Some(Ok(())));
    assert_eq!(
        *states.lock().unwrap(),
        [State::Live, State::Changed],
        "the saved reload ran, with its baseline, before the plain one"
    );
    assert_eq!(rig.source().core().channel, 3, "two reloads");
    assert_eq!(rig.state(), State::Live);
}

/// The other order: a plain reload asked first, then a saved one. Both run.
#[test]
fn a_saved_reload_asked_behind_a_plain_one_still_runs_with_its_baseline() {
    let mut rig = rig(MIB, Link::gate());
    rig.source().request_reload();
    let wrong = Fingerprint {
        size: 5,
        mtime: UNIX_EPOCH,
        sha256: None,
    };
    let saved = rig.source().clone();
    let (done_tx, done) = mpsc::channel();
    let caller = std::thread::spawn(move || {
        let _ = done_tx.send(saved.reload_saved(wrong).map_err(|e| format!("{e:#}")));
    });
    eventually("both reloads are queued", || {
        rig.source().core().reloads_queued() == 2
    });
    let mut result = None;
    eventually("the saved reload returns", || {
        rig.engine.pump();
        result = done.try_recv().ok();
        result.is_some()
    });
    caller.join().unwrap();
    assert_eq!(result, Some(Ok(())));
    assert_eq!(rig.source().core().channel, 3, "two reloads");
    assert_eq!(rig.state(), State::Changed, "the saved one ran last");
}

/// Two plain reloads asked before either starts are one reload; one asked while a reload is
/// running is another.
#[test]
fn plain_reloads_merge_only_while_none_has_started() {
    let mut rig = rig(MIB, Link::gate());
    rig.source().request_reload();
    rig.source().request_reload();
    assert_eq!(rig.source().core().reloads_queued(), 1);
    rig.engine.pump();
    assert_eq!(rig.source().core().channel, 2, "merged");
    let (again, mut asked) = (rig.source().clone(), false);
    rig.engine.at_handoff = Some(Box::new(move || {
        if !std::mem::replace(&mut asked, true) {
            again.request_reload();
        }
    }));
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!(
        rig.source().core().channel,
        4,
        "the one in progress, then the one asked"
    );
    assert_eq!(rig.source().core().reloads_queued(), 0);
    assert_eq!(rig.state(), State::Live);
}

/// A worker reading only through `read_at` recovers a failed stream by itself: its failed
/// miss asks for the same rate-limited retry `try_read`'s does, and still returns the error.
#[test]
fn read_at_alone_asks_a_failed_stream_to_retry() {
    let mut rig = rig(MIB, Link::gate());
    let mut r = rig.reader(ReaderKind::Bulk);
    rig.control.fault(0, Fault::Lost);
    assert!(r.try_read(1000, 480).is_pending());
    rig.drain();
    assert!(matches!(rig.state(), State::Failed(_)));
    let cancel = AtomicBool::new(false);
    let mut buf = [0; 480];
    let failed = r.read_at(1000, &mut buf, &cancel);
    assert!(matches!(failed, Err(ReadError::Failed(_))), "{failed:?}");
    rig.engine.pump();
    assert_eq!(rig.reopens(), 1, "the read asked for a retry");
    assert_eq!(rig.state(), State::Live);
    // Within the same second another failure retries nothing.
    rig.control.fault(rig.sent().len(), Fault::Lost);
    assert!(r.try_read(1000, 480).is_pending());
    rig.drain();
    assert!(matches!(
        r.read_at(1000, &mut buf, &cancel),
        Err(ReadError::Failed(_))
    ));
    rig.engine.pump();
    assert_eq!(rig.reopens(), 1, "at most once a second");
    rig.advance(RETRY_EVERY);
    assert!(r.read_at(1000, &mut buf, &cancel).is_err());
    rig.engine.pump();
    assert_eq!((rig.reopens(), rig.state()), (2, State::Live));
    let got = rig.read(&mut r, 1000, 480);
    assert!(got[..] == pattern(1000, 480)[..]);
}

/// V8, a close racing a reconnect: the engine has taken the retry and let go of the lock when
/// the Source closes. Nothing is opened and nothing is requested after the close.
#[test]
fn v8_a_close_at_the_reconnect_handoff_opens_nothing() {
    for reload in [false, true] {
        let mut rig = rig(MIB, Link::gate());
        let mut r = rig.reader(ReaderKind::Interactive);
        rig.control.fault(0, Fault::Lost);
        assert!(r.try_read(1000, 480).is_pending());
        rig.drain();
        assert!(matches!(r.try_read(1000, 480), Poll::Ready(Err(_))));
        if reload {
            rig.source().request_reload();
        }
        let n = rig.sent().len();
        let closing = Arc::new(Mutex::new(rig.source.take()));
        rig.engine.at_handoff = Some(Box::new(move || {
            drop(closing.lock().unwrap().take());
        }));
        assert!(matches!(rig.engine.pump(), Flow::Closed));
        assert_eq!(rig.reopens(), 0, "a connection opened after the close");
        assert_eq!(rig.sent().len(), n);
        assert_eq!(r.try_read(1000, 480), Poll::Ready(Err(ReadError::Closed)));
    }
}

/// A local file's `read_at` looks at its cancel flag between the reads that fill the buffer,
/// retries after an interrupted read included.
#[test]
fn a_local_read_at_is_cancelled_between_its_reads() {
    use std::io::{Error, ErrorKind};
    let cancel = AtomicBool::new(false);
    let mut calls = 0;
    let mut buf = [0u8; 100];
    // A short read, during which the cancel is set: the fill stops there.
    let got = fill(
        |b, _| {
            calls += 1;
            match calls {
                1 => {
                    cancel.store(true, Ordering::SeqCst);
                    b[..10].fill(7);
                    Ok(10)
                }
                _ => Ok(0),
            }
        },
        0,
        &mut buf,
        Some(&cancel),
    );
    assert_eq!((got, calls), (Err(ReadError::Cancelled), 1));
    // An interrupted read, then the cancel: no retry.
    cancel.store(false, Ordering::SeqCst);
    let mut calls = 0;
    let got = fill(
        |_, _| {
            calls += 1;
            if calls == 1 {
                cancel.store(true, Ordering::SeqCst);
                return Err(Error::from(ErrorKind::Interrupted));
            }
            Ok(0)
        },
        0,
        &mut buf,
        Some(&cancel),
    );
    assert_eq!((got, calls), (Err(ReadError::Cancelled), 1));
    // No flag (a frame's read), or a flag not set: interrupted reads are retried to the end.
    cancel.store(false, Ordering::SeqCst);
    for flag in [None, Some(&cancel)] {
        let mut calls = 0;
        let got = fill(
            |b, at| {
                calls += 1;
                match calls {
                    2 => Err(Error::from(ErrorKind::Interrupted)),
                    4 => Ok(0),
                    _ => {
                        b[..30].fill(at as u8);
                        Ok(30)
                    }
                }
            },
            5,
            &mut buf,
            flag,
        );
        assert_eq!((got, calls), (Ok(60), 4));
        assert!(buf[..30] == [5; 30] && buf[30..60] == [35; 30]);
    }
}

/// 06 §3, a wait depends on nothing the cache keeps: six readers each wait for 16 MiB, half
/// again what the cache holds, and nobody looks while it all arrives. Each waiter holds the
/// blocks that came for it, so every read completes, with its own bytes and without a block
/// fetched twice, while the cache stays at its bound.
#[test]
fn readers_waiting_for_more_than_the_cache_holds_all_complete() {
    let mut rig = rig(MODEL, sim(1, 1_000_000_000));
    let place = |i: u64| (i + 1) * GIB + 1000;
    let mut readers: Vec<(u64, Reader, bool)> = (0..6)
        .map(|i| (place(i), rig.reader(media()), false))
        .collect();
    // Nobody looks while everything asked for arrives: all of it is fetched, once.
    for (at, r, _) in readers.iter_mut() {
        assert!(r.try_read(*at, MAX_READ).is_pending());
    }
    settle(&mut rig);
    let span = (MAX_READ as u64 + 1000).div_ceil(BLOCK);
    assert_eq!(read_offsets(&rig).len() as u64, 6 * span);
    assert_eq!(
        rig.source().core().cache.len() as u64,
        CACHE_BYTES / BLOCK,
        "the cache dropped a third of it"
    );
    let mut order = Vec::new();
    // As frames do, the readers look again only when the waker fired.
    let mut woken = usize::MAX;
    for _ in 0..200_000 {
        let wakes = rig.wakes();
        for (i, (at, r, done)) in readers.iter_mut().enumerate() {
            if *done || wakes == woken {
                continue;
            }
            if let Poll::Ready(got) = r.try_read(*at, MAX_READ) {
                let got = got.expect("a read");
                assert_eq!(got.len(), MAX_READ);
                assert!(got[..] == pattern(*at, MAX_READ)[..], "reader {i}'s bytes");
                *done = true;
                order.push(i);
            }
        }
        woken = wakes;
        assert!(rig.source().core().cache.len() as u64 <= CACHE_BYTES / BLOCK);
        if order.len() == readers.len() {
            break;
        }
        rig.engine.pump();
        if rig.pending() > 0 {
            rig.engine.step(None);
        } else {
            // Nothing is out: a reader must have something to read.
            assert!(
                rig.wakes() != woken || wakes != woken,
                "stalled with {order:?} done"
            );
            woken = usize::MAX;
        }
    }
    assert_eq!(
        order,
        [0, 1, 2, 3, 4, 5],
        "every read completes, oldest first"
    );
    rig.drain();
    rig.check_admission();
    // No block was fetched twice: nothing a reader waited for was evicted under it.
    let offsets: Vec<u64> = rig
        .sent()
        .iter()
        .filter_map(|s| match s.asked {
            Asked::Read { off, .. } => Some(off),
            _ => None,
        })
        .collect();
    let distinct: std::collections::BTreeSet<u64> = offsets.iter().copied().collect();
    assert_eq!(distinct.len(), offsets.len(), "a block was fetched twice");
    let blocks = (MAX_READ as u64 + 1000).div_ceil(BLOCK);
    assert!(offsets.len() as u64 >= 6 * blocks);
}

/// `shares` adds in 128 bits: a window, or their sum, past 64 bits does not overflow.
#[test]
fn shares_do_not_overflow() {
    let huge = limits(ReaderKind::Media {
        bytes_per_s: u64::MAX,
    });
    assert_eq!(huge, (32 * MIB, u64::MAX));
    let one = shares(&[huge]);
    assert_eq!(
        one,
        [(FLOOR_AHEAD, PROTECTED_BYTES)],
        "all but nothing of it is behind"
    );
    let many = shares(&[(u64::MAX / 2, 0); 4]);
    assert_eq!(many, [(PROTECTED_BYTES / 4, 0); 4]);
    let max = shares(&[(u64::MAX, u64::MAX), (MIB, 0)]);
    assert_eq!(max[1], (MIB, 0));
    assert!(max[0].0 + max[0].1 <= PROTECTED_BYTES / 2 + FLOOR_AHEAD);
}

/// Replies that arrive at one timestamp (a batch handed over together) all count toward the
/// rate: their bytes are carried into the next interval that has a length.
#[test]
fn completions_at_one_timestamp_count_toward_the_rate() {
    let ms = Duration::from_millis;
    let t0 = Instant::now();
    let mut adm = Admission::default();
    // The first completion has no interval of its own, and two more share its instant.
    adm.delivered(1000, Duration::ZERO, t0);
    adm.delivered(1000, ms(5), t0);
    adm.delivered(1000, ms(5), t0);
    assert_eq!(adm.rate(), None, "no interval yet");
    adm.delivered(1000, ms(10), t0 + ms(10));
    assert_eq!(adm.rate(), Some(400_000.0), "4000 bytes in 10 ms");
    // Later ones too: three at one instant, then the interval.
    let mut one = adm.clone();
    for _ in 0..3 {
        adm.delivered(2000, ms(1), t0 + ms(10));
    }
    adm.delivered(2000, ms(10), t0 + ms(20));
    one.delivered(8000, ms(10), t0 + ms(20));
    assert_eq!(adm.rate(), one.rate(), "as one reply of their bytes");
}

/// `Rig::stream` holds every read to its length: a short result fails it.
#[test]
#[should_panic(expected = "a read of 480 bytes")]
fn the_rigs_stream_refuses_a_short_read() {
    let mut rig = rig(MIB, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    // The last read ends past the file: it comes back 100 bytes long.
    rig.stream_exact(&mut r, MIB - 580, 480, 2);
}

// ---- review round 2 ----

/// V8's boundary: a reconnect that saw the Source open and went to the network is an operation
/// under way. A close then does not wait for it; when it returns, what it opened is closed
/// unused — no read, no fence, no poll — and the engine ends. Both ways to the network: a
/// retry's new connection, and a reload's re-open on the connection it has.
#[test]
fn v8_a_close_after_the_reconnect_guard_closes_what_it_opened_unused() {
    for reload in [false, true] {
        let mut rig = rig(MIB, Link::gate());
        let mut r = rig.reader(ReaderKind::Interactive);
        if reload {
            rig.read(&mut r, 1000, 480);
            rig.drain();
            rig.source().request_reload();
        } else {
            rig.control.fault(0, Fault::Lost);
            assert!(r.try_read(1000, 480).is_pending());
            rig.drain();
            assert!(matches!(r.try_read(1000, 480), Poll::Ready(Err(_))));
        }
        let n = rig.sent().len();
        let closing = Arc::new(Mutex::new(rig.source.take()));
        let closed = Arc::new(AtomicBool::new(false));
        let returned = closed.clone();
        rig.engine.at_reopen = Some(Box::new(move || {
            // The last handle drops here, past the guard — and returns, the reconnect still
            // to come.
            drop(closing.lock().unwrap().take());
            returned.store(true, Ordering::SeqCst);
        }));
        assert!(matches!(rig.engine.pump(), Flow::Closed), "the engine ends");
        assert!(
            closed.load(Ordering::SeqCst),
            "the close returned inside the hook"
        );
        assert_eq!(
            rig.reopens(),
            if reload { 0 } else { 1 },
            "the open under way happened, and no other"
        );
        assert_eq!(rig.sent().len(), n, "nothing was requested on it");
        assert!(rig.engine.transport().is_none(), "and it was closed");
        assert_eq!(
            r.try_read(1000, 480),
            Poll::Ready(Err(ReadError::Closed)),
            "the Source stays closed (reload: {reload})"
        );
        assert!(matches!(rig.engine.pump(), Flow::Closed));
        assert_eq!(rig.sent().len(), n);
    }
}

/// Two callers blocked in `reload()`, the second asking while the first is in progress: each
/// gets its own result. The first finds the path gone and fails; the path is back for the
/// second, which succeeds.
#[test]
fn overlapping_blocking_reloads_each_get_their_own_result() {
    let mut rig = rig(MIB, Link::gate());
    rig.control.change(Change::Remove);
    let (first_tx, first) = mpsc::channel();
    let (second_tx, second) = mpsc::channel();
    let caller = rig.source().clone();
    let a = std::thread::spawn(move || {
        let _ = first_tx.send(caller.reload().map_err(|e| format!("{e:#}")));
    });
    eventually("the first reload is queued", || {
        rig.source().core().reloads_queued() == 1
    });
    let (watched, control) = (rig.source().clone(), rig.control.clone());
    let mut calls = 0;
    let mut second_tx = Some(second_tx);
    rig.engine.at_handoff = Some(Box::new(move || {
        calls += 1;
        if calls == 1 {
            // The first reload is in progress: the second caller asks now.
            let (caller, tx) = (watched.clone(), second_tx.take().unwrap());
            std::thread::spawn(move || {
                let _ = tx.send(caller.reload().map_err(|e| format!("{e:#}")));
            });
            eventually("the second reload is queued", || {
                watched.core().reloads_queued() == 1
            });
        } else {
            control.change(Change::Replace(FakeFile::bytes(vec![9; 5000])));
        }
    }));
    let (mut one, mut two) = (None, None);
    eventually("both reloads return", || {
        rig.engine.pump();
        one = one.take().or_else(|| first.try_recv().ok());
        two = two.take().or_else(|| second.try_recv().ok());
        one.is_some() && two.is_some()
    });
    a.join().unwrap();
    let failed = one.unwrap().unwrap_err();
    assert!(
        failed.contains("no such file"),
        "the first caller's own: {failed}"
    );
    assert_eq!(two, Some(Ok(())), "the second caller's own");
    assert_eq!((rig.state(), rig.source().size()), (State::Live, 5000));
}

/// An interrupt that lands between a blocked `read_at`'s cancel check and its wait is not
/// lost: the flag is looked at again under the lock the wait releases, and `interrupt`
/// notifies under that lock. The wait here would last 60 s, so ending on its timeout cannot
/// pass for having heard the interrupt.
#[test]
fn an_interrupt_between_the_cancel_check_and_the_wait_is_not_lost() {
    let rig = rig(MIB, Link::gate());
    let cancel = Arc::new(AtomicBool::new(false));
    let (flag, interrupter) = (cancel.clone(), rig.source().clone());
    {
        let mut probe = rig.source().probe();
        probe.wait = Some(Duration::from_secs(60));
        probe.seam = Some(Box::new(move || {
            flag.store(true, Ordering::SeqCst);
            interrupter.interrupt();
        }));
    }
    let mut r = rig.reader(ReaderKind::Bulk);
    let flag = cancel.clone();
    // The engine is never stepped: the block never comes.
    let got = bounded("the interrupted read", move || {
        r.read_at(0, &mut [0; 64], &flag)
    });
    assert_eq!(got, Err(ReadError::Cancelled));
    let probe = rig.source().probe();
    assert!(probe.seam.is_none(), "the seam ran");
    assert_eq!(
        probe.timed_out, 0,
        "it ended on a timeout: the interrupt was lost"
    );
}

/// 06 §4 and V4 under cache pressure: older Bulk waits are out, and a foreground miss that
/// comes after them is still the next thing sent — no Bulk read goes while it waits.
#[test]
fn a_foreground_miss_is_served_ahead_of_older_bulk_waits() {
    let mut rig = rig(MODEL, sim(20, 500_000));
    let place = |i: u64| (i + 1) * GIB;
    let mut passes: Vec<Reader> = (0..4).map(|_| rig.reader(ReaderKind::Bulk)).collect();
    for (i, pass) in passes.iter_mut().enumerate() {
        assert!(pass.try_read(place(i as u64), MAX_READ).is_pending());
    }
    rig.engine.pump();
    assert!(!rig.room(), "bulk reads fill the cap");
    let n = rig.trace().len();
    let mut viewer = rig.reader(ReaderKind::Interactive);
    let at = 20 * GIB + 1000;
    assert!(viewer.try_read(at, 480).is_pending());
    let got = rig.read(&mut viewer, at, 480);
    assert!(got[..] == pattern(at, 480)[..]);
    let trace = rig.trace();
    let first = trace[n..].iter().position(|t| t.class.is_some()).unwrap() + n;
    assert_eq!(
        (trace[first].class, trace[first].block),
        (Some(Class::Demand), Some(at / BLOCK)),
        "the next read sent is the foreground miss"
    );
    assert_eq!(rig.sent()[first + 1].asked, Asked::Fstat);
    // The passes go on behind it, each read to its end and none refetched.
    settle(&mut rig);
    for (i, pass) in passes.iter_mut().enumerate() {
        take_whole(pass, place(i as u64), 256, "a bulk pass");
    }
    assert_none_fetched_twice(&rig);
    rig.check_admission();
}

/// The first four completions of a short prefetch whose fence fails, with its rest already
/// sent: requests 0 the screen's block (short), 1 its fence, 2 block 3 as prefetch (short),
/// 3 its fence — which fails — 4 the screen's rest, 5 that fence, 6 block 3's rest, 7 the
/// rest's fence. Leaves completions 0–3 applied.
fn abandoned_remainder(rest: Option<Fault>) -> (Rig, Reader) {
    let mut rig = rig_with(FakeFile::pattern(MODEL), clipped(40_000), NEVER, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.control.fault(3, Fault::Status("Failure"));
    if let Some(fault) = rest {
        rig.control.fault(6, fault);
    }
    assert!(r.try_read(3 * BLOCK - 480, 480).is_pending());
    for _ in 0..4 {
        rig.one();
    }
    assert_eq!(rig.sent().len(), 8, "block 3's rest is out");
    assert_eq!(rig.state(), State::Live);
    (rig, r)
}

/// A reply of a fetch given up is dropped before it is read: while a reader waits for a new
/// fetch of the block, the old remainder coming back as an error does not fail the Source,
/// coming back empty does not mark it `Changed`, and coming back with bytes adds nothing to
/// the new fetch.
#[test]
fn a_stale_remainder_reply_is_dropped_before_it_is_read() {
    let at = 3 * BLOCK + 1000;
    for case in ["error", "empty", "bytes"] {
        let fault = (case == "error").then_some(Fault::Status("Failure"));
        let (mut rig, _r) = abandoned_remainder(fault);
        let mut other = rig.reader(ReaderKind::Interactive);
        assert!(other.try_read(at, 100).is_pending());
        rig.engine.pump();
        assert_eq!(
            rig.sent()[8].asked,
            read_of(3),
            "a new fetch, from its start"
        );
        if case == "empty" {
            // The old remainder answered with no bytes (fed directly: on the Fake only a
            // real cut gives that, and a real cut is a change).
            let old = rig.sent()[6];
            let now = rig.engine.transport().unwrap().now();
            rig.source().core().apply(Completion {
                ticket: old.ticket,
                asked: old.asked,
                reply: Ok(Reply::Data(bytes::Bytes::new())),
                took: Duration::from_millis(30),
                at: now,
            });
            assert_eq!(rig.state(), State::Live, "not Changed");
        }
        let got = rig.read(&mut other, at, 100);
        assert!(got[..] == pattern(at, 100)[..], "{case}");
        rig.drain();
        assert_eq!(rig.state(), State::Live, "{case}");
        let whole = rig.read(&mut other, 3 * BLOCK, BLOCK as usize);
        assert!(
            whole[..] == pattern(3 * BLOCK, BLOCK as usize)[..],
            "{case}"
        );
        let fetches = rig.sent().iter().filter(|s| s.asked == read_of(3)).count();
        assert_eq!(fetches, 2, "the one given up and the new one ({case})");
    }
}

/// The fence fails while the block's rest is queued and not sent: the rest is never sent.
#[test]
fn a_queued_remainder_of_a_failed_fence_is_never_sent() {
    let mut rig = rig_with(FakeFile::pattern(MODEL), clipped(40_000), NEVER, None);
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.control.fault(3, Fault::Status("Failure"));
    let at = 3 * BLOCK - 480;
    assert!(r.try_read(at, 480).is_pending());
    rig.engine.pump();
    assert_eq!(rig.sent().len(), 4);
    // The four replies with no turn of the engine between them: nothing more is sent.
    for i in 0..4 {
        // A wake left over from the read ends a wait early: until the reply.
        let c = (0..1000)
            .find_map(|_| rig.engine.transport().unwrap().next(None))
            .expect("a reply");
        rig.source().core().apply(c);
        if i == 2 {
            assert!(rig.source().core().pf.rest_queued(3), "the rest is queued");
        }
    }
    assert!(
        !rig.source().core().pf.rest_queued(3),
        "and dropped with its fetch"
    );
    let got = rig.read(&mut r, at, 480);
    assert!(got[..] == pattern(at, 480)[..]);
    rig.drain();
    let rest = Asked::Read {
        off: 3 * BLOCK + 40_000,
        len: (BLOCK - 40_000) as u32,
    };
    assert!(
        rig.sent().iter().all(|s| s.asked != rest),
        "the rest was sent"
    );
    assert!(!rig.source().core().cache.contains(3));
    assert_eq!(rig.source().core().held(), (0, 0));
    assert_eq!(rig.state(), State::Live);
}

// ---- review round 3: a waited block is held by its waiter ----

/// Steps the engine until nothing is out and nothing more goes, with no reader looking.
/// Bounded well above what any test here fetches, so a stream that fetches without end fails.
fn settle(rig: &mut Rig) {
    for _ in 0..40_000 {
        rig.engine.pump();
        if rig.pending() == 0 {
            return;
        }
        rig.engine.step(None);
    }
    panic!("the stream goes on fetching with no reader looking");
}

fn read_offsets(rig: &Rig) -> Vec<u64> {
    rig.sent()
        .iter()
        .filter_map(|s| match s.asked {
            Asked::Read { off, .. } => Some(off),
            _ => None,
        })
        .collect()
}

fn assert_none_fetched_twice(rig: &Rig) {
    let offsets = read_offsets(rig);
    let distinct: std::collections::BTreeSet<u64> = offsets.iter().copied().collect();
    assert_eq!(distinct.len(), offsets.len(), "a block was fetched twice");
}

/// `blocks` whole blocks at `at`, which must be there now, with the pattern's bytes.
fn take_whole(r: &mut Reader, at: u64, blocks: u64, who: &str) {
    let len = (blocks * BLOCK) as usize;
    match r.try_read(at, len) {
        Poll::Ready(Ok(got)) => {
            assert_eq!(got.len(), len, "{who}");
            assert!(got[..] == pattern(at, len)[..], "{who}'s bytes");
        }
        other => panic!("{who}: its blocks came and are not there to read: {other:?}"),
    }
}

/// A wait keeps the blocks that came for it, whatever the cache does since: three Bulk waits
/// of 256 blocks have their blocks in, unread, when two foreground waits of 256 blocks come.
/// The foreground demands go; the cache, past its bound, drops Bulk's blocks; every Bulk read
/// still completes, and nothing is fetched twice.
#[test]
fn bulk_waits_keep_their_blocks_when_foreground_waits_fill_the_cache() {
    let mut rig = rig(MODEL, sim(1, 1_000_000_000));
    let place = |i: u64| (i + 1) * GIB;
    let mut passes: Vec<Reader> = (0..3).map(|_| rig.reader(ReaderKind::Bulk)).collect();
    for (i, pass) in passes.iter_mut().enumerate() {
        assert!(pass.try_read(place(i as u64), MAX_READ).is_pending());
    }
    settle(&mut rig);
    assert_eq!(
        read_offsets(&rig).len(),
        3 * 256,
        "the passes' blocks are in"
    );
    let n = rig.trace().len();
    let mut viewers: Vec<Reader> = (0..2).map(|_| rig.reader(media())).collect();
    for (i, viewer) in viewers.iter_mut().enumerate() {
        assert!(viewer.try_read(place(10 + i as u64), MAX_READ).is_pending());
    }
    settle(&mut rig);
    let reads: Vec<Option<Class>> = rig.trace()[n..]
        .iter()
        .filter_map(|t| t.class.map(Some))
        .collect();
    assert_eq!(reads.len(), 2 * 256);
    assert!(
        reads.iter().all(|c| *c == Some(Class::Demand)),
        "the foreground demands went"
    );
    assert_eq!(
        rig.source().core().cache.len(),
        1024,
        "the cache is at its bound"
    );
    for (i, viewer) in viewers.iter_mut().enumerate() {
        take_whole(viewer, place(10 + i as u64), 256, "a viewer");
    }
    for (i, pass) in passes.iter_mut().enumerate() {
        take_whole(pass, place(i as u64), 256, "a bulk pass");
    }
    settle(&mut rig);
    assert_none_fetched_twice(&rig);
    assert_eq!(read_offsets(&rig).len(), 5 * 256);
    rig.check_admission();
}

/// Priority is the queue's class order alone: foreground waits of mixed sizes, more together
/// than the cache holds, and a small Bulk wait older than all of them. No Bulk read is sent
/// while any foreground demand is unsent, and every read completes without a refetch.
#[test]
fn no_bulk_read_goes_while_any_foreground_demand_is_unsent() {
    let mut rig = rig(MODEL, sim(1, 1_000_000_000));
    let place = |i: u64| (i + 1) * GIB;
    let mut pass = rig.reader(ReaderKind::Bulk);
    assert!(pass.try_read(place(20), BLOCK as usize).is_pending());
    let sizes = [256u64, 256, 256, 255, 100];
    let mut viewers: Vec<Reader> = sizes.iter().map(|_| rig.reader(media())).collect();
    for (i, viewer) in viewers.iter_mut().enumerate() {
        let len = (sizes[i] * BLOCK) as usize;
        assert!(viewer.try_read(place(i as u64), len).is_pending());
    }
    settle(&mut rig);
    let trace = rig.trace();
    let demands = trace
        .iter()
        .filter(|t| t.class == Some(Class::Demand))
        .count() as u64;
    assert_eq!(
        demands,
        sizes.iter().sum::<u64>(),
        "every foreground demand was sent"
    );
    let bulk = trace
        .iter()
        .position(|t| t.class == Some(Class::Bulk))
        .expect("the bulk read, in its turn");
    let last_demand = trace
        .iter()
        .rposition(|t| t.class == Some(Class::Demand))
        .unwrap();
    assert!(
        bulk > last_demand,
        "a bulk read went with foreground demand unsent"
    );
    for (i, viewer) in viewers.iter_mut().enumerate() {
        take_whole(viewer, place(i as u64), sizes[i], "a viewer");
    }
    take_whole(&mut pass, place(20), 1, "the bulk pass");
    settle(&mut rig);
    assert_none_fetched_twice(&rig);
    rig.check_admission();
}

/// A wait takes hold, when it registers, of the blocks of its span the cache has then: one
/// evicted afterwards — here by four older waits filling the cache first — is not fetched
/// again.
#[test]
fn a_wait_keeps_the_blocks_cached_when_it_registered() {
    let mut rig = rig(MODEL, sim(1, 1_000_000_000));
    let place = |i: u64| (i + 1) * GIB;
    // The last block of what will be waited for, cached by a reader long gone.
    let last = 15 * GIB + 255 * BLOCK;
    let mut gone = rig.reader(ReaderKind::Bulk);
    rig.read(&mut gone, last, BLOCK as usize);
    drop(gone);
    settle(&mut rig);
    let mut older: Vec<Reader> = (0..4).map(|_| rig.reader(media())).collect();
    for (i, r) in older.iter_mut().enumerate() {
        assert!(r.try_read(place(i as u64), MAX_READ).is_pending());
    }
    let mut waiter = rig.reader(media());
    assert!(waiter.try_read(15 * GIB, MAX_READ).is_pending());
    settle(&mut rig);
    assert!(
        !rig.source().core().cache.contains(last / BLOCK),
        "the older waits' blocks pushed it out of the cache"
    );
    take_whole(&mut waiter, 15 * GIB, 256, "the waiter");
    let fetched = read_offsets(&rig)
        .iter()
        .filter(|off| **off == last)
        .count();
    assert_eq!(fetched, 1, "the block it held was fetched again");
    for (i, r) in older.iter_mut().enumerate() {
        take_whole(r, place(i as u64), 256, "an older wait");
    }
    assert_none_fetched_twice(&rig);
}

/// The memory bound (06 §3): a reader holds blocks only for its one wait — at most one read's
/// worth — and lets go of them when the read completes, when it moves to another place, when
/// it is renewed or dropped, and when the Source changes.
#[test]
fn a_reader_holds_blocks_only_for_its_one_wait() {
    let mut rig = rig(MODEL, sim(1, 1_000_000_000));
    let holding = |rig: &Rig| rig.source().core().pf.holding();
    let mut r = rig.reader(media());
    assert!(r.try_read(GIB, MAX_READ).is_pending());
    settle(&mut rig);
    assert_eq!(holding(&rig), 256, "what came for its wait");
    // A maximum read that starts inside a block touches 257: 16 MiB and one block.
    assert!(r.try_read(3 * GIB + 1000, MAX_READ).is_pending());
    settle(&mut rig);
    assert_eq!(holding(&rig), 257);
    assert_eq!(
        257 * BLOCK,
        MAX_READ as u64 + BLOCK,
        "the most one waiter holds"
    );
    // Moved: what it held of the old place goes.
    assert!(r.try_read(5 * GIB, 100 * BLOCK as usize).is_pending());
    assert_eq!(holding(&rig), 0);
    settle(&mut rig);
    assert_eq!(holding(&rig), 100);
    // Read: nothing is held.
    take_whole(&mut r, 5 * GIB, 100, "the reader");
    assert_eq!(holding(&rig), 0);
    // Renewed, and dropped.
    assert!(r.try_read(7 * GIB, MAX_READ).is_pending());
    let mut other = rig.reader(media());
    assert!(other.try_read(7 * GIB, MAX_READ).is_pending());
    settle(&mut rig);
    assert_eq!(holding(&rig), 512, "each waiter its own references");
    r.renew();
    assert_eq!(holding(&rig), 256);
    drop(other);
    assert_eq!(holding(&rig), 0);
    // A change: nothing held is served, or kept.
    assert!(r.try_read(9 * GIB, 300 * BLOCK as usize / 2).is_pending());
    settle(&mut rig);
    assert_eq!(holding(&rig), 150);
    rig.control.change(write(0, false));
    let mut miss = rig.reader(ReaderKind::Interactive);
    assert!(miss.try_read(11 * GIB + 1000, 480).is_pending());
    settle(&mut rig);
    assert_eq!(rig.state(), State::Changed);
    assert_eq!(holding(&rig), 0);
    assert_eq!(
        r.try_read(9 * GIB, 300 * BLOCK as usize / 2),
        Poll::Ready(Err(ReadError::Changed))
    );
}

/// A read that is answered with nothing — at or past the end of the file, or of no length —
/// ends the reader's previous wait as a move does: what it held is let go, and its unsent
/// demand is not sent.
#[test]
fn an_empty_read_retires_the_wait_before_it() {
    let holding = |rig: &Rig| rig.source().core().pf.holding();
    let cancel = AtomicBool::new(false);
    for case in ["at the end", "of no length", "into an empty buffer"] {
        // Whether the read came back with nothing, and no error.
        let empty = |r: &mut Reader| match case {
            "at the end" => matches!(r.try_read(MODEL, 100), Poll::Ready(Ok(b)) if b.is_empty()),
            "of no length" => matches!(r.try_read(9 * GIB, 0), Poll::Ready(Ok(b)) if b.is_empty()),
            // The blocking read of workers and HTTP connections.
            _ => r.read_at(9 * GIB, &mut [], &cancel) == Ok(0),
        };
        // Its blocks arrived, unread.
        let mut rig = rig(MODEL, sim(1, 1_000_000_000));
        let mut r = rig.reader(media());
        assert!(r.try_read(GIB, MAX_READ).is_pending());
        settle(&mut rig);
        assert_eq!(holding(&rig), 256);
        assert!(empty(&mut r), "{case}");
        assert_eq!(holding(&rig), 0, "an empty read {case} let go of nothing");
        assert_eq!(r.waiting(), None);
        // Most of its demand unsent, on a slow link.
        let mut rig = rig_with(FakeFile::pattern(MODEL), sim(20, 500_000), NEVER, None);
        let mut r = rig.reader(media());
        assert!(r.try_read(GIB, MAX_READ).is_pending());
        rig.engine.pump();
        let sent = read_offsets(&rig).len();
        assert!((1..16).contains(&sent), "the cap let a few go: {sent}");
        assert!(empty(&mut r), "{case}");
        settle(&mut rig);
        assert_eq!(
            read_offsets(&rig).len(),
            sent,
            "an obsolete read was sent after an empty read {case}"
        );
        assert_eq!(holding(&rig), 0);
    }
}

/// A cancelled `read_at` ends its wait too: a Reader kept after the cancel holds nothing,
/// what was in flight for it arrives and fills nothing, and nothing more of it is sent. Both
/// places the flag is looked at: before a look, and under the lock before the wait.
#[test]
fn a_cancelled_read_at_retires_its_wait() {
    let holding = |rig: &Rig| rig.source().core().pf.holding();
    for at_the_wait in [false, true] {
        let mut rig = rig_with(FakeFile::pattern(MODEL), sim(20, 500_000), NEVER, None);
        let mut r = rig.reader(media());
        assert!(r.try_read(GIB, MAX_READ).is_pending());
        rig.engine.pump();
        let sent = read_offsets(&rig).len();
        assert!((1..16).contains(&sent), "the cap let a few go: {sent}");
        let cancel = Arc::new(AtomicBool::new(!at_the_wait));
        if at_the_wait {
            let flag = cancel.clone();
            rig.source().probe().seam = Some(Box::new(move || flag.store(true, Ordering::SeqCst)));
        }
        let mut buf = vec![0; MAX_READ];
        assert_eq!(r.read_at(GIB, &mut buf, &cancel), Err(ReadError::Cancelled));
        assert_eq!(
            r.waiting(),
            None,
            "the wait outlived the cancel ({at_the_wait})"
        );
        // What was in flight arrives: it fills nothing, and nothing more is sent.
        settle(&mut rig);
        assert_eq!(
            holding(&rig),
            0,
            "the cancelled wait was filled ({at_the_wait})"
        );
        assert_eq!(read_offsets(&rig).len(), sent, "an obsolete read was sent");
    }
}

/// What a reader holds does not outlive a failure or a reload, with the cache long past the
/// blocks it held: none is kept, the reader gets the state's own error, and after the stream
/// is back (and the reader renewed) it reads the bytes that are there now.
#[test]
fn held_blocks_do_not_outlive_a_failure_or_a_reload() {
    let mut rig = rig(MODEL, sim(1, 1_000_000_000));
    let holding = |rig: &Rig| rig.source().core().pf.holding();
    let whole = MAX_READ;
    // `r` waits at `at`, its blocks arrive unread, and another reader's reading pushes them
    // out of the cache.
    fn hold_and_evict(rig: &mut Rig, r: &mut Reader, at: u64, far: u64) {
        assert!(r.try_read(at, MAX_READ).is_pending());
        settle(rig);
        let mut other = rig.reader(ReaderKind::Interactive);
        rig.stream(&mut other, far, far + 1100 * BLOCK, BLOCK as usize);
        drop(other);
        settle(rig);
        assert_eq!(rig.source().core().pf.holding(), 256);
        assert!(
            !rig.source().core().cache.contains(at / BLOCK + 100),
            "what it holds is no longer in the cache"
        );
    }
    let mut r = rig.reader(ReaderKind::Bulk);

    // Failed.
    hold_and_evict(&mut rig, &mut r, GIB, 2 * GIB);
    rig.control.fault(rig.sent().len(), Fault::Lost);
    let mut miss = rig.reader(ReaderKind::Interactive);
    assert!(miss.try_read(3 * GIB + 1000, 480).is_pending());
    rig.drain();
    assert!(matches!(rig.state(), State::Failed(_)));
    assert_eq!(holding(&rig), 0, "kept through a failure");
    assert!(matches!(
        r.try_read(GIB, whole),
        Poll::Ready(Err(ReadError::Failed(_)))
    ));
    rig.engine.pump();
    assert_eq!(rig.state(), State::Live, "the read asked for the retry");
    let got = rig.read(&mut r, GIB, whole);
    assert!(got[..] == pattern(GIB, whole)[..]);
    assert_eq!(holding(&rig), 0);

    // A reload that succeeds, on a file written since.
    hold_and_evict(&mut rig, &mut r, 4 * GIB, 5 * GIB);
    rig.control.change(write(4 * GIB, false));
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!(rig.state(), State::Live);
    assert_eq!(holding(&rig), 0, "kept through a reload");
    assert_eq!(
        r.try_read(4 * GIB, whole),
        Poll::Ready(Err(ReadError::Reloaded))
    );
    r.renew();
    let got = rig.read(&mut r, 4 * GIB, whole);
    assert!(got[..4096] == [0xEE; 4096], "the file as it is now");
    assert!(got[4096..] == pattern(4 * GIB + 4096, whole - 4096)[..]);

    // A reload that fails.
    hold_and_evict(&mut rig, &mut r, 6 * GIB, 7 * GIB);
    let now = rig.control.file().unwrap();
    rig.control.change(Change::Remove);
    rig.source().request_reload();
    rig.engine.pump();
    assert!(matches!(rig.state(), State::Failed(_)));
    assert_eq!(holding(&rig), 0, "kept through a failed reload");
    assert_eq!(
        r.try_read(6 * GIB, whole),
        Poll::Ready(Err(ReadError::Reloaded))
    );
    rig.control.change(Change::Replace(now));
    rig.source().request_reload();
    rig.engine.pump();
    assert_eq!(rig.state(), State::Live);
    r.renew();
    let got = rig.read(&mut r, 6 * GIB, whole);
    assert!(got[..] == pattern(6 * GIB, whole)[..]);
    assert_eq!(holding(&rig), 0);
}

// ---- the cache under a Source ----

/// 06 §3: a pass over the file stays within 64 MiB and does not evict what the screen needs —
/// a Bulk reader's blocks go in at the cold end.
#[test]
fn a_bulk_pass_stays_within_the_cache_and_keeps_the_screen() {
    let mut rig = rig(MODEL, sim(1, 1_000_000_000));
    let mut viewer = rig.reader(ReaderKind::Interactive);
    let screen = 20 * GIB + 1000;
    rig.read(&mut viewer, screen, 480);
    // A block no reader's window covers any more: only its recent use keeps it.
    let seen = 5 * GIB + 1000;
    let mut gone = rig.reader(ReaderKind::Interactive);
    rig.read(&mut gone, seen, 480);
    drop(gone);
    rig.drain();
    let mut bulk = rig.reader(ReaderKind::Bulk);
    let mut most = 0;
    for mib in 0..80 {
        rig.stream(&mut bulk, mib * MIB, (mib + 1) * MIB, BLOCK as usize);
        most = most.max(rig.source().core().cache.bytes());
    }
    assert_eq!(most, CACHE_BYTES, "the bound, reached and never passed");
    rig.drain();
    let n = rig.sent().len();
    assert!(
        viewer.try_read(screen, 480).is_ready(),
        "the screen's block stayed"
    );
    let mut back = rig.reader(ReaderKind::Interactive);
    assert!(
        back.try_read(seen, 480).is_ready(),
        "the pass's blocks went before a block the viewer had used"
    );
    rig.engine.pump();
    assert_eq!(rig.sent().len(), n);
    let f = rig.source().fetched();
    assert!(f.bulk >= 80 * MIB && f.bulk <= 81 * MIB + BLOCK, "{f:?}");
    assert_eq!(rig.source().stats().delivered, 80 * MIB + 2 * 480);
    rig.check_admission();
}

// ---- the stats events and the receipt hooks (06 §11) ----

#[test]
fn the_stats_lines() {
    let mut rig = rig(MODEL, Link::gate());
    let mut r = rig.reader(ReaderKind::Interactive);
    rig.read(&mut r, 0, 480);
    rig.source().mark(Mark::First);
    rig.run_for(TWO_S - Duration::from_millis(1));
    assert_eq!(rig.lines.lock().unwrap().len(), 2, "open, first");
    rig.run_for(Duration::from_secs(10));
    assert_eq!(rig.lines.lock().unwrap().len(), 3, "idle, once");
    rig.source().mark(Mark::Input(0x5_0000_0000));
    rig.read(&mut r, 0x5_0000_0000 + 1000, 480);
    rig.source().mark(Mark::Ready);
    rig.source().sent_http(0, 1000);
    rig.source().sent_http(500, 1000);
    rig.source().sent_http(5000, 10);
    rig.source().cut();
    rig.drain();
    let stats = rig.source().stats();
    assert_eq!((stats.http_sent, stats.cuts, stats.sniff), (1510, 1, 8192));
    drop(rig.source.take());
    let lines = rig.lines.lock().unwrap().clone();
    let events: Vec<&str> = lines.iter().map(|l| l.split(' ').nth(1).unwrap()).collect();
    assert_eq!(events, ["open", "first", "idle", "input", "ready", "close"]);
    let fields: Vec<&str> = lines[5]
        .split(' ')
        .map(|f| f.split('=').next().unwrap())
        .collect();
    let names = [
        "stream",
        "close",
        "t",
        "host:/m.bin",
        "fetched",
        "demand",
        "prefetch",
        "bulk",
        "delivered",
        "http_sent",
        "sniff",
        "requests",
        "max_in_flight",
        "runs",
        "jumps",
        "cuts",
    ];
    assert_eq!(fields, names);
    assert!(lines[0].ends_with(
        " host:/m.bin fetched=0 demand=0 prefetch=0 bulk=0 delivered=0 http_sent=0 sniff=8192 \
         requests=0 max_in_flight=0 runs=0 jumps=0 cuts=0"
    ));
    assert!(lines[1].contains(" fetched=65536 demand=65536 prefetch=0 bulk=0 delivered=480 "));
    assert!(lines[3].ends_with(" place=0x500000000"), "{}", lines[3]);
    assert!(
        lines[5].ends_with(
            " fetched=131072 demand=131072 prefetch=0 bulk=0 delivered=960 http_sent=1510 \
             sniff=8192 requests=4 max_in_flight=65636 runs=0 jumps=1 cuts=1"
        ),
        "{}",
        lines[5]
    );
}

#[test]
fn an_input_before_two_seconds_means_no_idle_line() {
    let mut rig = rig(MODEL, Link::gate());
    rig.source().mark(Mark::First);
    rig.run_for(Duration::from_secs(1));
    rig.source().mark(Mark::Input(64));
    rig.run_for(Duration::from_secs(5));
    let lines = rig.lines.lock().unwrap();
    assert_eq!(lines.len(), 3, "open, first, input: {lines:?}");
}

/// `SOPEN_STREAM_THROTTLE=<ms>,<bytes/s>`: every read delayed and paced, order kept.
#[test]
fn the_throttle_delays_and_paces_every_read() {
    assert_eq!(
        Throttle::parse("100,64000"),
        Some(Throttle {
            delay: Duration::from_millis(100),
            rate: 64_000
        })
    );
    for bad in ["", "100", "x,1", "100,0", "100,-5"] {
        assert_eq!(Throttle::parse(bad), None, "{bad}");
    }
    let throttle = Throttle::parse("100,64000");
    let mut rig = rig_with(FakeFile::pattern(MIB), sim(0, u64::MAX), NEVER, throttle);
    let mut r = rig.reader(ReaderKind::Interactive);
    let got = rig.read(&mut r, 0, 480);
    assert!(got[..] == pattern(0, 480)[..]);
    let block = Duration::from_millis(100) + Duration::from_secs_f64(65_536.0 / 64_000.0);
    assert_eq!(rig.control.elapsed(), block, "the delay, then the pace");
    // A second block starts its own delay.
    rig.read(&mut r, 3 * BLOCK + 1000, 480);
    assert_eq!(rig.control.elapsed(), block * 2);
    assert_eq!(rig.source().fetched().total, 2 * BLOCK);
    // A reload keeps the throttle.
    rig.source().request_reload();
    rig.engine.pump();
    r.renew();
    rig.read(&mut r, 0, 480);
    assert_eq!(rig.control.elapsed(), block * 3);
}

/// 06 §5: what a reader waits for, and for how long — the viewer's `loading 0x… (N KiB)` after
/// 1 s and `the remote is slow` after 10 s.
#[test]
fn a_waiting_reader_says_what_it_waits_for_and_how_long() {
    let mut rig = rig(MODEL, sim(3000, 5_000_000));
    let mut r = rig.reader(ReaderKind::Interactive);
    assert_eq!(r.waiting(), None);
    let at = 0x5_0000_0000 + 1000;
    assert!(r.try_read(at, 480).is_pending());
    rig.run_for(Duration::from_millis(1500));
    assert!(r.try_read(at, 480).is_pending());
    assert_eq!(
        r.waiting(),
        Some(Waiting {
            off: 0x5_0000_0000,
            len: BLOCK,
            waited: Duration::from_millis(1500)
        })
    );
    rig.read(&mut r, at, 480);
    assert_eq!(r.waiting(), None);
}

/// A hidden viewer does not poll (06 §5).
#[test]
fn a_hidden_viewer_does_not_poll() {
    let mut rig = rig_with(FakeFile::pattern(MIB), Link::gate(), TWO_S, None);
    rig.source().set_visible(false);
    rig.run_for(Duration::from_secs(9));
    assert_eq!(rig.sent().len(), 0);
    rig.source().set_visible(true);
    rig.run_for(Duration::from_secs(5));
    let asked: Vec<Asked> = rig.sent().iter().map(|s| s.asked).collect();
    assert_eq!(asked[..2], [Asked::Fstat, Asked::StatPath]);
    assert_eq!(asked.len(), 6, "one pair every 2 s: {asked:?}");
    assert_eq!(STAT_REPLY, STATUS_BYTES);
    rig.check_admission();
}

// ---- the engine's thread ----

/// Workers read through `read_at`: it blocks until the bytes are there, ends short at the end
/// of the file, and a local `File` reads the same way through `Bytes`.
#[test]
fn read_at_blocks_until_the_bytes_come() {
    bounded("read_at_blocks_until_the_bytes_come", || {
        let (source, _control) =
            threaded(FakeFile::pattern(3 * MIB + 5), Link::instant(), no_reopen());
        let cancel = AtomicBool::new(false);
        let mut r = source.reader(ReaderKind::Bulk);
        let mut buf = vec![0; 2 * MIB as usize + 300];
        assert_eq!(r.read_at(1000, &mut buf, &cancel), Ok(buf.len()));
        assert!(buf[..] == pattern(1000, buf.len())[..]);
        let mut tail = vec![0; 1000];
        assert_eq!(r.read_at(3 * MIB - 10, &mut tail, &cancel), Ok(15));
        assert!(tail[..15] == pattern(3 * MIB - 10, 15)[..]);
        assert_eq!(r.read_at(3 * MIB + 5, &mut tail, &cancel), Ok(0));
        assert_eq!(r.read_at(u64::MAX - 5, &mut tail, &cancel), Ok(0));
        assert!(source.fetched().bulk <= 3 * MIB + 5, "no block twice");
        assert_eq!(source.fetched().demand, 0);
        // `Bytes`, for a stream and for a local file.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("local.bin");
        std::fs::write(&path, pattern(0, 100_000)).unwrap();
        let mut local = std::fs::File::open(&path).unwrap();
        let mut both: [(&mut dyn Bytes, u64); 2] = [(&mut r, 3 * MIB + 5), (&mut local, 100_000)];
        for (bytes, size) in both.iter_mut() {
            assert_eq!((bytes.len(), bytes.is_empty()), (*size, false));
            let mut got = [0; 64];
            assert_eq!(bytes.read_at(70_000, &mut got, &cancel), Ok(64));
            assert!(got[..] == pattern(70_000, 64)[..]);
            match bytes.try_read(70_000, 64) {
                Poll::Ready(Ok(b)) => assert!(b[..] == got[..]),
                other => panic!("{other:?}"),
            }
            assert_eq!(bytes.read_at(*size - 3, &mut got, &cancel), Ok(3));
            cancel.store(true, Ordering::SeqCst);
            assert_eq!(
                bytes.read_at(0, &mut got, &cancel),
                Err(ReadError::Cancelled)
            );
            cancel.store(false, Ordering::SeqCst);
        }
        assert!(matches!(
            r.try_read(0, MAX_READ + 1),
            Poll::Ready(Err(ReadError::Failed(_)))
        ));
    });
}

/// A blocked `read_at` returns once its cancel flag is set (06 §9's shutdown). The cancel
/// comes only after the read is seen waiting — its block asked for on a link that never
/// answers — and it returns with nothing read.
#[test]
fn a_blocked_read_at_is_cancelled() {
    bounded("a_blocked_read_at_is_cancelled", || {
        let slow = Link {
            latency: Duration::from_secs(100_000),
            clock: Clock::Real,
            ..Link::gate()
        };
        let (source, control) = threaded(FakeFile::pattern(MIB), slow, no_reopen());
        let cancel = Arc::new(AtomicBool::new(false));
        let (flag, mut r) = (cancel.clone(), source.reader(media()));
        let worker = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let got = r.read_at(0, &mut buf, &flag);
            (got, buf)
        });
        eventually("the read waits for its block", || {
            source.core().pf.waiting_readers() == 1 && control.sent().len() >= 2
        });
        assert!(!worker.is_finished(), "it returned with nothing to return");
        cancel.store(true, Ordering::SeqCst);
        source.interrupt();
        let (got, buf) = worker.join().unwrap();
        assert_eq!(got, Err(ReadError::Cancelled));
        assert_eq!(buf, [0; 64], "no bytes came");
        assert_eq!(source.fetched().total, 0);
    });
}

/// V7: after one of sopen's own saves no change is reported — the reload finds the save's
/// baseline and is `Live` on the saved file, its cache dropped. A file that is not the one
/// saved is `Changed`. `reload` waits for the new file.
#[test]
fn v7_after_sopens_own_save_no_change_is_reported() {
    bounded("v7_after_sopens_own_save_no_change_is_reported", || {
        let (source, control) = threaded(FakeFile::pattern(MIB), Link::instant(), no_reopen());
        let cancel = AtomicBool::new(false);
        let mut r = source.reader(ReaderKind::Interactive);
        let mut got = [0; 32];
        assert_eq!(r.read_at(1000, &mut got, &cancel), Ok(32));
        assert!(got[..] == pattern(1000, 32)[..]);
        // The save renamed its staged temp over the target.
        let saved = FakeFile::bytes(vec![7; 300_000]);
        let baseline = saved.stat().fingerprint();
        control.change(Change::Replace(saved));
        source.reload_saved(baseline).unwrap();
        assert_eq!((source.state(), source.size()), (State::Live, 300_000));
        assert_eq!(source.opened().mtime, baseline.mtime);
        assert_eq!(
            r.read_at(1000, &mut got, &cancel),
            Err(ReadError::Reloaded),
            "a reader of the file before the save"
        );
        r.renew();
        assert_eq!(r.read_at(1000, &mut got, &cancel), Ok(32));
        assert!(got[..] == [7; 32], "the saved file, not the old cache");
        assert_eq!(r.read_at(299_990, &mut got, &cancel), Ok(10));
        // A file that is not the one saved: `Changed`.
        let other = Fingerprint {
            size: 300_001,
            ..baseline
        };
        source.reload_saved(other).unwrap();
        assert_eq!(source.state(), State::Changed);
        source.reload().unwrap();
        assert_eq!(source.state(), State::Live);
        // The path gone: the reload says why, and the Source is Failed.
        control.change(Change::Remove);
        let gone = source.reload().unwrap_err();
        assert!(format!("{gone:#}").contains("no such file"), "{gone:#}");
        assert!(matches!(source.state(), State::Failed(_)));
    });
}

/// A transport that is gone with nothing out on it — here a channel lost at a prefetch nobody
/// waits for, which is not shown — is noticed without a read: the engine's wait ends at once
/// and unasked, so it asks the transport something, hears the error and fails the Source
/// instead of spinning. The next read retries.
#[test]
fn a_dead_idle_transport_is_noticed_without_a_read() {
    bounded("a_dead_idle_transport_is_noticed_without_a_read", || {
        let (transport, control) = fake("/m.bin", FakeFile::pattern(MIB), Link::instant());
        let again = control.clone();
        // Requests: 0 the screen's block, 1 its fence, 2 the margin's block (prefetch).
        control.fault(2, Fault::Lost);
        let source =
            Source::open(transport, Box::new(move || again.open()), Arc::new(|| {})).unwrap();
        let cancel = AtomicBool::new(false);
        let mut r = source.reader(ReaderKind::Interactive);
        let mut got = [0; 480];
        assert_eq!(r.read_at(3 * BLOCK - 480, &mut got, &cancel), Ok(480));
        let started = Instant::now();
        while source.state() == State::Live {
            assert!(
                started.elapsed() < HANG,
                "the dead transport was never noticed"
            );
            std::thread::yield_now();
        }
        let State::Failed(reason) = source.state() else {
            panic!("{:?}", source.state())
        };
        assert!(reason.contains("the connection closed"), "{reason}");
        let asked: Vec<Asked> = control.sent().iter().map(|s| s.asked).collect();
        assert_eq!(
            asked[..4],
            [read_of(2), Asked::Fstat, read_of(3), Asked::Fstat]
        );
        assert!(
            asked[4..].iter().all(|a| !matches!(a, Asked::Read { .. })),
            "it asked the transport, it read nothing: {asked:?}"
        );
        // The screen stays, and a read of anything else retries on a new channel.
        assert_eq!(r.read_at(3 * BLOCK - 480, &mut got, &cancel), Ok(480));
        loop {
            match r.try_read(9 * BLOCK + 1000, 32) {
                Poll::Ready(Ok(b)) => {
                    assert!(b[..] == pattern(9 * BLOCK + 1000, 32)[..]);
                    break;
                }
                _ => assert!(started.elapsed() < HANG, "the retry never came"),
            }
            std::thread::yield_now();
        }
        assert_eq!(source.state(), State::Live);
    });
}

// ---- the real server ----

fn local_file(len: usize) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
    let tmp = tempfile::tempdir().unwrap();
    let body = pattern(0, len);
    let path = tmp.path().join("stream.bin");
    std::fs::write(&path, &body).unwrap();
    (tmp, path, body)
}

/// The proxy type of `SftpRemote::local_server_with`, which this module cannot name.
fn proxy_of<P: Default>(_: fn(Option<P>) -> Option<SftpRemote>) -> P {
    P::default()
}

/// A Source over `fs`, stepped by hand, polling every `poll_every`.
fn over_sftp(fs: SftpRemote, path: &std::path::Path, poll_every: Duration) -> (Source, Engine) {
    let transport = Box::new(fs).into_ranged(path.to_str().unwrap()).unwrap();
    let settings = Settings {
        poll_every,
        ..Settings::plain()
    };
    let opening = Opening {
        label: "local".into(),
        began: Instant::now(),
        sniff: 0,
    };
    Source::build(transport, no_reopen(), Arc::new(|| {}), opening, settings)
}

/// Steps the engine until `done`; the bound only catches a hang.
fn until(engine: &mut Engine, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < HANG, "the stream hangs");
        engine.step(Some(Duration::from_millis(200)));
    }
}

/// V5 through the clipping proxy, at the Source: every `SSH_FXP_DATA` cut to 20 000 bytes, and
/// a pass over a 4 MiB file gives bytes whose SHA-256 is the file's.
#[test]
fn v5_a_source_through_the_clipping_proxy_reads_the_files_bytes() {
    bounded(
        "v5_a_source_through_the_clipping_proxy_reads_the_files_bytes",
        || {
            if SftpRemote::local_server().is_none() {
                return;
            }
            let (_tmp, path, body) = local_file(4 << 20);
            let mut proxy = proxy_of(SftpRemote::local_server_with);
            proxy.clip = Some(20_000);
            let fs = SftpRemote::local_server_with(Some(proxy)).unwrap();
            let transport = Box::new(fs).into_ranged(path.to_str().unwrap()).unwrap();
            let source = Source::open(transport, no_reopen(), Arc::new(|| {})).unwrap();
            let mut r = source.reader(ReaderKind::Bulk);
            let cancel = AtomicBool::new(false);
            let mut hash = Sha256::new();
            let mut buf = vec![0; 300_000];
            let mut off = 0;
            loop {
                let n = r.read_at(off, &mut buf, &cancel).unwrap();
                hash.update(&buf[..n]);
                off += n as u64;
                if n < buf.len() {
                    break;
                }
            }
            assert_eq!(off, body.len() as u64);
            assert!(hash.finalize()[..] == Sha256::digest(&body)[..]);
            assert_eq!(source.state(), State::Live);
        },
    );
}

/// V5 on the real server: a truncate between the two halves of a block. The block is not
/// used and the Source is `Changed`.
#[test]
fn v5_a_truncate_between_the_halves_of_a_block_is_a_change_on_the_local_server() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, path, _) = local_file(1 << 20);
    let mut proxy = proxy_of(SftpRemote::local_server_with);
    proxy.clip = Some(20_000);
    let fs = SftpRemote::local_server_with(Some(proxy)).unwrap();
    let (source, mut engine) = over_sftp(fs, &path, NEVER);
    let mut r = source.reader(ReaderKind::Interactive);
    assert!(r.try_read(0, 480).is_pending());
    // The first half: a short reply. Its rest is not sent before the next turn.
    until(&mut engine, || source.core().pf.rest_queued(0));
    assert_eq!(source.state(), State::Live);
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(10_000).unwrap();
    until(&mut engine, || source.state() != State::Live);
    assert_eq!(source.state(), State::Changed);
    assert_eq!(r.try_read(0, 480), Poll::Ready(Err(ReadError::Changed)));
    assert!(source.core().cache.is_empty());
}

/// V7 on the real server: a write in place after a block was read. The next block's fence
/// sees the mtime move, and nothing of the new version is shown.
#[test]
fn v7_a_write_in_place_is_a_change_on_the_local_server() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (_tmp, path, body) = local_file(1 << 20);
    let fs = SftpRemote::local_server().unwrap();
    let (source, mut engine) = over_sftp(fs, &path, NEVER);
    let mut r = source.reader(ReaderKind::Interactive);
    assert!(r.try_read(1000, 480).is_pending());
    until(&mut engine, || r.try_read(1000, 480).is_ready());
    assert!(matches!(r.try_read(1000, 480), Poll::Ready(Ok(b)) if b[..] == body[1000..1480]));
    // Same size, another whole second: SFTP carries no more than that.
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all_at(&[0xEE; 4096], 5 * BLOCK + 1000).unwrap();
    let opened = source.opened().mtime.duration_since(UNIX_EPOCH).unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(opened.as_secs() + 10))
        .unwrap();
    assert!(r.try_read(5 * BLOCK + 1000, 480).is_pending());
    until(&mut engine, || source.state() != State::Live);
    assert_eq!(source.state(), State::Changed);
    assert!(source.core().cache.is_empty());
    assert_eq!(
        r.try_read(5 * BLOCK + 1000, 480),
        Poll::Ready(Err(ReadError::Changed))
    );
}

/// V7 on the real server: another file renamed over the path. The poll finds the name and the
/// handle disagreeing; the handle still reads the version opened.
#[test]
fn v7_a_rename_over_is_replaced_on_the_local_server() {
    if SftpRemote::local_server().is_none() {
        return;
    }
    let (tmp, path, body) = local_file(1 << 20);
    let fs = SftpRemote::local_server().unwrap();
    let (source, mut engine) = over_sftp(fs, &path, Duration::from_millis(20));
    let mut r = source.reader(ReaderKind::Interactive);
    assert!(r.try_read(0, 480).is_pending());
    until(&mut engine, || r.try_read(0, 480).is_ready());
    let other = tmp.path().join("other.bin");
    std::fs::write(&other, b"new").unwrap();
    std::fs::rename(&other, &path).unwrap();
    until(&mut engine, || source.state() != State::Live);
    assert_eq!(source.state(), State::Replaced);
    let at = 9 * BLOCK + 1000;
    assert!(r.try_read(at, 480).is_pending());
    until(&mut engine, || r.try_read(at, 480).is_ready());
    let from = at as usize;
    assert!(matches!(r.try_read(at, 480), Poll::Ready(Ok(b)) if b[..] == body[from..from + 480]));
    assert_eq!(source.state(), State::Replaced);
}

/// What the viewer is getting from the remote (07 §7's expected time, §4.1's cost): the
/// admission's own measure, `None` until a read has come back.
#[test]
fn the_rate_is_the_admissions_measure() {
    let mut rig = rig(MODEL, Link::gate());
    assert_eq!(rig.source().rate(), None, "nothing came yet");
    let mut bulk = rig.reader(ReaderKind::Bulk);
    for mib in 0..4 {
        rig.stream(&mut bulk, mib * MIB, (mib + 1) * MIB, BLOCK as usize);
    }
    let rate = rig.source().rate().expect("measured");
    assert_eq!(Some(rate), rig.source().core().pf.adm.rate());
    // The gate link: 5 MB/s behind 20 ms.
    assert!((2e6..=5.5e6).contains(&rate), "{rate}");
}
