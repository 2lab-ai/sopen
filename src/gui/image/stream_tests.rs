//! 06 §7.3 and §12's V14 and V15: the image window on a remote file read through a `Source`.
//! The Source is a real one on the `Fake` (06 §2), its engine stepped by hand — a test decides
//! when a reply arrives — so every bound is in bytes fetched and buffers asked for, never in
//! seconds. The fixtures are written here, byte by byte: a progressive JPEG (the `image` crate
//! writes baseline only), PNGs plain and Adam7-interlaced, and headers that lie.

use super::probe::{self, Ask, Head, Probe};
use super::*;
use crate::gui::testkit::Driver;
use crate::gui::Window as _;
use crate::remote::{
    fake, Asked, Change, FakeControl, FakeFile, Fault, Link, LocalFs, RemoteFs, Stat,
};
use crate::stream::{Engine, Opening, Settings, BLOCK};
use eframe::egui::Modifiers;
use std::time::Instant;

/// Only catches a hang.
const HANG: Duration = Duration::from_secs(120);
/// No poll within a test that does not ask for one.
const NEVER: Duration = Duration::from_secs(1_000_000);
const MIB: u64 = 1 << 20;

// ---------------------------------------------------------------------------------------------
// Fixtures.

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5000) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let typed = [kind.as_slice(), data].concat();
    out.extend_from_slice(&typed);
    out.extend_from_slice(&crc32(&typed).to_be_bytes());
}

fn png_head(w: u32, h: u32, interlaced: bool) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    // 8 bits, RGB, deflate, the one filter method.
    ihdr.extend_from_slice(&[8, 2, 0, 0, u8::from(interlaced)]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    out
}

/// The colour of a test picture at a place: different in every row and column.
fn shade(x: u32, y: u32) -> [u8; 3] {
    [(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8 | 1]
}

/// An RGB PNG written here: unfiltered rows in stored (uncompressed) deflate blocks, so a
/// share of the bytes is the same share of the rows; Adam7 where asked.
pub(super) fn png(w: u32, h: u32, interlaced: bool) -> Vec<u8> {
    let mut raw = Vec::new();
    let passes: &[(u32, u32, u32, u32)] = if interlaced {
        &[
            (0, 8, 0, 8),
            (4, 8, 0, 8),
            (0, 4, 4, 8),
            (2, 4, 0, 4),
            (0, 2, 2, 4),
            (1, 2, 0, 2),
            (0, 1, 1, 2),
        ]
    } else {
        &[(0, 1, 0, 1)]
    };
    for &(x0, dx, y0, dy) in passes {
        if x0 >= w {
            continue;
        }
        for y in (y0..h).step_by(dy as usize) {
            raw.push(0);
            for x in (x0..w).step_by(dx as usize) {
                raw.extend_from_slice(&shade(x, y));
            }
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none()));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = png_head(w, h, interlaced);
    for part in z.chunks(64 << 10) {
        png_chunk(&mut out, b"IDAT", part);
    }
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// A PNG of a few bytes whose header claims `w`×`h` (*view* §2.4's 74-byte file).
fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    let mut out = png_head(w, h, false);
    png_chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    png_chunk(&mut out, b"IEND", &[]);
    out
}

/// A JPEG's entropy-coded bits: bytes with `FF` stuffed, padded with ones.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                if self.acc as u8 == 0xFF {
                    self.out.push(0);
                }
                (self.acc, self.n) = (0, 0);
            }
        }
    }

    /// A coefficient as JPEG writes it after its size: `size` low bits, negatives one less.
    fn value(&mut self, v: i32) {
        let size = 32 - v.unsigned_abs().leading_zeros();
        let bits = if v < 0 { v + (1 << size) - 1 } else { v };
        self.put(bits as u32, size);
    }

    fn end(mut self) -> Vec<u8> {
        while self.n != 0 {
            self.put(1, 1);
        }
        self.out
    }
}

pub(super) fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(payload);
}

/// The DC coefficient of block `(bx, by)` of [`progressive`]: with the quantiser of 8 it is
/// the block's mean, as a distance from mid grey.
fn dc(bx: u32, by: u32) -> i32 {
    ((bx * 5 + by * 3) % 201) as i32 - 100
}

/// A grey progressive JPEG (SOF2) of `w`×`h` (multiples of 8), written here: a DC scan — the
/// whole frame, coarse — then one scan with `ac` more coefficients for every block, which is
/// nearly all of the file. Its Huffman tables give every symbol a code of one length (4 bits
/// for DC sizes, 8 for AC runs and sizes), so the coding is a table lookup.
pub(super) fn progressive(w: u32, h: u32, ac: u32) -> Vec<u8> {
    assert!(w.is_multiple_of(8) && h.is_multiple_of(8));
    let mut out = vec![0xFF, 0xD8];
    segment(&mut out, 0xDB, &[[0u8].as_slice(), &[8u8; 64]].concat());
    let mut sof = vec![8];
    sof.extend_from_slice(&(h as u16).to_be_bytes());
    sof.extend_from_slice(&(w as u16).to_be_bytes());
    sof.extend_from_slice(&[1, 1, 0x11, 0]);
    segment(&mut out, 0xC2, &sof);
    // DC table 0: sizes 0..=11, each a 4-bit code equal to itself.
    let mut dht = vec![0x00];
    dht.extend_from_slice(&[0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    dht.extend(0u8..12);
    segment(&mut out, 0xC4, &dht);
    // AC table 0: end of block, a run of 16 zeros, then every (run, size): 8-bit codes, in
    // this order.
    let mut symbols = vec![0x00u8, 0xF0];
    for run in 0u8..16 {
        for size in 1u8..=10 {
            symbols.push((run << 4) | size);
        }
    }
    let mut dht = vec![0x10];
    dht.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, symbols.len() as u8]);
    dht.extend_from_slice(&[0; 8]);
    dht.extend_from_slice(&symbols);
    segment(&mut out, 0xC4, &dht);
    let code_of = |symbol: u8| symbols.iter().position(|s| *s == symbol).unwrap() as u32;
    let (bw, bh) = (w / 8, h / 8);
    // The DC scan.
    segment(&mut out, 0xDA, &[1, 1, 0x00, 0, 0, 0x00]);
    let mut bits = Bits::default();
    let mut before = 0;
    for by in 0..bh {
        for bx in 0..bw {
            let diff = dc(bx, by) - before;
            before = dc(bx, by);
            let size = 32 - diff.unsigned_abs().leading_zeros();
            bits.put(size, 4);
            bits.value(diff);
        }
    }
    out.extend_from_slice(&bits.end());
    // The AC scan: the first `ac` coefficients of each block, small and of both signs.
    segment(&mut out, 0xDA, &[1, 1, 0x00, 1, 63, 0x00]);
    let mut bits = Bits::default();
    let mut seed = 0x2545_F491u32;
    let end = code_of(0x00);
    let sizes = [code_of(1), code_of(2)];
    for _ in 0..bw * bh {
        for _ in 0..ac {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = ((seed >> 24) % 3) as i32 + 1;
            let v = if seed & 0x10_0000 == 0 { v } else { -v };
            let size = 32 - v.unsigned_abs().leading_zeros();
            bits.put(sizes[size as usize - 1], 8);
            bits.value(v);
        }
        bits.put(end, 8);
    }
    out.extend_from_slice(&bits.end());
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

/// A picture with something in every block: what a baseline JPEG's rows are told apart by.
fn busy(w: u32, h: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        let n = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 13;
        image::Rgb([
            (40 + (x * 150 / w) + n % 60) as u8,
            (200 - (y * 150 / h)) as u8,
            (n % 200) as u8,
        ])
    }))
}

/// A baseline JPEG, as the `image` crate writes one.
pub(super) fn baseline(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    busy(w, h)
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
        .unwrap();
    out
}

/// `jpeg` with `segments` (marker, payload) put right after its SOI.
pub(super) fn with_segments(jpeg: &[u8], segments: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec();
    for (marker, payload) in segments {
        segment(&mut out, *marker, payload);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// An Exif APP1 payload: the orientation in IFD0, `thumb` as IFD1's JPEG, zeros up to `len`.
pub(super) fn exif(thumb: &[u8], orientation: u16, len: usize) -> Vec<u8> {
    let mut tiff = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
    let entry = |tiff: &mut Vec<u8>, tag: u16, kind: u16, value: [u8; 4]| {
        tiff.extend_from_slice(&tag.to_be_bytes());
        tiff.extend_from_slice(&kind.to_be_bytes());
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&value);
    };
    tiff.extend_from_slice(&1u16.to_be_bytes());
    let o = orientation.to_be_bytes();
    entry(&mut tiff, 0x0112, 3, [o[0], o[1], 0, 0]);
    tiff.extend_from_slice(&26u32.to_be_bytes());
    tiff.extend_from_slice(&2u16.to_be_bytes());
    entry(&mut tiff, 0x0201, 4, 56u32.to_be_bytes());
    entry(&mut tiff, 0x0202, 4, (thumb.len() as u32).to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(tiff.len(), 56);
    tiff.extend_from_slice(thumb);
    let mut out = b"Exif\0\0".to_vec();
    out.extend_from_slice(&tiff);
    assert!(out.len() <= len && len <= 65_533, "{} in {len}", out.len());
    out.resize(len, 0);
    out
}

/// A thumbnail as a camera stores one: a small baseline JPEG, of a size no test image has.
fn thumb() -> Vec<u8> {
    baseline(160, 104)
}

/// Where the frame header of `jpeg` ends, found the slow way: every segment by its length.
fn sof_end(jpeg: &[u8]) -> u64 {
    let mut at = 2;
    loop {
        assert_eq!(jpeg[at], 0xFF, "a marker at {at}");
        let len = usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
        if matches!(jpeg[at + 1], 0xC0..=0xC2) {
            return (at + 2 + len) as u64;
        }
        at += 2 + len;
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn loading() -> Hooks {
    Hooks {
        state: crate::gui::State {
            loading: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------------------------
// The window on a Source over the Fake.

struct Fx {
    _tmp: tempfile::TempDir,
    source: Source,
    engine: Engine,
    control: FakeControl,
    /// The stats lines (06 §11), as `SOPEN_STREAM_STATS=1` prints them.
    lines: Arc<Mutex<Vec<String>>>,
    win: Streaming,
    drv: Driver,
    /// Replies taken so far: the link answers in the order it was asked, so the next reply is
    /// to request number `taken`.
    taken: usize,
}

fn fx_of(file: FakeFile, hooks: &Hooks, budget: u64) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let at = Stat {
        size: file.size,
        mtime: file.mtime,
        ..LocalFs::new().stat(tmp.path().to_str().unwrap()).unwrap()
    };
    let (transport, control) = fake("/m.img", file, Link::gate());
    let base = transport.now() - control.elapsed();
    let (clock, again) = (control.clone(), control.clone());
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let settings = Settings {
        poll_every: NEVER,
        stats: Some(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string())
        })),
        throttle: None,
        clock: Arc::new(move || base + clock.elapsed()),
    };
    let opening = Opening {
        label: "host:/m.img".into(),
        began: Instant::now(),
        sniff: 0,
    };
    let (source, engine) = Source::build(
        transport,
        Box::new(move || again.open()),
        Arc::new(|| {}),
        opening,
        settings,
    );
    let origin = Origin::new(
        Box::new(LocalFs::new()),
        None,
        Some("host".into()),
        "/m.img".into(),
        at,
    );
    let cache = tmp.path().join("cache");
    let input = Input::Stream(source.clone());
    let win = Streaming::new(input, origin, &cache, hooks, Vec::new(), budget).unwrap();
    Fx {
        _tmp: tmp,
        source,
        engine,
        control,
        lines,
        win,
        drv: Driver::new(900.0, 700.0),
        taken: 0,
    }
}

fn fx(bytes: Vec<u8>) -> Fx {
    fx_of(FakeFile::bytes(bytes), &Hooks::default(), probe::BUDGET)
}

impl Fx {
    /// Sends what may go; how many replies are on their way.
    fn out(&mut self) -> usize {
        self.engine.pump();
        self.engine.transport().map_or(0, |r| r.pending())
    }

    /// Takes one reply, if one is on its way.
    fn reply(&mut self) -> bool {
        if self.out() == 0 {
            return false;
        }
        for _ in 0..1000 {
            if self.engine.step(None) {
                self.taken += 1;
                return true;
            }
        }
        panic!("no reply came");
    }

    /// What the next reply answers, if a request waits for one.
    fn next(&self) -> Option<Asked> {
        self.control.sent().get(self.taken).map(|sent| sent.asked)
    }

    /// Whether the next reply brings no byte of the file at or past `stop`: the link serves
    /// the file up to there and no further (a status reply is always let through).
    fn serves_below(&self, stop: u64) -> bool {
        !matches!(self.next(), Some(Asked::Read { off, len }) if off + u64::from(len) > stop)
    }

    /// Passes and replies in turn until `done`; a reply is taken only while `feed` says so.
    ///
    /// Only to a request sent before `done` looked: the loader asks on threads of its own, so
    /// one sent after the look can belong to a step the look did not see — V14's file read,
    /// begun the moment the header is in. What is on its way is counted first, then `done`
    /// looks; replies come in order, so while that count is not 0 the next reply is to one of
    /// those. What has been fetched when `done` holds was all asked before it held.
    fn run(
        &mut self,
        what: &str,
        mut feed: impl FnMut(&Fx) -> bool,
        mut done: impl FnMut(&Fx) -> bool,
    ) {
        let started = Instant::now();
        loop {
            self.drv.idle(&mut self.win, 1);
            let out = self.out();
            if done(self) {
                return;
            }
            if !(out > 0 && feed(self) && self.reply()) {
                // Nothing was on its way: a worker may be about to ask, or to answer.
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(started.elapsed() < HANG, "{what}: not within {HANG:?}");
        }
    }

    fn until(&mut self, what: &str, done: impl FnMut(&Fx) -> bool) {
        self.run(what, |_| true, done);
    }

    /// Takes every reply on its way, a pass after each. Only for a loader that is known to ask
    /// for nothing more by itself — it waits at the memory line, or it has ended — so that
    /// what is on its way is all there will be.
    fn drain(&mut self) {
        assert!(
            self.asking() || self.loader_ended(),
            "the loader may still ask: nothing can be said of what it will fetch"
        );
        for _ in 0..1_000_000 {
            self.drv.idle(&mut self.win, 1);
            if !self.reply() {
                return;
            }
        }
        panic!("the stream did not run dry");
    }

    /// The loader waits at the memory line: it asks the stream for nothing until an answer.
    fn asking(&self) -> bool {
        self.win.load.progress().ask.is_some()
    }

    /// The loader and its workers have ended, or the loader has handed in its result.
    fn loader_ended(&self) -> bool {
        self.workers_ended() || self.win.failure().is_some() || self.win.image.is_some()
    }

    /// Where the link stops for V14's "by 25 % fetched": a quarter of the file, up to the end
    /// of the read step it falls in (the loader reads the file a step at a time).
    fn quarter(&self) -> u64 {
        let step = READ_STEP as u64;
        self.source.size().div_ceil(4).div_ceil(step) * step
    }

    /// Serves the file up to [`Fx::quarter`] and no further, until a picture of those bytes
    /// exists; what was fetched by then.
    fn picture_by_a_quarter(&mut self) -> (u64, Arc<image::RgbaImage>) {
        let (size, stop) = (self.source.size(), self.quarter());
        self.run(
            "a picture by a quarter",
            |fx| fx.serves_below(stop),
            |fx| fx.partial().is_some(),
        );
        let fetched = self.fetched();
        assert!(
            fetched <= size / 4 + READ_STEP as u64,
            "{fetched} fetched of {size} by the first picture"
        );
        let (at, picture) = self.partial().unwrap();
        assert!(at <= fetched, "made of {at} bytes, {fetched} fetched");
        (fetched, picture)
    }

    fn fetched(&self) -> u64 {
        self.source.stats().fetched.total
    }

    fn requests(&self) -> usize {
        self.control.sent().len()
    }

    fn loaded(&self) -> u64 {
        self.win.load.progress().loaded
    }

    fn probed(&self) -> bool {
        self.win.load.progress().probed
    }

    fn reserved(&self) -> u64 {
        self.win.load.progress().reserved
    }

    fn sketch(&self) -> Option<(Sketch, Arc<image::RgbaImage>)> {
        self.win.load.progress().sketch.clone()
    }

    fn partial(&self) -> Option<(u64, Arc<image::RgbaImage>)> {
        match self.sketch() {
            Some((Sketch::Partial { at }, picture)) => Some((at, picture)),
            _ => None,
        }
    }

    /// Every text the last pass painted.
    fn painted(&self) -> Vec<String> {
        self.drv.texts().into_iter().map(|(_, _, t)| t).collect()
    }

    fn says(&self, needle: &str) -> bool {
        self.painted().iter().any(|t| t.contains(needle))
    }

    fn key(&mut self, key: Key) {
        self.drv.key(&mut self.win, key, Modifiers::NONE);
    }

    /// The stats line of `event`, if it was written.
    fn line(&self, event: &str) -> Option<String> {
        let lines = self.lines.lock().unwrap();
        lines
            .iter()
            .find(|l| l.starts_with(&format!("stream {event} ")))
            .cloned()
    }

    /// The loader and every worker of it have ended: what they held is freed.
    fn workers_ended(&self) -> bool {
        Arc::strong_count(&self.win.load) == 1
    }

    fn image(&self) -> &ImageWindow {
        self.win.image.as_deref().expect("the image window")
    }
}

fn colour_of(fx: &Fx, text: &str) -> Color32 {
    fx.drv
        .colors_of(text)
        .unwrap_or_else(|| panic!("{text}: {:?}", fx.painted()))[0]
}

/// A number of a stats line.
fn stat(line: &str, name: &str) -> u64 {
    line.split_whitespace()
        .find_map(|part| part.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} in {line}"))
        .parse()
        .unwrap()
}

/// How far a grey level is from what block `(bx, by)` of [`progressive`] decodes to.
fn off_dc(picture: &image::RgbaImage, w: u32, h: u32, bx: u32, by: u32) -> i32 {
    // The middle of the block, in the picture's own pixels.
    let x = (bx * 8 + 4) * picture.width() / w;
    let y = (by * 8 + 4) * picture.height() / h;
    i32::from(picture.get_pixel(x, y).0[0]) - (128 + dc(bx, by))
}

// ---------------------------------------------------------------------------------------------
// §12 V14: the header first, then pictures of what has arrived.

/// V14: a 6000×4000 progressive JPEG shows its size and Exif thumbnail having fetched no more
/// than the bytes up to its SOF and one block; a whole-frame picture exists by 25 % fetched;
/// Esc stops the read and keeps nothing.
#[test]
fn v14_a_progressive_jpeg_shows_its_thumbnail_then_the_whole_frame_coarse_and_esc_keeps_nothing() {
    let (w, h) = (6000, 4000);
    let plain = progressive(w, h, 35);
    let file = with_segments(&plain, &[(0xE1, exif(&thumb(), 1, 20_000))]);
    let size = file.len() as u64;
    assert!(size > 15 * MIB, "{size}");
    let header = sof_end(&file);
    let mut fx = fx(file);
    // The header: one reply at a time, until the window has it.
    fx.until("the header", |fx| fx.probed());
    let at_header = fx.fetched();
    assert!(
        at_header <= header + BLOCK,
        "{at_header} fetched for a header of {header}"
    );
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.says("6000×4000"), "{:?}", fx.painted());
    assert!(fx.says("MB, loading"), "{:?}", fx.painted());
    assert!(
        matches!(fx.win.drawn, Drawn::Sketch(Sketch::Thumb)),
        "{:?}",
        fx.win.drawn
    );
    assert_eq!(fx.drv.pictures().len(), 1, "the thumbnail is on screen");
    // 06 §11: `first` is that frame's, with what had been fetched by then.
    let first = fx.line("first").expect("the first line");
    assert_eq!(stat(&first, "fetched"), at_header, "{first}");
    // The link serves a quarter of the file and no more: the whole frame is there, coarse.
    let (fetched, picture) = fx.picture_by_a_quarter();
    // Blocks far below the rows whose detail has come: each is its own coarse value, not the
    // flat grey of a row that is not there.
    for (near, by) in [(20, 250), (400, 380), (740, 498), (3, 499)] {
        let bx = (near..near + 40)
            .find(|bx| dc(*bx, by).abs() >= 50)
            .unwrap();
        let off = off_dc(&picture, w, h, bx, by);
        assert!(off.abs() <= 8, "block ({bx}, {by}) is {off} off");
    }
    fx.drv.idle(&mut fx.win, 2);
    assert!(matches!(
        fx.win.drawn,
        Drawn::Sketch(Sketch::Partial { .. })
    ));
    // Esc: the window asks to close, the read stops and nothing of it is kept.
    let buffer = fx.win.load.progress().buffer.clone();
    assert!(buffer.upgrade().is_some(), "the file is being read");
    fx.key(Key::Escape);
    assert!(fx.drv.closed(), "Esc closes the window");
    fx.until("the workers end", |fx| fx.workers_ended());
    let requests = fx.requests();
    fx.drain();
    assert!(fx.win.image.is_none() && fx.win.failure().is_none());
    assert_eq!(fx.requests(), requests, "nothing is asked after Esc");
    // What was on its way when Esc came is at most the cap (06 §4).
    assert!(
        fx.fetched() <= fetched + crate::stream::CAP_MAX && fx.fetched() < size / 2,
        "{} fetched of {size} after Esc at {fetched}",
        fx.fetched()
    );
    // Nothing is kept: the buffer the file was read into is gone with the loader, the result
    // was never made, and — the window gone too — nothing was written under the cache root
    // (06 §7.3: no cache copy is made) or anywhere beside it.
    assert!(buffer.upgrade().is_none(), "the bytes read are still held");
    assert!(fx.win.load.progress().done.is_none());
    let Fx { _tmp: tmp, win, .. } = fx;
    drop(win);
    let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

/// V14: a 3000×2000 plain PNG shows at least 10 % of its rows by 25 % fetched.
#[test]
fn v14_a_plain_png_shows_its_rows_so_far() {
    let (w, h) = (3000, 2000);
    let file = png(w, h, false);
    let mut fx = fx(file);
    let (_, picture) = fx.picture_by_a_quarter();
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.says("3000×2000"), "{:?}", fx.painted());
    // Rows from the top, as the file has them; the rest is not there yet.
    let rows = (0..picture.height())
        .take_while(|y| picture.get_pixel(picture.width() / 2, *y).0[3] == 255)
        .count() as u32;
    assert!(
        rows * 10 >= picture.height() && rows * 2 < picture.height(),
        "{rows} rows of {}",
        picture.height()
    );
    for y in [0, rows / 2, rows - 1] {
        let x = picture.width() / 3;
        let from = shade(x * w / picture.width(), y * h / picture.height());
        assert_eq!(picture.get_pixel(x, y).0[..3], from, "row {y}");
    }
    assert_eq!(picture.get_pixel(5, picture.height() - 1).0[3], 0);
}

/// V14: a baseline JPEG shows its top rows by 25 % fetched; the rows not there are flat.
#[test]
fn v14_a_baseline_jpeg_shows_its_top_rows() {
    let (w, h) = (1600, 1200);
    let file = baseline(w, h);
    let whole = Doc::decode(&file).unwrap().img.to_rgba8();
    let mut fx = fx(file);
    let (_, picture) = fx.picture_by_a_quarter();
    assert_eq!(picture.dimensions(), (w, h));
    let same = |y: u32| {
        (0..w)
            .step_by(7)
            .all(|x| picture.get_pixel(x, y) == whole.get_pixel(x, y))
    };
    assert!(same(0) && same(h / 10), "the top rows are the picture's");
    // The last row is one colour: nothing of it has come.
    let flat = (0..w).all(|x| picture.get_pixel(x, h - 1) == picture.get_pixel(0, h - 1));
    assert!(flat && !same(h - 1), "the bottom is not there yet");
}

/// An image as a progressive display shows an Adam7 file after its first `passes` passes:
/// each sample painted over its footprint, in file order, so a later pass refines what the
/// earlier ones drew. Written out the plain way, pass by pass, as a reference.
fn adam7_after(w: u32, h: u32, passes: usize) -> image::RgbImage {
    // Where a pass starts, how far apart its samples are, and the block a sample stands for.
    const PASSES: [[u32; 6]; 7] = [
        [0, 8, 0, 8, 8, 8],
        [4, 8, 0, 8, 4, 8],
        [0, 4, 4, 8, 4, 4],
        [2, 4, 0, 4, 2, 4],
        [0, 2, 2, 4, 2, 2],
        [1, 2, 0, 2, 1, 2],
        [0, 1, 1, 2, 1, 1],
    ];
    let mut out = image::RgbImage::new(w, h);
    for [x0, dx, y0, dy, fw, fh] in PASSES.into_iter().take(passes) {
        for y in (y0..h).step_by(dy as usize) {
            for x in (x0..w).step_by(dx as usize) {
                for yy in y..(y + fh).min(h) {
                    for xx in x..(x + fw).min(w) {
                        out.put_pixel(xx, yy, image::Rgb(shade(x, y)));
                    }
                }
            }
        }
    }
    out
}

/// Where pass `passes` of [`png`]`(w, h, true)` ends in the file: its raw rows, and room for
/// what the file wraps them in (far less than the pass after it).
fn adam7_cut(w: u32, h: u32, passes: usize) -> usize {
    const PASSES: [[u32; 4]; 7] = [
        [0, 8, 0, 8],
        [4, 8, 0, 8],
        [0, 4, 4, 8],
        [2, 4, 0, 4],
        [0, 2, 2, 4],
        [1, 2, 0, 2],
        [0, 1, 1, 2],
    ];
    let raw: usize = PASSES[..passes]
        .iter()
        .map(|[x0, dx, y0, dy]| {
            let (pw, ph) = ((w - x0).div_ceil(*dx), (h - y0).div_ceil(*dy));
            (ph * (1 + 3 * pw)) as usize
        })
        .sum();
    raw + 2000
}

/// 06 §7.3: an interlaced PNG shows the whole frame from its first pass on, each completed
/// pass laid over the ones before — a later pass refines the picture, it does not replace
/// it with its own samples alone. The fixture's colour differs in every row and column, so a
/// sample in the wrong place, or one pass shown alone, is another picture.
#[test]
fn an_interlaced_png_shows_a_whole_coarse_frame_from_its_first_passes() {
    let (w, h) = (1024, 768);
    let file = png(w, h, true);
    let header = header_of(&file);
    let cut = |n: usize| Cursor::new(&file[..n.min(file.len())]);
    for passes in 1..=7 {
        let expected = adam7_after(w, h, passes);
        // At its own size, and sampled down as a large image's picture is.
        for side in [2048, 256] {
            let end = adam7_cut(w, h, passes);
            let picture = probe::partial(cut(end), &header, side).expect("a pass");
            let (pw, ph) = picture.dimensions();
            assert_eq!((pw, ph), if side == 2048 { (w, h) } else { (256, 192) });
            for y in 0..ph {
                for x in 0..pw {
                    let from = expected.get_pixel(x * w / pw, y * h / ph).0;
                    assert_eq!(
                        picture.get_pixel(x, y).0,
                        [from[0], from[1], from[2], 255],
                        "({x}, {y}) after pass {passes}, side {side}"
                    );
                }
            }
        }
    }
    // An unfinished pass is not shown: just short of pass 5's end, the picture is pass 4's.
    let short = adam7_cut(w, h, 5) - 2000 - 4000;
    let picture = probe::partial(cut(short), &header, 2048).unwrap();
    assert_eq!(
        image::DynamicImage::ImageRgba8(picture).to_rgb8(),
        adam7_after(w, h, 4)
    );
    // The whole file is the image itself, as the full decode gives it.
    let whole = probe::partial(cut(file.len()), &header, 2048).unwrap();
    assert_eq!(whole, Doc::decode(&file).unwrap().img.to_rgba8());
    // A picture never takes more memory than the header said (what the memory line was
    // about): a header of a smaller frame than the bytes hold gives none.
    let jpeg = baseline(320, 240);
    let honest = header_of(&jpeg);
    assert!(probe::partial(Cursor::new(&jpeg[..]), &honest, 2048).is_some());
    let smaller = probe::Header {
        need: honest.need - 1,
        ..honest
    };
    assert!(probe::partial(Cursor::new(&jpeg[..]), &smaller, 2048).is_none());
    // Before its first pass is whole there is no picture.
    assert!(probe::partial(cut(2000), &header, 2048).is_none());
}

// ---------------------------------------------------------------------------------------------
// §12 V15: header limits and the memory line.

/// V15: a JPEG with a 64 KB Exif APP1 and an ICC segment before its SOF shows its size and
/// thumbnail having fetched at most 1 MiB.
#[test]
fn v15_a_jpeg_with_large_metadata_before_its_frame_shows_size_and_thumbnail_within_the_cap() {
    let icc = [b"ICC_PROFILE\0\x01\x01".as_slice(), &[7u8; 60_000]].concat();
    let file = with_segments(
        &baseline(640, 480),
        &[(0xE1, exif(&thumb(), 6, 65_533)), (0xE2, icc)],
    );
    let header = sof_end(&file);
    assert!(header > 2 * BLOCK - 8192, "{header}");
    let mut fx = fx_of(FakeFile::bytes(file), &loading(), probe::BUDGET);
    fx.until("the header", |fx| fx.probed());
    let fetched = fx.fetched();
    assert!(fetched <= probe::HEADER_CAP, "{fetched}");
    assert!(fetched <= header + BLOCK, "{fetched} for {header}");
    fx.drv.idle(&mut fx.win, 2);
    // Orientation 6 turns it: the size shown is the upright one.
    assert!(fx.says("480×640"), "{:?}", fx.painted());
    assert!(matches!(fx.win.drawn, Drawn::Sketch(Sketch::Thumb)));
    let (_, thumb) = fx.sketch().unwrap();
    assert_eq!(thumb.dimensions(), (104, 160), "the thumbnail, upright");
    // `loading=1` (06 §11): this frame is the receipt's.
    assert!(fx.win.ready());
}

/// V15: a PNG whose header claims 100000×100000 shows the memory line with Continue and
/// Close, having fetched at most 64 KiB; nothing is reserved until Continue, and Continue
/// proceeds.
#[test]
fn v15_a_huge_header_shows_the_memory_line_and_continue_proceeds() {
    // The header's few bytes, and megabytes of file behind them: a read that went on past the
    // header would show in what is fetched.
    let mut file = FakeFile::bytes(png_claiming(100_000, 100_000));
    file.size = 8 * MIB;
    let need = 30_000_000_000u64;
    let mut fx = fx_of(file, &Hooks::default(), probe::BUDGET);
    fx.until("the memory line", |fx| fx.says("Continue (Enter)"));
    fx.drain();
    // With Continue unpressed: 64 KiB at most, and no request more however long it waits.
    let (fetched, requests) = (fx.fetched(), fx.requests());
    assert!(fetched <= 64 << 10, "{fetched} fetched before Continue");
    fx.drv.idle(&mut fx.win, 50);
    fx.drain();
    assert_eq!((fx.fetched(), fx.requests()), (fetched, requests));
    let line = fx
        .painted()
        .into_iter()
        .find(|t| t.contains("Continue"))
        .unwrap();
    assert!(
        line.starts_with(
            "decoding this image needs 27.9 GB of memory (100000×100000); this machine has "
        ),
        "{line}"
    );
    assert!(
        line.ends_with(" — Continue (Enter) · Close (Esc)"),
        "{line}"
    );
    // More than this machine has is said in the error colour; Continue is offered all the same.
    let has = probe::physical_memory().unwrap();
    assert!(line.contains(&format!("this machine has {}", probe::memory_text(has))));
    let plain = colour_of(&fx, &line);
    fx.win.physical = Some(16 << 30);
    fx.drv.idle(&mut fx.win, 1);
    let line = line.replace(&format!("has {}", probe::memory_text(has)), "has 16 GB");
    let short = colour_of(&fx, &line);
    fx.win.physical = Some(64 << 30);
    fx.drv.idle(&mut fx.win, 1);
    let line = line.replace("has 16 GB", "has 64 GB");
    let enough = colour_of(&fx, &line);
    assert_ne!(short, enough, "over the machine's memory is another colour");
    assert_eq!(plain, if has < need { short } else { enough });
    // Hex view comes with W8b: until then it is not offered.
    assert!(!line.contains("Hex"), "{line}");
    assert!(fx.says("100000×100000"), "{:?}", fx.painted());
    assert!(fx.fetched() <= BLOCK, "{}", fx.fetched());
    assert_eq!(fx.reserved(), 0, "nothing is reserved before Continue");
    assert_eq!(
        fx.loaded(),
        0,
        "nothing past the header is read before Continue"
    );
    // A scripted run cannot answer: the line is what its screenshot shows.
    assert!(fx.win.ready());
    // Continue: the file is read, and the decode is asked of the allocator, whole. Either the
    // system refuses it — an error line, not an abort — or the file's bytes fail to fill it.
    fx.key(Key::Enter);
    fx.until("the decode", |fx| fx.win.failure().is_some());
    assert_eq!(fx.reserved(), need, "Continue asks for the decode's memory");
    assert!(fx.fetched() >= 8 * MIB, "Continue reads the file");
    let why = fx.win.failure().unwrap();
    assert!(
        why.contains("cannot decode the image") || why.contains("not enough memory"),
        "{why}"
    );
    assert!(
        fx.drv.closed(),
        "a file that cannot be shown asks to be closed"
    );
}

/// The memory bound, pinned: a 24 GB file — its header a small picture's, or none at all — is
/// not read into memory, and an image of huge dimensions is not decoded, until Continue: a
/// header's worth is fetched, and no buffer is asked for.
#[test]
fn a_24_gb_file_or_a_huge_image_takes_no_memory_before_continue() {
    const MODEL: u64 = 24_000_000_000;
    // A small picture's header, 24 GB behind it.
    let mut small = FakeFile::bytes(png(64, 64, false));
    small.size = MODEL;
    // A JPEG frame of 65535×65535, three components: 12.9 GB decoded.
    let mut huge = vec![0xFF, 0xD8];
    segment(
        &mut huge,
        0xC0,
        &[
            8, 0xFF, 0xFF, 0xFF, 0xFF, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0,
        ],
    );
    huge.extend_from_slice(&[0xFF, 0xD9]);
    // 24 GB whose first megabyte holds no frame: nothing is known of the picture.
    let mut blind = FakeFile::bytes(with_segments(
        &baseline(64, 64),
        &(0..17)
            .map(|_| (0xE5, vec![0u8; 65_000]))
            .collect::<Vec<_>>(),
    ));
    blind.size = MODEL;
    // 24 GB whose header is a 30000×30000 frame, 2.5 GB decoded: both are held at once, and
    // the line says their sum — not the decode alone, which is the smaller part.
    let mut both = vec![0xFF, 0xD8];
    segment(
        &mut both,
        0xC0,
        &[
            8, 0x75, 0x30, 0x75, 0x30, 3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0,
        ],
    );
    let mut both = FakeFile::bytes(both);
    both.size = MODEL;
    let read = "reading this image needs 22.4 GB of memory (the file itself)";
    let decode = "decoding this image needs 12 GB of memory (65535×65535)";
    let sum = "reading and decoding this image needs 24.9 GB of memory (30000×30000; the file \
               itself 22.4 GB)";
    assert_eq!(
        probe::memory_text(MODEL + 30_000 * 30_000 * 3),
        "24.9 GB",
        "the file and the decode together"
    );
    for (file, words, cap) in [
        (small, read, BLOCK),
        (FakeFile::bytes(huge), decode, BLOCK),
        (blind, read, probe::HEADER_CAP),
        (both, sum, BLOCK),
    ] {
        let mut fx = fx_of(file, &Hooks::default(), probe::BUDGET);
        fx.until(words, |fx| fx.says("Continue (Enter)"));
        fx.drain();
        assert!(fx.says(words), "{:?}", fx.painted());
        if fx.source.size() == MODEL {
            assert!(fx.says("24.0 GB"), "the file's size: {:?}", fx.painted());
        }
        let (fetched, requests) = (fx.fetched(), fx.requests());
        assert!(fetched <= cap, "{words}: {fetched} fetched");
        fx.drv.idle(&mut fx.win, 50);
        fx.drain();
        assert_eq!(
            (fx.fetched(), fx.requests()),
            (fetched, requests),
            "{words}"
        );
        assert_eq!(fx.reserved(), 0, "{words}: a buffer was asked for");
        assert_eq!(fx.loaded(), 0, "{words}");
        assert!(fx.win.image.is_none() && fx.win.failure().is_none());
        // Close (Esc) ends it there.
        fx.key(Key::Escape);
        assert!(fx.drv.closed());
        fx.until("the loader ends", |fx| fx.workers_ended());
        assert_eq!(fx.reserved(), 0, "{words}");
    }
}

/// Continue on an image that does decode: the file streams in whole and the window becomes the
/// image window — the bytes as opened, their SHA-256 as the Save baseline (01 §5), each byte
/// fetched once, and the stream let go.
#[test]
fn continue_reads_the_file_and_the_window_becomes_the_image_window() {
    let file = png(300, 200, false);
    let (size, hash) = (file.len() as u64, sha256(&file));
    // A budget this picture's 180 000 bytes are over.
    let mut fx = fx_of(FakeFile::bytes(file.clone()), &Hooks::default(), 100_000);
    fx.until("the memory line", |fx| fx.says("Continue (Enter)"));
    fx.drain();
    // (The file is over this small budget too: the line is for both.)
    assert!(
        fx.says(
            "reading and decoding this image needs 0.3 MB of memory (300×200; the file itself \
             0.2 MB)"
        ),
        "{:?}",
        fx.painted()
    );
    assert_eq!((fx.reserved(), fx.loaded()), (0, 0));
    assert!(!fx.says("loading 0 %"), "nothing loads while it asks");
    fx.key(Key::Enter);
    fx.until("the image", |fx| fx.win.image.is_some());
    assert_eq!(fx.reserved(), size.max(180_000));
    let image = fx.image();
    assert_eq!(image.doc.size(), (300, 200));
    let held = image.file.lock().unwrap();
    assert_eq!(held.original(), file.as_slice());
    assert_eq!(held.origin.at.sha256, Some(hash));
    drop(held);
    assert!(
        fx.win.source.is_none(),
        "the stream is let go once the image is whole"
    );
    assert!(
        fx.fetched() <= size.div_ceil(BLOCK) * BLOCK,
        "{} fetched for {size}",
        fx.fetched()
    );
    // It is the image window: its rows, its keys, its picture.
    fx.drv.idle(&mut fx.win, 3);
    assert!(fx.win.ready());
    assert!(fx.says("300×200 PNG"), "{:?}", fx.painted());
    assert_eq!(fx.win.keys().len(), crate::gui::chrome::IMAGE_KEYS.len());
    assert!(!fx.says("Continue"));
}

// ---------------------------------------------------------------------------------------------
// The rest of §7.3.

/// Within the budget nothing is asked: the file streams in with its progress on the status
/// row, and without `loading=1` the window is ready only once the image is whole.
#[test]
fn a_small_image_streams_in_and_is_ready_when_whole() {
    let file = baseline(800, 600);
    let size = file.len() as u64;
    let mut fx = fx(file.clone());
    assert!(!fx.win.ready());
    fx.until("the header", |fx| fx.probed());
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.says("800×600") && fx.says("loading 0 %"),
        "{:?}",
        fx.painted()
    );
    assert!(!fx.says("Continue") && !fx.win.ready());
    assert!(fx.win.keys().is_empty(), "no Save while it loads");
    fx.until("the image", |fx| fx.win.image.is_some());
    assert_eq!(fx.image().file.lock().unwrap().original(), file.as_slice());
    assert_eq!(fx.image().doc.size(), (800, 600));
    assert!(fx.fetched() <= size.div_ceil(BLOCK) * BLOCK);
    fx.drv.idle(&mut fx.win, 3);
    assert!(fx.win.ready());
}

/// A JPEG whose frame is past the first megabyte has no header: not an error — the size line
/// alone, having fetched the cap and no more, then the file streams and the decoder decides.
#[test]
fn no_header_within_the_cap_shows_the_size_line_and_the_decoder_decides() {
    let file = with_segments(
        &baseline(320, 240),
        &(0..17)
            .map(|_| (0xE5, vec![0u8; 65_000]))
            .collect::<Vec<_>>(),
    );
    assert!(sof_end(&file) > probe::HEADER_CAP);
    let mut fx = fx(file);
    fx.until("the header read", |fx| fx.probed());
    assert!(fx.fetched() <= probe::HEADER_CAP, "{}", fx.fetched());
    assert!(fx.win.load.progress().header.is_none());
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.says("MB, loading") && !fx.says("×"),
        "{:?}",
        fx.painted()
    );
    fx.until("the image", |fx| fx.win.image.is_some());
    assert_eq!(fx.image().doc.size(), (320, 240));
}

/// A file that is no image, and a JPEG no decoder here reads, are said at once: one block is
/// fetched and the rest of the file is not streamed for a decode that cannot be.
#[test]
fn a_file_that_cannot_be_a_picture_is_refused_from_its_header() {
    let mut arithmetic = vec![0xFF, 0xD8];
    segment(
        &mut arithmetic,
        0xC9,
        &[8, 0x0F, 0xA0, 0x17, 0x70, 1, 1, 0x11, 0],
    );
    let mut twelve = vec![0xFF, 0xD8];
    segment(
        &mut twelve,
        0xC1,
        &[12, 0x0F, 0xA0, 0x17, 0x70, 1, 1, 0x11, 0],
    );
    for (head, why) in [
        (
            b"just text, not a picture at all\n".to_vec(),
            "not an image format sopen can read",
        ),
        (
            arithmetic,
            "cannot decode the image: an arithmetic-coded 8-bit JPEG (SOF9), 6000×4000, is not \
             supported",
        ),
        (
            twelve,
            "cannot decode the image: a 12-bit JPEG (SOF1), 6000×4000, is not supported",
        ),
    ] {
        let mut file = FakeFile::bytes(head);
        file.size = 40 * MIB;
        let mut fx = fx_of(file, &Hooks::default(), probe::BUDGET);
        fx.until(why, |fx| fx.win.failure().is_some());
        assert_eq!(fx.win.failure().as_deref(), Some(why));
        assert!(fx.drv.closed());
        fx.drain();
        assert!(fx.fetched() <= BLOCK, "{why}: {}", fx.fetched());
    }
}

/// Without a way to the hex window (`switch` `None`: no host that shows one), a file that
/// arrives whole and does not decode says so as before.
#[test]
fn a_file_that_does_not_decode_says_so_as_today() {
    let mut file = png(64, 64, false);
    let len = file.len();
    // The pixel data cut: the header is fine, the rows are not all there.
    file.truncate(len - 6000);
    let today = format!("{:#}", Doc::decode(&file).err().unwrap());
    let mut fx = fx(file);
    fx.until("the failure", |fx| fx.win.failure().is_some());
    assert_eq!(fx.win.failure(), Some(today));
    assert!(fx.drv.closed() && fx.win.image.is_none());
}

/// 06 §7.3 and V15 (W8b), in a window of its own ([`open_stream`]'s): a file that does not
/// decode gives way to the hex window on the same stream — why said first in its rows, its
/// first rows read from what the stream already holds — and the same window becomes the hex
/// window ([`Switching`]): it does not close, and it fails nothing.
#[test]
fn w8b_a_file_that_does_not_decode_becomes_the_hex_window_on_its_stream() {
    let mut file = png(64, 64, false);
    let len = file.len();
    file.truncate(len - 6000);
    let today = format!("{:#}", Doc::decode(&file).err().unwrap());
    let fx = fx(file);
    let Fx {
        _tmp,
        source,
        mut engine,
        mut win,
        mut drv,
        lines,
        ..
    } = fx;
    let switch = HexSwitch::new(crate::gui::hex::Repaint::default());
    win.switch = Some(switch.clone());
    let failed = win.failed.clone();
    let mut host = Switching {
        switch: Some(switch),
        image: Some(win),
        hex: None,
        hooks: Hooks::default(),
        hex_outcome: Arc::default(),
        hex_leftover: Arc::default(),
        failed,
    };
    let started = Instant::now();
    while drv
        .find_text("89 50 4E 47 0D 0A 1A 0A  00 00 00 0D 49 48 44 52")
        .is_none()
    {
        drv.idle(&mut host, 1);
        engine.pump();
        if engine.transport().map_or(0, |r| r.pending()) > 0 {
            engine.step(None);
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(started.elapsed() < HANG, "the hex rows never came");
    }
    assert!(
        host.image.is_none() && host.hex.is_some(),
        "the hex window now"
    );
    assert!(
        drv.texts().iter().any(|(_, _, t)| t.contains(&today)),
        "why, in its rows"
    );
    assert!(!drv.closed(), "the window stayed");
    assert_eq!(super::super::Window::failure(&host), None);
    assert!(source.state().status().is_none(), "the same stream, live");
    // One open, one `first` on the stats line (06 §11): the image's, not the hex window's too.
    let said = lines.lock().unwrap().clone();
    let firsts = said
        .iter()
        .filter(|l| l.starts_with("stream first "))
        .count();
    assert_eq!(firsts, 1, "{said:?}");
}

/// W8b: a stream that changed mid-read is what failed, not the file's content: with a way to
/// the hex window the window still says it changed and gives no way.
#[test]
fn w8b_a_file_changed_mid_read_is_not_given_to_the_hex_window() {
    let file = png(1200, 900, false);
    let size = file.len() as u64;
    let mut fx = fx(file);
    let switch = HexSwitch::new(crate::gui::hex::Repaint::default());
    fx.win.switch = Some(switch.clone());
    fx.until("a part", |fx| fx.loaded() > size / 4);
    fx.control.change(Change::Truncate(size / 2));
    fx.until("the failure", |fx| fx.win.failure().is_some());
    let why = fx.win.failure().unwrap();
    assert!(why.contains("changed"), "{why}");
    assert!(switch.take().is_none(), "no way to the hex window");
}

/// 06 §5 in the loading window: a connection lost mid-read is said, the screen stays, and the
/// read goes on when the stream is back — the image arrives whole.
#[test]
fn a_lost_connection_is_said_and_the_read_resumes() {
    let file = png(1200, 900, false);
    let size = file.len() as u64;
    let mut fx = fx(file.clone());
    fx.until("a part", |fx| fx.loaded() > size / 4);
    // The next request finds the channel closed.
    fx.control.fault(fx.requests(), Fault::Lost);
    fx.until("the warning", |fx| fx.win.load.progress().warning.is_some());
    fx.drv.idle(&mut fx.win, 2);
    let warning = fx.win.load.progress().warning.clone().unwrap();
    assert!(fx.says(&warning), "{warning}: {:?}", fx.painted());
    assert!(fx.win.image.is_none() && fx.win.failure().is_none());
    // Time passes on the link: the stream reconnects, and the read goes on from where it was.
    let started = Instant::now();
    while fx.win.image.is_none() {
        fx.engine.step(Some(Duration::from_millis(500)));
        fx.drv.idle(&mut fx.win, 1);
        assert!(started.elapsed() < HANG, "the read did not resume");
    }
    assert_eq!(fx.image().file.lock().unwrap().original(), file.as_slice());
    assert!(fx.win.load.progress().warning.is_none());
}

/// A file that changes on the remote while it streams is not shown as a mix of two versions:
/// the window says it changed.
#[test]
fn a_file_changed_mid_read_is_not_shown() {
    let file = png(1200, 900, false);
    let size = file.len() as u64;
    let mut fx = fx(file);
    fx.until("a part", |fx| fx.loaded() > size / 4);
    fx.control.change(Change::Truncate(size / 2));
    fx.until("the failure", |fx| fx.win.failure().is_some());
    let why = fx.win.failure().unwrap();
    assert!(why.contains("changed"), "{why}");
    assert!(fx.win.image.is_none() && fx.drv.closed());
}

/// `loading=1` (06 §11) on a file with no thumbnail: the window is ready at its first picture
/// of the bytes so far, not at the size line — the receipt of V14.
#[test]
fn loading_1_waits_for_the_first_picture_where_the_format_gives_one() {
    let file = png(1500, 1000, false);
    let size = file.len() as u64;
    let mut fx = fx_of(FakeFile::bytes(file), &loading(), probe::BUDGET);
    fx.until("the header", |fx| fx.probed());
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        fx.says("1500×1000") && !fx.win.ready(),
        "the size line is not the picture"
    );
    let half = size / 2;
    fx.run("a picture", |fx| fx.loaded() < half, |fx| fx.win.ready());
    assert!(matches!(
        fx.win.drawn,
        Drawn::Sketch(Sketch::Partial { .. })
    ));
    assert!(fx.win.image.is_none() && fx.loaded() < size);
    // A format with no partial picture is ready at its size line.
    let mut bmp = Vec::new();
    busy(400, 300)
        .write_to(&mut Cursor::new(&mut bmp), ImageFormat::Bmp)
        .unwrap();
    let mut fx = fx_of(FakeFile::bytes(bmp), &loading(), probe::BUDGET);
    fx.until("the header", |fx| fx.probed());
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.says("400×300") && fx.win.ready());
}

/// A preview (02 §4.2) of an image still loading: its lines over the picture, no rows of its
/// own, and no key of its own — Enter does not continue past the memory line.
#[test]
fn a_preview_shows_its_lines_and_takes_no_keys() {
    let file = FakeFile::bytes(png(300, 200, false));
    let mut fx = fx_of(file, &Hooks::default(), 100_000);
    fx.win.set_preview(true);
    fx.until("the memory line", |fx| fx.says("Continue (Enter)"));
    assert!(fx.says("300×200"), "{:?}", fx.painted());
    fx.key(Key::Enter);
    fx.key(Key::Escape);
    // Neither key was the preview's: it still asks (Continue answers in the pass that takes
    // the key), and it was not stopped.
    assert!(fx.asking() && !fx.win.load.cancel.load(Ordering::SeqCst));
    fx.drain();
    assert!(!fx.drv.closed() && fx.win.image.is_none());
    assert_eq!((fx.reserved(), fx.loaded()), (0, 0));
    // The hand-over: the keys are the window's again.
    fx.win.set_preview(false);
    fx.key(Key::Enter);
    fx.until("the image", |fx| fx.win.image.is_some());
}

/// 06 §7.3 on a local image: a file already in memory whose decode is over the budget shows
/// the memory line before any pixel buffer is asked for; Continue decodes it.
#[test]
fn a_local_image_over_the_budget_asks_before_it_decodes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.png");
    let bytes = png(300, 200, false);
    std::fs::write(&path, &bytes).unwrap();
    let mut fs = LocalFs::new();
    let abs = path.to_str().unwrap().to_string();
    let at = fs.stat(&abs).unwrap();
    let cache = tmp.path().join("cache");
    let file = |fs: LocalFs| {
        let origin = Origin::new(Box::new(fs), None, None, abs.clone(), at);
        ImageFile::new(origin, bytes.clone(), &cache)
    };
    // Within the budget it is decoded as before; a run that saves without a window has
    // nobody to ask, and decodes.
    let hooks = Hooks::default();
    let within = decoded_or_asking(file(fs), &hooks, Vec::new(), 180_000).unwrap();
    assert!(matches!(within, Start::Decoded(parts) if parts.1.size() == (300, 200)));
    let saving = Hooks {
        autosave: true,
        ..Default::default()
    };
    let told = decoded_or_asking(file(LocalFs::new()), &saving, Vec::new(), 100_000).unwrap();
    assert!(matches!(told, Start::Decoded(..)));
    let over = decoded_or_asking(file(LocalFs::new()), &hooks, Vec::new(), 100_000).unwrap();
    let Start::Asking(mut win) = over else {
        panic!("a decode over the budget is asked about first");
    };
    let win = &mut *win;
    let mut drv = Driver::new(900.0, 700.0);
    drv.until(win, 60, |win| win.asked);
    drv.idle(win, 1);
    let painted: Vec<String> = drv.texts().into_iter().map(|(_, _, t)| t).collect();
    assert!(
        painted.iter().any(|t| t == "300×200, 180.3 kB"),
        "the size line while it asks: {painted:?}"
    );
    assert!(painted.iter().any(|t| t.contains("Continue (Enter)")));
    assert_eq!(win.load.progress().reserved, 0);
    assert!(win.image.is_none());
    drv.key(win, Key::Enter, Modifiers::NONE);
    drv.until(win, 60, |win| win.image.is_some());
    assert_eq!(win.load.progress().reserved, 180_000);
    let image = win.image.as_deref().unwrap();
    assert_eq!(image.file.lock().unwrap().original(), bytes.as_slice());
}

/// 06 §7.3 on a local image in a window of its own: its memory line offers Hex view, and H
/// makes the same window the hex window on the file, read in place.
#[test]
fn w8b_hex_view_on_a_local_images_memory_line_becomes_the_hex_window() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("huge.png");
    let bytes = png_claiming(100_000, 100_000);
    std::fs::write(&path, &bytes).unwrap();
    let mut fs = LocalFs::new();
    let abs = path.to_str().unwrap().to_string();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = ImageFile::new(origin, bytes, &tmp.path().join("cache"));
    let hooks = Hooks::default();
    let asking = decoded_or_asking(file, &hooks, Vec::new(), probe::BUDGET).unwrap();
    let Start::Asking(win) = asking else {
        panic!("a decode over the budget is asked about first");
    };
    let failed = win.failed.clone();
    let mut host = Switching {
        switch: win.switch.clone(),
        image: Some(*win),
        hex: None,
        hooks,
        hex_outcome: Arc::default(),
        hex_leftover: Arc::default(),
        failed,
    };
    let mut drv = Driver::new(900.0, 700.0);
    drv.until(&mut host, 60, |host| {
        host.image.as_ref().is_some_and(|win| win.asked)
    });
    drv.idle(&mut host, 1);
    assert!(
        drv.texts()
            .iter()
            .any(|(_, _, t)| t.contains("Hex view (H)")),
        "the memory line offers Hex view"
    );
    drv.key(&mut host, Key::H, Modifiers::NONE);
    let started = Instant::now();
    while drv
        .find_text("89 50 4E 47 0D 0A 1A 0A  00 00 00 0D 49 48 44 52")
        .is_none()
    {
        assert!(started.elapsed() < HANG, "the hex rows never came");
        drv.idle(&mut host, 1);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        host.image.is_none() && host.hex.is_some(),
        "the hex window now"
    );
    assert!(!drv.closed(), "the window stayed");
}

/// `SOPEN_AUTOSAVE` through a stream: no window and nobody to ask — the file is read, the
/// state applied and the edit saved over the original, checked against the bytes read.
#[test]
fn autosave_through_a_stream_reads_applies_and_saves() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    let bytes = png(120, 80, false);
    std::fs::write(&path, &bytes).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let transport = Box::new(LocalFs::new()).into_ranged(&abs).ok().unwrap();
    let reopen: crate::stream::Reopen = Box::new(|| Err(anyhow!("no second connection")));
    let source = Source::open(transport, reopen, Arc::new(|| {})).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let hooks = Hooks {
        autosave: true,
        state: crate::gui::State {
            rect: Some([10, 10, 40, 30]),
            ops: vec![Op::Crop],
            ..Default::default()
        },
        ..Default::default()
    };
    let cache = tmp.path().join("cache");
    let out = open_stream(origin, source, &cache, &hooks, Vec::new(), None).unwrap();
    assert!(out.saved && out.refused.is_none());
    let saved = Doc::decode(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.size(), (40, 30));
}

// ---------------------------------------------------------------------------------------------
// The header read (06 §7.3's rules, X4).

fn header_of(bytes: &[u8]) -> probe::Header {
    match probe::probe(&mut Head::of(bytes)).unwrap() {
        Probe::Header(header) => header,
        other => panic!("{other:?}"),
    }
}

fn gives_up(bytes: &[u8]) -> bool {
    matches!(probe::probe(&mut Head::of(bytes)).unwrap(), Probe::Unknown)
}

/// The walk skips whole segments by their lengths: the Exif thumbnail's own SOF, inside the
/// APP1, is not the answer (a plain search for the first SOF code gives the thumbnail's size).
#[test]
fn the_walk_reads_the_frame_not_the_thumbnail_inside_the_exif() {
    let file = with_segments(&baseline(640, 480), &[(0xE1, exif(&thumb(), 1, 20_000))]);
    let first_sof = file.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap() as u64;
    assert!(
        first_sof + 19 < sof_end(&file),
        "the thumbnail's SOF comes first"
    );
    let header = header_of(&file);
    assert_eq!((header.width, header.height), (640, 480));
    assert_eq!(header.need, 640 * 480 * 3);
    assert_eq!(header.format, ImageFormat::Jpeg);
    let thumb_size = |h: &probe::Header| h.thumb.as_ref().map(|t| t.dimensions());
    assert_eq!(thumb_size(&header), Some((160, 104)));
    // Orientations 5–8 swap the sides shown; the thumbnail is turned with them.
    let turned = header_of(&with_segments(
        &baseline(640, 480),
        &[(0xE1, exif(&thumb(), 8, 20_000))],
    ));
    assert_eq!(turned.upright(), (480, 640));
    assert_eq!((turned.width, turned.height), (640, 480));
    assert_eq!(thumb_size(&turned), Some((104, 160)));
    // Only the first APP1 that starts `Exif\0\0` is the Exif.
    let xmp = b"http://ns.adobe.com/xap/1.0/\0<x/>".to_vec();
    let later = with_segments(
        &baseline(64, 48),
        &[(0xE1, xmp), (0xE1, exif(&thumb(), 6, 20_000))],
    );
    assert_eq!(header_of(&later).upright(), (48, 64));
}

/// The frame's components give the memory: one byte a pixel for one, three for three and for
/// four (CMYK decodes to RGB); fill bytes before a marker are skipped.
#[test]
fn the_frame_gives_the_memory_by_its_components() {
    let frame = |code: u8, components: u8, fill: usize| {
        let mut out = vec![0xFF, 0xD8];
        segment(&mut out, 0xE0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0");
        out.extend(std::iter::repeat_n(0xFF, fill));
        let mut sof = vec![8, 0x0B, 0xB8, 0x0F, 0xA0, components];
        for id in 0..components {
            sof.extend_from_slice(&[id + 1, 0x11, 0]);
        }
        segment(&mut out, code, &sof);
        out
    };
    for (code, components, fill, need) in [
        (0xC0, 1, 0, 4000 * 3000),
        (0xC1, 3, 0, 4000 * 3000 * 3),
        (0xC2, 4, 5, 4000 * 3000 * 3),
        (0xC2, 2, 0, 4000 * 3000 * 3),
    ] {
        let header = header_of(&frame(code, components, fill));
        assert_eq!(
            (header.width, header.height, header.need),
            (4000, 3000, need)
        );
    }
}

/// Everything else before the frame ends the walk with no header (not an error — the decoder
/// decides): it never searches forward for a marker.
#[test]
fn the_walk_gives_up_on_anything_else_before_the_frame() {
    let good = {
        let mut out = vec![0xFF, 0xD8];
        segment(&mut out, 0xC0, &[8, 0, 16, 0, 16, 1, 1, 0x11, 0]);
        out
    };
    assert_eq!(header_of(&good).need, 256);
    let before_frame = |bytes: &[u8]| [&[0xFF, 0xD8], bytes, &good[2..]].concat();
    for (what, bytes) in [
        ("a byte that is not FF", before_frame(&[0x00])),
        ("FF 00", before_frame(&[0xFF, 0x00])),
        ("TEM, a marker with no length", before_frame(&[0xFF, 0x01])),
        ("RST0", before_frame(&[0xFF, 0xD0])),
        ("RST7", before_frame(&[0xFF, 0xD7])),
        ("a second SOI", before_frame(&[0xFF, 0xD8])),
        ("EOI", before_frame(&[0xFF, 0xD9])),
        ("SOS", before_frame(&[0xFF, 0xDA, 0, 2])),
        ("a length of 1", before_frame(&[0xFF, 0xE0, 0, 1])),
        ("a length of 0", before_frame(&[0xFF, 0xE0, 0, 0])),
        (
            "a segment past the end",
            before_frame(&[0xFF, 0xE0, 0xFF, 0xFF]),
        ),
        (
            "the file ends in the frame",
            good[..good.len() - 2].to_vec(),
        ),
        ("the file ends in a marker", vec![0xFF, 0xD8, 0xFF]),
        ("only the SOI", vec![0xFF, 0xD8]),
    ] {
        assert!(gives_up(&bytes), "{what}");
    }
    let sof = |payload: &[u8]| {
        let mut out = vec![0xFF, 0xD8];
        segment(&mut out, 0xC0, payload);
        out
    };
    for (what, payload) in [
        (
            "a height of 0 (set later by DNL)",
            vec![8, 0, 0, 0, 16, 1, 1, 0x11, 0],
        ),
        ("a width of 0", vec![8, 0, 16, 0, 0, 1, 1, 0x11, 0]),
        ("no components", vec![8, 0, 16, 0, 16, 0]),
        (
            "five components",
            [&[8, 0, 16, 0, 16, 5][..], &[1; 15]].concat(),
        ),
        (
            "a length that is not its components'",
            vec![8, 0, 16, 0, 16, 3, 1, 0x11, 0],
        ),
        (
            "a frame longer than any",
            [&[8, 0, 16, 0, 16, 1][..], &[0; 800]].concat(),
        ),
    ] {
        assert!(gives_up(&sof(&payload)), "{what}");
    }
    // DHT, JPG and DAC carry codes in the frame's range and are not frames.
    for code in [0xC4, 0xC8, 0xCC] {
        let mut out = vec![0xFF, 0xD8];
        segment(&mut out, code, &[0; 6]);
        out.extend_from_slice(&good[2..]);
        assert_eq!(header_of(&out).need, 256, "{code:#X} is skipped");
    }
}

/// PNG, GIF, BMP, WebP and TIFF give their size and the bytes a decode holds from one block; a
/// TIFF whose first IFD is at the end of the file costs one more read, wherever it points.
#[test]
fn the_other_formats_give_their_header_from_the_blocks_it_is_in() {
    let picture = busy(300, 200);
    for (format, need) in [
        (ImageFormat::Png, 300 * 200 * 3),
        (ImageFormat::Gif, 300 * 200 * 4),
        (ImageFormat::Bmp, 300 * 200 * 3),
        (ImageFormat::WebP, 300 * 200 * 3),
        (ImageFormat::Tiff, 300 * 200 * 3),
    ] {
        let mut bytes = Vec::new();
        picture
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        let size = bytes.len() as u64;
        let mut asked = Vec::new();
        let header = {
            let mut head = Head::new(size, probe::HEADER_CAP, |off, buf: &mut [u8]| {
                asked.push(off);
                let from = off as usize;
                let n = buf.len().min(bytes.len() - from);
                buf[..n].copy_from_slice(&bytes[from..from + n]);
                Ok(n)
            });
            match probe::probe(&mut head).unwrap() {
                Probe::Header(header) => header,
                other => panic!("{format:?}: {other:?}"),
            }
        };
        assert_eq!((header.width, header.height), (300, 200), "{format:?}");
        assert_eq!(header.need, need, "{format:?}");
        assert_eq!(header.format, format);
        let whole = Doc::decode(&bytes).unwrap();
        assert_eq!(
            u64::from(whole.img.color().bytes_per_pixel()) * 300 * 200,
            need
        );
        if format == ImageFormat::Tiff {
            // The image crate writes the IFD after the pixels: one more read, where it points.
            assert!(size > 2 * BLOCK, "{size}");
            assert_eq!(asked, [0, (size - 1) / BLOCK * BLOCK], "{format:?}");
        } else {
            assert_eq!(asked, [0], "{format:?}");
        }
    }
}

/// The header read never fetches more than its cap, whatever the file says: segments that go
/// on for gigabytes, and a TIFF whose offsets send the reader all over a large file.
#[test]
fn the_header_read_stops_at_its_cap() {
    let fetched_of = |size: u64, at: &dyn Fn(u64) -> u8| -> (Probe, u64, usize) {
        let mut reads = 0;
        let (found, fetched) = {
            let mut head = Head::new(size, probe::HEADER_CAP, |off, buf: &mut [u8]| {
                reads += 1;
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = at(off + i as u64);
                }
                Ok(buf.len())
            });
            let found = probe::probe(&mut head).unwrap();
            (found, head.fetched())
        };
        (found, fetched, reads)
    };
    // A JPEG of nothing but 4-byte segments, for 24 GB.
    let (found, fetched, reads) = fetched_of(24_000_000_000, &|off| match off {
        0 => 0xFF,
        1 => 0xD8,
        _ => [0xFF, 0xE3, 0x00, 0x02][((off - 2) % 4) as usize],
    });
    assert!(matches!(found, Probe::Unknown), "{found:?}");
    assert_eq!((fetched, reads), (probe::HEADER_CAP, 16));
    // Nothing but fill bytes.
    let (found, fetched, _) = fetched_of(24_000_000_000, &|off| if off == 1 { 0xD8 } else { 0xFF });
    assert!(matches!(found, Probe::Unknown) && fetched <= probe::HEADER_CAP);
    // A TIFF whose first IFD is 2 GB in, and is all offsets.
    let (found, fetched, _) = fetched_of(24_000_000_000, &|off| match off {
        0 | 1 => b'I',
        2 => 42,
        3 => 0,
        4..8 => [0x00, 0x00, 0xFF, 0x7F][(off - 4) as usize],
        _ => 0xEE,
    });
    assert!(fetched <= probe::HEADER_CAP, "{found:?}: {fetched}");
    assert!(!matches!(found, Probe::Header(_)), "{found:?}");
}

/// A read that fails under the header read is the stream's failure, not the file's: it is
/// handed up as it is, and nothing is concluded about the file.
#[test]
fn a_failed_read_under_the_header_is_not_the_files_fault() {
    // A JPEG segment, and a PNG chunk, that run into the next block — which fails.
    let jpeg = b"\xFF\xD8\xFF\xE0\xFF\xFF".to_vec();
    let mut png = png_head(64, 64, false);
    png.extend_from_slice(b"\0\x02\0\0iCCP");
    for start in [jpeg, png] {
        let mut head = Head::new(10 * MIB, probe::HEADER_CAP, |off, buf: &mut [u8]| {
            if off > 0 {
                return Err(std::io::Error::other("the connection was lost"));
            }
            buf.fill(0);
            buf[..start.len()].copy_from_slice(&start);
            Ok(buf.len())
        });
        match probe::probe(&mut head) {
            Err(e) => assert_eq!(e.to_string(), "the connection was lost"),
            Ok(found) => panic!("{found:?}"),
        }
    }
}

/// The memory line's words (06 §7.3), and where it is due.
#[test]
fn the_memory_line_says_what_is_needed_and_what_the_machine_has() {
    let ask = Ask::Decode {
        need: 40_000_000_000,
        size: (100_000, 100_000),
    };
    assert_eq!(
        ask.line(Some(16 << 30)),
        "decoding this image needs 37.3 GB of memory (100000×100000); this machine has 16 GB"
    );
    assert!(ask.exceeds(Some(16 << 30)) && !ask.exceeds(Some(64 << 30)) && !ask.exceeds(None));
    assert_eq!(probe::memory_text(600 << 20), "600 MB");
    assert_eq!(probe::memory_text(1536 << 20), "1.5 GB");
    // Up to 512 MiB the window goes on.
    assert_eq!(probe::ask_decode(512 << 20, (1, 1), probe::BUDGET), None);
    assert_eq!(
        probe::ask_decode((512 << 20) + 1, (2, 3), probe::BUDGET),
        Some(Ask::Decode {
            need: (512 << 20) + 1,
            size: (2, 3)
        })
    );
    // Both over the budget: one line for what is held at once, and its colour by the sum.
    let both = probe::ask_open(3 << 30, Some((2 << 30, (9, 9))), probe::BUDGET).unwrap();
    assert_eq!(
        both.line(Some(4 << 30)),
        "reading and decoding this image needs 5 GB of memory (9×9; the file itself 3 GB); this \
         machine has 4 GB"
    );
    assert!(both.exceeds(Some(4 << 30)) && !both.exceeds(Some(5 << 30)));
    // One of them over it: that one's line.
    let decode = probe::ask_open(1 << 20, Some((2 << 30, (9, 9))), probe::BUDGET);
    assert!(matches!(decode, Some(Ask::Decode { .. })));
    let read = probe::ask_open(3 << 30, Some((1 << 20, (9, 9))), probe::BUDGET);
    assert!(matches!(read, Some(Ask::Read { .. })));
    assert!(matches!(
        probe::ask_open(3 << 30, None, probe::BUDGET),
        Some(Ask::Read { .. })
    ));
    assert_eq!(
        probe::ask_open(1 << 20, Some((1 << 20, (9, 9))), probe::BUDGET),
        None
    );
    assert_eq!(probe::ask_read(512 << 20, probe::BUDGET), None);
    assert!(probe::ask_read((512 << 20) + 1, probe::BUDGET).is_some());
    // This machine says how much it has.
    assert!(probe::physical_memory().is_some_and(|bytes| bytes >= 1 << 30));
    assert_eq!(probe::size_text(16_600_000), "16.6 MB");
    assert_eq!(probe::size_text(74), "74 bytes");
    assert_eq!(probe::size_text(24_000_000_000), "24.0 GB");
}

/// 06 §7.3: the decode's buffer is asked for with a check, in one piece of exactly the decoded
/// size, and a refusal is an error line — in place of `DynamicImage::from_decoder`, whose
/// result it gives for every pixel type the formats produce.
#[test]
fn the_decode_reserves_its_buffer_and_gives_what_from_decoder_gives() {
    use image::{Luma, LumaA, Rgb, Rgba};
    fn made<P: Pixel>(at: impl Fn(u32, u32) -> P) -> ImageBuffer<P, Vec<P::Subpixel>> {
        ImageBuffer::from_fn(9, 7, at)
    }
    let images = [
        DynamicImage::ImageLuma8(made(|x, y| Luma([(x * 9 + y) as u8]))),
        DynamicImage::ImageLumaA8(made(|x, y| LumaA([x as u8, y as u8]))),
        DynamicImage::ImageRgb8(made(|x, y| Rgb([x as u8, y as u8, 3]))),
        DynamicImage::ImageRgba8(made(|x, y| Rgba([x as u8, y as u8, 3, 200]))),
        DynamicImage::ImageLuma16(made(|x, y| Luma([(x * 5000 + y) as u16]))),
        DynamicImage::ImageLumaA16(made(|x, y| LumaA([x as u16 * 999, y as u16]))),
        DynamicImage::ImageRgb16(made(|x, y| Rgb([x as u16 * 777, y as u16, 3]))),
        DynamicImage::ImageRgba16(made(|x, y| Rgba([x as u16 * 777, y as u16, 3, 60_000]))),
        DynamicImage::ImageRgb32F(made(|x, y| Rgb([x as f32 / 9.0, y as f32, 0.25]))),
        DynamicImage::ImageRgba32F(made(|x, y| Rgba([x as f32 / 9.0, y as f32, 0.25, 0.5]))),
    ];
    for image in images {
        let format = match image.color() {
            image::ColorType::Rgb32F | image::ColorType::Rgba32F => ImageFormat::Tiff,
            _ => ImageFormat::Png,
        };
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        let decoder = |bytes: &[u8]| {
            ImageReader::with_format(Cursor::new(bytes.to_vec()), format)
                .into_decoder()
                .unwrap()
        };
        let theirs = DynamicImage::from_decoder(decoder(&bytes)).unwrap();
        let ours = probe::decode_reserved(decoder(&bytes)).unwrap();
        assert_eq!(ours.color(), image.color());
        assert_eq!(ours, theirs, "{:?}", image.color());
        assert_eq!(ours.as_bytes(), image.as_bytes(), "{:?}", image.color());
    }
    // `Doc::decode` goes through it: the largest reservation is the decoded size.
    std::thread::spawn(|| {
        let doc = Doc::decode(&png(300, 200, false)).unwrap();
        assert_eq!(doc.size(), (300, 200));
        assert_eq!(probe::largest_reserved(), 300 * 200 * 3);
    })
    .join()
    .unwrap();
    // What the system refuses is an error, said with its size: not an abort.
    let refused = probe::reserved::<u16>(usize::MAX / 4).expect_err("refused");
    assert!(
        format!("{refused}").starts_with("not enough memory: the system refused "),
        "{refused}"
    );
    // And `allow` is asked before it: its error is the decode's, and nothing is reserved.
    std::thread::spawn(|| {
        let asked = std::cell::Cell::new((0, (0, 0)));
        let refused = Doc::decode_asking(&png(300, 200, false), &|| Ok(()), &|need, size| {
            asked.set((need, size));
            bail!("not now")
        });
        assert_eq!(format!("{:#}", refused.err().unwrap()), "not now");
        assert_eq!(asked.get(), (180_000, (300, 200)));
        assert_eq!(probe::largest_reserved(), 0);
    })
    .join()
    .unwrap();
}

/// 06 §7.3: pictures of the bytes so far are made one at a time, off the read — the read does
/// not wait for one, and a mark reached while one is being made is skipped.
#[test]
fn the_read_does_not_wait_for_a_picture_and_marks_reached_meanwhile_are_skipped() {
    let file = png(1500, 1000, false);
    let size = file.len() as u64;
    let mut fx = fx(file.clone());
    fx.win.load.hold_sketch.store(true, Ordering::SeqCst);
    // The first picture is held before it is made: the file arrives whole all the same.
    fx.until("the whole file", |fx| fx.loaded() == size);
    assert!(fx.win.load.progress().previewing, "the picture at 25 %");
    assert!(fx.win.image.is_none() && fx.partial().is_none());
    fx.win.load.hold_sketch.store(false, Ordering::SeqCst);
    fx.until("the image", |fx| fx.win.image.is_some());
    assert_eq!(
        fx.win.load.progress().partials,
        1,
        "50 % and 75 % were skipped"
    );
    assert_eq!(fx.image().file.lock().unwrap().original(), file.as_slice());
    // One at a time, each mark gets its picture: the link serves the file up to the next
    // mark only once the picture of the mark before is counted and its worker is done.
    let mut fx = fx_of(FakeFile::bytes(file), &Hooks::default(), probe::BUDGET);
    let step = READ_STEP as u64;
    let marks: Vec<u64> = PREVIEW_MARKS
        .iter()
        .map(|percent| (size * percent).div_ceil(100).div_ceil(step) * step)
        .collect();
    fx.run(
        "the image",
        |fx| {
            let progress = fx.win.load.progress();
            let stop = marks.get(progress.partials).copied().unwrap_or(u64::MAX);
            !progress.previewing && fx.serves_below(stop)
        },
        |fx| fx.win.image.is_some(),
    );
    assert_eq!(fx.win.load.progress().partials, 3);
}

/// `SOPEN_AUTOSAVE` has no window to show a wait in and nobody to end it: a stream that fails
/// — a read answered with an error, or the connection lost — ends the run with the stream's
/// words, at once: it does not wait for a reconnect (the stream makes one a second later at
/// the earliest, and here the link's clock never gets that far).
#[test]
fn a_run_without_a_window_fails_on_a_failed_stream_instead_of_waiting() {
    for (fault, words) in [
        (Fault::Lost, "the connection closed"),
        (Fault::Status("the disk is on fire"), "the disk is on fire"),
    ] {
        let mut fx = fx(png(1200, 900, false));
        // The window's own loader is not what is tested: it is stopped.
        fx.win.stop();
        fx.until("the window's loader ends", |fx| fx.workers_ended());
        // The read's second request is the one that fails.
        let faulted = fx.requests() + 1;
        fx.control.fault(faulted, fault);
        let (tx, rx) = mpsc::channel();
        let source = fx.source.clone();
        std::thread::spawn(move || {
            let done = load_input(Input::Stream(source), &Load::windowless());
            let _ = tx.send(done.map(|_| ()).map_err(|e| format!("{e:#}")));
        });
        let started = Instant::now();
        let done = loop {
            fx.reply();
            match rx.recv_timeout(Duration::from_millis(2)) {
                Ok(done) => break done,
                Err(_) => assert!(
                    started.elapsed() < HANG / 4,
                    "{words}: the run waits for the stream"
                ),
            }
        };
        let why = done.expect_err(words);
        assert!(fx.requests() > faulted, "the faulted request was sent");
        assert!(why.contains(words), "{words}: {why}");
        // No reconnect was waited for: the link's clock is short of the first retry.
        assert!(
            fx.control.elapsed() < crate::stream::RETRY_EVERY,
            "{words}: {:?} passed on the link",
            fx.control.elapsed()
        );
    }
}

/// A decode nobody is asked about keeps the image crate's own allocation limit, as before:
/// only the asking path — the one with the memory line and Continue — lifts it. The TIFF
/// decoder is the one that holds its buffer to that limit; here the limit is a small one, and
/// a small TIFF crosses it (the real one is 512 MiB, and a 14000² page).
#[test]
fn only_a_decode_that_asks_lifts_the_crates_limit() {
    // The budget is the crate's own default: what `decode` and `decode_checked` are held to.
    assert_eq!(image::Limits::default().max_alloc, Some(probe::BUDGET));
    let mut tiff = Vec::new();
    busy(1200, 1200)
        .write_to(&mut Cursor::new(&mut tiff), ImageFormat::Tiff)
        .unwrap();
    // A limit the page is over, with room for what the decoder reads a strip into.
    let (need, limit) = (1200 * 1200 * 3, 2 << 20);
    let free = || Ok(());
    // Nobody asked (`Doc::decode`, `Doc::decode_checked`): refused at the limit.
    let unasked = Doc::decode_from(&tiff, &free, None, limit)
        .map(|doc| doc.size())
        .expect_err("over the limit");
    assert!(
        format!("{unasked:#}").contains("Memory limit exceeded"),
        "{unasked:#}"
    );
    // Asked and allowed: decoded.
    let asked = std::cell::Cell::new(0);
    let allow = |need: u64, _: (u32, u32)| {
        asked.set(need);
        Ok(())
    };
    let doc = Doc::decode_from(&tiff, &free, Some(&allow), limit).unwrap();
    assert_eq!((doc.size(), asked.get()), ((1200, 1200), need));
    // Within the limit nothing changes either way.
    assert!(Doc::decode_from(&tiff, &free, None, 2 * need).is_ok());
    assert!(Doc::decode(&tiff).is_ok() && Doc::decode_checked(&tiff, &free).is_ok());
}
