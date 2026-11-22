//! Slice S1's scenarios (02 §8.1's table) through the browser's own window, with real egui key
//! and pointer events: preview mode and the committed display (02 P1–P7, P12, P13 and every
//! exit of 02 §4.4's table), the F-key row and the key table (04 K-R1–K-R3), selection (K-S1)
//! and rename, copy and move through the real engine (K-C1–K-C5, K-M1, K-M2, K-N1, K-E1).
//!
//! Time is the test kit's: each pass is 1/60 s of egui's clock, which the 150 ms commit reads —
//! [`Fx::rest`] runs enough passes to pass it. Workers are waited for by what they do, never by
//! a sleep of a chosen length.

use super::*;
use crate::fileops::{Item, ItemOutcome, Progress, Report};
use crate::gui::testkit::command;
use crate::remote::SftpRemote;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

/// Passes past the 150 ms commit (200 ms of the test kit's clock).
const REST: usize = 12;

impl Fx {
    /// The bar rests: the display commits and what it shows is made ready.
    fn rest(&mut self) {
        self.drv.idle(&mut self.b, REST);
        self.settle();
        self.drv.idle(&mut self.b, 2);
    }

    /// The bar on `name`, rested.
    fn bar_on(&mut self, name: &str) {
        self.select(name);
        self.rest();
    }

    /// The right panel lists the folder `sub` of the root.
    fn other_into(&mut self, sub: &str) -> String {
        let dir = format!("{}/{sub}", self.root);
        self.b.list(Some(1), dir.clone(), None, None);
        self.settle();
        dir
    }

    /// Both panels list the root again (after the test changed it on disk).
    fn relist(&mut self) {
        let root = self.root.clone();
        self.b.list(None, root, None, None);
        self.settle();
    }

    fn with(&mut self, key: Key, modifiers: Modifiers) {
        self.drv.key(&mut self.b, key, modifiers);
    }

    /// `text` is painted in the right half of the window.
    fn on_right(&self, text: &str) -> bool {
        self.drv
            .texts_starting_with(text)
            .iter()
            .any(|(r, t)| t == text && r.left() >= 500.0)
    }

    /// `text` is laid out somewhere in the window, whole or scrolled (a long path in a field).
    fn laid_out(&self, text: &str) -> bool {
        self.drv.galleys().iter().any(|(_, g)| g.text() == text)
    }

    /// The first painted text starting with `prefix` in the right half.
    fn right_text(&self, prefix: &str) -> Option<String> {
        self.drv
            .texts_starting_with(prefix)
            .into_iter()
            .find(|(r, _)| r.left() >= 500.0)
            .map(|(_, text)| text)
    }

    fn label(&self, label: &str) -> bool {
        self.drv.find_text(&format!("{label:<7}")).is_some()
    }

    /// Passes until `done` (at most 20 s), then two more. After a file operation, wait for the
    /// panels' re-read too (`listing.is_empty()`): the list takes no key while one is listed.
    fn wait(&mut self, done: impl FnMut(&Browser) -> bool) {
        self.drv.until(&mut self.b, 20, done);
        self.drv.idle(&mut self.b, 2);
    }

    fn status(&self) -> String {
        self.b.status_line().text
    }
}

/// What the watched connections were asked, over all of them.
#[derive(Default)]
struct Seen {
    connects: usize,
    /// The files `read_to` was started on, in order, and the connection each was on.
    reads: Vec<String>,
    read_on: Vec<usize>,
    /// Bytes the reads handed on, over all of them.
    bytes: u64,
    live: usize,
    peak: usize,
    /// The files opened as a stream on a watched connection (06 §13), in order; the bytes
    /// their reads brought; and how often a stream gave its connection back.
    streams: Vec<String>,
    streamed: u64,
    given_back: usize,
    /// Streams that closed their transport for good.
    closed: usize,
}

type Shared = Arc<Mutex<Seen>>;

/// The local disk posing as a remote that counts its reads; a read of a file named `hold` stops
/// at the gate after its first chunk — a whole read's, or a stream's first reply with bytes
/// (on the stream's own thread: the preparation that waits for it still sees a cancel). With
/// `hold` `open:NAME` the preparation itself is held, on its own thread, where NAME's stream
/// is opened — a cancel meanwhile is seen only once it is let go.
struct Watching {
    fs: LocalFs,
    seen: Shared,
    gate: Gate,
    hold: &'static str,
    /// Which connection this is, counted from 1.
    id: usize,
}

impl RemoteFs for Watching {
    fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.fs.stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.fs.lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.fs.realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.fs.read_head(p, max)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.fs.list_dir(p)
    }
    fn list_entries(&mut self, p: &str) -> Result<Vec<crate::remote::Entry>> {
        self.fs.list_entries(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        {
            let mut seen = self.seen.lock().unwrap();
            seen.reads.push(p.to_string());
            seen.read_on.push(self.id);
            seen.live += 1;
            seen.peak = seen.peak.max(seen.live);
        }
        let held = !self.hold.is_empty() && p.ends_with(self.hold);
        let read = (|| -> Result<u64> {
            let bytes = std::fs::read(p)?;
            let mut done = 0u64;
            for (i, chunk) in bytes.chunks(64 * 1024).enumerate() {
                out.write_all(chunk)?;
                done += chunk.len() as u64;
                self.seen.lock().unwrap().bytes += chunk.len() as u64;
                progress(done);
                if i == 0 && held {
                    self.gate.hold();
                }
            }
            Ok(done)
        })();
        self.seen.lock().unwrap().live -= 1;
        read
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.fs.create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.fs.set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.fs.link_new(from, to)
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.fs.remove(p)
    }
    /// The connection becomes a stream's transport (06 §2), and comes back as a watched one.
    fn into_ranged(
        self: Box<Self>,
        path: &str,
    ) -> std::result::Result<Box<dyn crate::remote::Ranged>, crate::remote::Refused> {
        let Watching {
            fs,
            seen,
            gate,
            hold,
            id,
        } = *self;
        seen.lock().unwrap().streams.push(path.to_string());
        if hold
            .strip_prefix("open:")
            .is_some_and(|name| path.ends_with(name))
        {
            gate.hold();
        }
        match Box::new(fs).into_ranged(path) {
            Ok(ranged) => {
                // An open stream is activity on the session, as a read under way is.
                {
                    let mut seen = seen.lock().unwrap();
                    seen.live += 1;
                    seen.peak = seen.peak.max(seen.live);
                }
                Ok(Box::new(WatchedStream {
                    inner: Some(ranged),
                    seen,
                    gate,
                    hold,
                    id,
                    held: false,
                }))
            }
            Err(refused) => Err(crate::remote::Refused {
                conn: Box::new(Watching {
                    fs: LocalFs::new(),
                    seen,
                    gate,
                    hold,
                    id,
                }),
                error: refused.error,
            }),
        }
    }
}

/// A watched connection while a stream reads on it: its reads' bytes are counted
/// ([`Seen::streamed`]), the stream of a file named `hold` stops at the gate at its first reply
/// with bytes, and it is the same watched connection again when the stream gives it back.
struct WatchedStream {
    inner: Option<Box<dyn crate::remote::Ranged>>,
    seen: Shared,
    gate: Gate,
    hold: &'static str,
    id: usize,
    /// It stopped at the gate once already.
    held: bool,
}

impl Drop for WatchedStream {
    fn drop(&mut self) {
        self.seen.lock().unwrap().live -= 1;
    }
}

impl WatchedStream {
    fn inner(&self) -> &dyn crate::remote::Ranged {
        self.inner.as_ref().expect("a live transport").as_ref()
    }

    fn inner_mut(&mut self) -> &mut Box<dyn crate::remote::Ranged> {
        self.inner.as_mut().expect("a live transport")
    }

    fn into_watching(mut self) -> Watching {
        drop(
            self.inner
                .take()
                .expect("a live transport")
                .into_connection(),
        );
        Watching {
            fs: LocalFs::new(),
            seen: self.seen.clone(),
            gate: self.gate.clone(),
            hold: self.hold,
            id: self.id,
        }
    }
}

impl crate::remote::Ranged for WatchedStream {
    fn path(&self) -> &str {
        self.inner().path()
    }
    fn opened(&self) -> Stat {
        self.inner().opened()
    }
    fn read(&mut self, off: u64, len: u32) -> crate::remote::Ticket {
        self.inner_mut().read(off, len)
    }
    fn fstat(&mut self) -> crate::remote::Ticket {
        self.inner_mut().fstat()
    }
    fn stat_path(&mut self) -> crate::remote::Ticket {
        self.inner_mut().stat_path()
    }
    fn pending(&self) -> usize {
        self.inner().pending()
    }
    fn next(&mut self, timeout: Option<Duration>) -> Option<crate::remote::Completion> {
        let c = self.inner_mut().next(timeout)?;
        let n = c.data().map_or(0, |b| b.len() as u64);
        self.seen.lock().unwrap().streamed += n;
        let held = !self.hold.is_empty()
            && !self.hold.starts_with("open:")
            && self.path().ends_with(self.hold);
        if n > 0 && held && !std::mem::replace(&mut self.held, true) {
            self.gate.hold();
        }
        Some(c)
    }
    fn waker(&self) -> crate::remote::Wake {
        self.inner().waker()
    }
    fn now(&self) -> std::time::Instant {
        self.inner().now()
    }
    fn reopen(
        self: Box<Self>,
        path: &str,
    ) -> std::result::Result<Box<dyn crate::remote::Ranged>, crate::remote::Refused> {
        Box::new(self.into_watching()).into_ranged(path)
    }
    fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        self.seen.lock().unwrap().given_back += 1;
        Box::new(self.into_watching())
    }
    fn close(mut self: Box<Self>) {
        self.seen.lock().unwrap().closed += 1;
        if let Some(inner) = self.inner.take() {
            inner.close();
        }
    }
}

fn watching(seen: &Shared, gate: &Gate, hold: &'static str) -> Connect {
    let (seen, gate) = (seen.clone(), gate.clone());
    Arc::new(move || {
        let id = {
            let mut seen = seen.lock().unwrap();
            seen.connects += 1;
            seen.connects
        };
        Ok(Box::new(Watching {
            fs: LocalFs::new(),
            seen: seen.clone(),
            gate: gate.clone(),
            hold,
            id,
        }) as Box<dyn RemoteFs + Send>)
    })
}

/// The tree "on host box" over watched connections.
fn watched(hold: &'static str) -> (Fx, Shared, Gate) {
    let (seen, gate) = (Shared::default(), Gate::default());
    let fx = fixture_with(Some("box"), watching(&seen, &gate, hold), None);
    (fx, seen, gate)
}

/// Every file under `dir`, by its path below it.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.push(path.strip_prefix(base).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// What is at `path` itself (never what a link points at): its type, inode, mode, size and
/// time, and its bytes (a file) or its target (a link).
fn entry_state(path: &Path) -> String {
    let m = std::fs::symlink_metadata(path).unwrap();
    let t = m.file_type();
    let what = if t.is_symlink() {
        format!("link to {}", std::fs::read_link(path).unwrap().display())
    } else if t.is_dir() {
        "folder".to_string()
    } else if t.is_file() {
        format!("file {:?}", std::fs::read(path).unwrap())
    } else {
        "special".to_string()
    };
    format!(
        "{what} ino={} mode={:o} size={} mtime={}.{}",
        m.ino(),
        m.mode(),
        m.len(),
        m.mtime(),
        m.mtime_nsec()
    )
}

/// [`entry_state`] of `dir` and of everything under it, by path.
fn tree_state(dir: &Path) -> Vec<(String, String)> {
    fn walk(at: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let name = at.strip_prefix(base).unwrap().display().to_string();
        out.push((name, entry_state(at)));
        if std::fs::symlink_metadata(at).unwrap().is_dir() {
            for entry in std::fs::read_dir(at).unwrap().flatten() {
                walk(&entry.path(), base, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// A picture of about 2 MiB at `big.png` (noise: it does not compress) — a file a preview reads
/// whole, through its stream (its decode needs every byte; text and binaries read what they
/// show, 06 §13). Its size.
fn big_picture(fx: &Fx) -> u64 {
    let mut seed = 0x2545_f491_u32;
    let img = image::RgbImage::from_fn(900, 800, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let [a, b, c, _] = seed.to_le_bytes();
        image::Rgb([a, b, c])
    });
    let path = format!("{}/big.png", fx.root);
    img.save(&path).unwrap();
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(size > 1 << 20, "{size}");
    size
}

// ---------------------------------------------------------------------------------------------
// 02 §4.5: the preview.

/// P1: the window opens in preview mode, the bar on `..`, slot 9 reading `Dual`; the bar resting
/// on a file shows it on the other side — text, an image, hex rows, a video's first frame with
/// its size and duration, a sound's waveform with its duration (S1-M; a file that is not the
/// video its name says is its info card) — in a single-lined frame titled with the file; the
/// side's own list is back when the bar rests on a folder.
#[test]
fn p1_the_other_side_previews_the_file_under_the_bar() {
    let mut fx = fixture();
    let tools = media_tools();
    std::fs::write(format!("{}/broken.mp4", fx.root), b"not really a video").unwrap();
    if let Some(tools) = &tools {
        clip(
            tools,
            Path::new(&format!("{}/clip.mp4", fx.root)),
            (160, 120),
        );
        tone(tools, Path::new(&format!("{}/tone.wav", fx.root)), 3);
    }
    fx.relist();
    assert_eq!(fx.current(), "..");
    assert!(fx.label("Dual"), "slot 9 names the mode F9 switches to");
    assert!(fx.on_right("alpha"), "on .. the other side is its own list");

    fx.bar_on("notes.txt");
    assert!(fx.on_right("plain notes"), "the text, on the other side");
    assert!(!fx.on_right("alpha"), "in place of that side's list");
    let title = fx.framed(&format!("{}/notes.txt", fx.root));
    assert!(fx.on_right(&title), "the side's frame names the file");
    assert!(fx.b.viewer.is_none(), "a preview, not a viewer");
    assert_eq!(fx.b.active, 0, "the keyboard stays in the list");

    fx.bar_on("main.rs");
    let colors = fx.drv.colors_of("fn main() {").expect("the code");
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert!(distinct.len() >= 2, "highlighted: {colors:?}");
    assert!(fx.on_right("fn main() {"));

    fx.bar_on("photo.png");
    let pictures = fx.drv.pictures();
    assert!(
        pictures.iter().any(|r| r.left() >= 500.0),
        "the picture, on the other side: {pictures:?}"
    );

    fx.bar_on("data.bin");
    // The hex rows start in the side (sixteen bytes a row are wider than half a window: the
    // row's end is cut by the side's frame, so it is looked for among everything laid out).
    let rows = fx.drv.texts();
    assert!(
        rows.iter()
            .any(|(r, _, t)| t.starts_with("00 01 02 03") && r.left() >= 500.0),
        "the hex rows, on the other side"
    );
    assert!(fx.on_right("0000(0000)"), "the first row's offset");

    if tools.is_some() {
        fx.bar_on("clip.mp4");
        let pictures = fx.drv.pictures();
        assert!(
            pictures
                .iter()
                .any(|r| r.left() >= 500.0 && r.width() > 160.0),
            "the video's frame, fitted in the other side: {pictures:?}"
        );
        let said = fx.right_text("160×120").expect("the video's size");
        assert!(said.contains("0:02.00"), "and its duration: {said}");
        assert!(fx.b.viewer.is_none(), "a preview, not a viewer");
        assert!(fx.drv.find_text("Play").is_none(), "without its bars");

        fx.bar_on("tone.wav");
        let said = fx.right_text("audio · mono").expect("the sound's line");
        assert!(said.contains("0:03.00"), "its whole duration: {said}");
        assert!(!fx.on_right("File Info"), "the waveform, not a card");
        assert!(fx.drv.find_text("Play").is_none(), "without its bars");

        fx.bar_on("broken.mp4");
        assert!(fx.on_right("File Info"), "not a video: its info card");
        let why = fx.right_text("ffprobe cannot read").expect("why");
        assert!(why.contains("broken.mp4"), "{why}");
    }

    fx.bar_on("beta");
    assert!(fx.on_right("alpha"), "on a folder: the side's own list");
    assert!(!fx.on_right("File Info"));
}

/// P2: the bar swept quickly over files prepares nothing for the ones passed; one preparation,
/// on one connection, for the file it rests on.
#[test]
fn p2_a_quick_sweep_prepares_only_the_file_the_bar_rests_on() {
    let (mut fx, seen, _gate) = watched("");
    fx.key(Key::ArrowDown);
    fx.key(Key::ArrowDown);
    // .hidden, data.bin, main.rs, notes.txt, photo.png: each passed in 50 ms.
    for _ in 0..5 {
        fx.key(Key::ArrowDown);
    }
    assert_eq!(fx.current(), "photo.png");
    assert!(
        seen.lock().unwrap().streams.is_empty(),
        "nothing is read while the bar moves"
    );
    fx.rest();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.streams, [format!("{}/photo.png", fx.root)]);
    assert!(seen.reads.is_empty(), "never read whole");
    assert_eq!(seen.connects, 1, "one preview connection");
    assert_eq!(fx.b.preview.spawned(), 1, "one preparation");
}

/// P3: a remote file over the read budget that would be read whole (an image) is its info card,
/// decided from the stat: nothing of it is read. With streaming (06 §13) the budget is no gate
/// on a file the hex window shows: a binary over it previews its first rows, one block read.
#[test]
fn p3_a_file_over_the_preview_limit_is_its_card_and_nothing_is_read() {
    let (mut fx, seen, _gate) = watched("");
    for name in ["huge.png", "huge.bin"] {
        let big = std::fs::File::create(format!("{}/{name}", fx.root)).unwrap();
        big.set_len(33 << 20).unwrap();
    }
    fx.relist();
    fx.bar_on("huge.png");
    assert!(fx.on_right("File Info"));
    assert_eq!(
        fx.right_text("over the preview limit").as_deref(),
        Some("over the preview limit: Enter opens it")
    );
    assert!(seen.lock().unwrap().reads.is_empty(), "nothing was read");
    assert!(seen.lock().unwrap().streams.is_empty());
    fx.bar_on("huge.bin");
    assert!(fx.on_right("00 00 00 00 00 00 00 00"), "its first rows");
    let seen = seen.lock().unwrap();
    assert!(seen.reads.is_empty(), "never read whole");
    assert_eq!(seen.streamed, crate::stream::BLOCK, "one block");
    assert!(files_under(&fx.cache).is_empty(), "nothing on disk");
}

/// P4: the bar leaves a preview in the middle of its read (an image, read whole through its
/// stream): the read is cancelled, the side keeps what it showed until the commit, the next
/// preparation starts only once the first worker has ended (never two at once, one session —
/// the cancelled stream gave it back), and nothing is on disk.
#[test]
fn p4_leaving_a_preview_mid_read_cancels_it_and_the_next_waits_its_turn() {
    // The preparation held where its stream opens: it cannot end before it is let go.
    let (mut fx, seen, gate) = watched("open:big.png");
    big_picture(&fx);
    fx.relist();
    fx.select("big.png");
    fx.drv.idle(&mut fx.b, REST);
    gate.reached();
    fx.drv.idle(&mut fx.b, 2);
    assert!(
        fx.right_text("reading big.png").is_some(),
        "the read's progress shows in the side"
    );
    // Away, and resting on another file while the first worker is still held in its read.
    fx.select("notes.txt");
    fx.drv.idle(&mut fx.b, REST);
    assert_eq!(
        fx.b.preview.spawned(),
        1,
        "the next preparation waits for the first worker's end"
    );
    assert_eq!(seen.lock().unwrap().streams.len(), 1);
    gate.release();
    fx.rest();
    assert!(fx.on_right("plain notes"), "then the next preview shows");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.peak, 1, "never two streams at once");
    assert_eq!(seen.connects, 1, "one preview session");
    assert_eq!(seen.given_back, 1, "the cancelled stream gave it back");
    assert_eq!(fx.b.preview.spawned(), 2);
    assert!(seen.reads.is_empty(), "never read whole");
    // The abandoned read stopped where it was let go — at its stream's open, before any of
    // its bytes: far short of the file's end. (notes.txt, read through its own stream on the
    // same session, brought one block.)
    assert!(
        seen.streamed < 256 * 1024 + crate::stream::BLOCK,
        "the cancelled read went on: {} of {} bytes",
        seen.streamed,
        2 << 20
    );
    assert_eq!(
        seen.streams,
        [
            format!("{}/big.png", fx.root),
            format!("{}/notes.txt", fx.root)
        ]
    );
    let kept = files_under(&fx.cache);
    assert!(kept.is_empty(), "a preview writes nothing: {kept:?}");
}

/// P5: clicks, a drag and the wheel over a preview change nothing — it is not a viewer, the
/// keyboard stays in the list — and the list's keys go on acting; the preview's frame is the
/// inactive one.
#[test]
fn p5_a_preview_takes_no_input_and_the_list_keeps_its_keys() {
    let mut fx = fixture();
    fx.bar_on("photo.png");
    let before = fx.drv.pictures();
    let centre = Pos2::new(750.0, 350.0);
    fx.drv.click(&mut fx.b, centre);
    fx.drv.drag(
        &mut fx.b,
        PointerButton::Primary,
        centre - Vec2::new(80.0, 60.0),
        centre + Vec2::new(80.0, 60.0),
    );
    fx.drv.wheel(&mut fx.b, centre, -5.0);
    fx.drv.idle(&mut fx.b, 2);
    assert_eq!(fx.drv.pictures(), before, "the picture did not move");
    assert!(fx.b.viewer.is_none());
    assert_eq!((fx.b.active, fx.current().as_str()), (0, "photo.png"));
    let title = fx.framed(&format!("{}/photo.png", fx.root));
    assert_eq!(
        fx.drv.colors_of(&title),
        Some(vec![chrome::PALETTE.frame_idle.color]),
        "the preview's frame is the inactive one"
    );
    // A text preview takes no typing either: the letter is the list's jump.
    fx.bar_on("notes.txt");
    fx.typed("z");
    assert_eq!(fx.current(), "zeta.md", "the list's keys still act");
    assert!(Window::close_requested(&mut fx.b), "nothing is asked");
}

/// P6: Enter on a previewed remote image hands the live preview over: the viewer is there at
/// once, the file is not read again (its one stream, read whole for the preview), and nothing
/// is claimed or written in the cache.
#[test]
fn p6_enter_on_a_previewed_file_hands_the_preview_over() {
    let (mut fx, seen, _gate) = watched("");
    fx.bar_on("photo.png");
    let streamed = seen.lock().unwrap().streamed;
    fx.key(Key::Enter);
    assert!(fx.b.viewer.is_some(), "the viewer is there at once");
    fx.settle();
    assert!(fx.label("Exit"), "a viewer now: its row");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.streams,
            [format!("{}/photo.png", fx.root)],
            "read once"
        );
        assert_eq!(seen.streamed, streamed, "nothing read again");
        assert!(seen.reads.is_empty(), "never read whole");
    }
    assert!(files_under(&fx.cache).is_empty(), "nothing claimed");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "photo.png");
}

/// P6: Enter while the preview still reads: the same read goes on as the viewer's opening — its
/// progress on the status row, no second read — and the viewer shows when it ends.
#[test]
fn p6_enter_during_a_previews_read_continues_that_read_as_the_opening() {
    let (mut fx, seen, gate) = watched("big.png");
    big_picture(&fx);
    fx.relist();
    fx.select("big.png");
    fx.drv.idle(&mut fx.b, REST);
    gate.reached();
    fx.key(Key::Enter);
    let shown = fx.status();
    assert!(
        shown.starts_with("fetching big.png ") && shown.ends_with("Esc cancels"),
        "{shown}"
    );
    gate.release();
    fx.wait(|b| b.viewer.is_some());
    fx.settle();
    assert_eq!(seen.lock().unwrap().streams.len(), 1, "the same read");
    assert!(seen.lock().unwrap().reads.is_empty(), "never read whole");
    assert_eq!(fx.b.preview.spawned(), 1, "the same worker");
    assert!(fx.label("Exit"), "the viewer's row");
    assert_eq!(fx.drv.pictures().len(), 1, "the file's viewer");
}

/// P6 for streamed previews (06 §13): Enter on a previewed remote text file, and on a remote
/// binary, hands the preview's own Source to the viewer. Across the Enter the window is the
/// same one, no stream is opened, no connection is made, no byte crosses and nothing is given
/// back: the viewer reads on what the preview had. The session went with it — closing the
/// viewer closes that transport for good, it is not returned to the window — so the next
/// preview opens a new preview session.
#[test]
fn p6_enter_on_a_streamed_preview_hands_its_source_over() {
    for (name, shows) in [
        ("notes.txt", "plain notes"),
        ("data.bin", "00 01 02 03 04 05 06 07"),
    ] {
        let (mut fx, seen, _gate) = watched("");
        fx.bar_on(name);
        assert!(fx.on_right(shows), "{name}: its preview");
        let previewed = fx.b.preview.shown_window().expect("a preview shows");
        let state = |seen: &Shared| {
            let seen = seen.lock().unwrap();
            (
                seen.connects,
                seen.streams.clone(),
                seen.streamed,
                seen.given_back,
                seen.closed,
                seen.reads.len(),
            )
        };
        let before = state(&seen);
        assert_eq!(before.0, 1, "{name}: the preview session");
        assert_eq!(before.1, vec![format!("{}/{name}", fx.root)]);
        assert!(before.2 > 0, "{name}: the preview read its bytes");
        fx.key(Key::Enter);
        assert!(fx.b.viewer.is_some(), "{name}: the viewer is there at once");
        fx.settle();
        fx.drv.idle(&mut fx.b, 5);
        assert!(fx.label("Exit"), "{name}: a viewer now, its row");
        let viewer = fx.b.viewer.as_ref().unwrap();
        let window = &*viewer.embedded.window as *const dyn Window as *const () as usize;
        assert_eq!(window, previewed, "{name}: the preview's own window");
        assert_eq!(
            state(&seen),
            before,
            "{name}: no stream opened, no connection, no byte, nothing given back"
        );
        assert_eq!(seen.lock().unwrap().peak, 1, "{name}: one session active");
        // The viewer closes: its transport closes with it, and is not given back.
        fx.key(Key::Escape);
        assert!(fx.b.viewer.is_none());
        until("the viewer's stream to close", || {
            seen.lock().unwrap().closed == 1
        });
        fx.rest();
        let after = state(&seen);
        assert_eq!(after.3, 0, "{name}: closed, not returned to the window");
        assert_eq!(
            after.0, 2,
            "{name}: the next preview opened a new preview session"
        );
        assert!(files_under(&fx.cache).is_empty());
    }
}

/// P7 and 04 K-R1: F9 switches to dual mode at once — both sides lists, slot 9 `Prevw` — and
/// back to preview mode, the preview there again.
#[test]
fn p7_f9_switches_between_preview_and_dual() {
    let mut fx = fixture();
    fx.bar_on("notes.txt");
    assert!(fx.on_right("plain notes"));
    fx.key(Key::F9);
    assert!(
        fx.label("Prevw") && !fx.label("Dual"),
        "slot 9 in dual mode"
    );
    assert!(fx.on_right("alpha"), "both sides are lists");
    assert!(!fx.on_right("plain notes"));
    fx.rest();
    assert!(fx.on_right("alpha"), "dual mode never previews");
    fx.key(Key::F9);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.label("Dual"));
    assert!(fx.on_right("plain notes"), "preview mode again");
}

/// `SOPEN_STATE` `loading=1` and a preview that has settled (06 §11): Enter opens a viewer of
/// its own, which is handed `loading` — the preview's window was made before anyone could
/// tell it. Without the key, Enter makes the preview's own window the viewer, as ever.
#[test]
fn enter_on_a_settled_preview_hands_loading_to_a_fresh_viewer() {
    for loading in [false, true] {
        let mut fx = fixture();
        fx.b.open_loading = loading;
        fx.bar_on("main.rs");
        assert!(fx.on_right("// a comment"), "loading {loading}: previewed");
        let previewed = fx.b.preview.shown_window().expect("the preview's window");
        assert_eq!(crate::gui::text::embedded_loading(), Some(false));
        fx.key(Key::Enter);
        fx.drv
            .until(&mut fx.b, 20, |b| b.viewer.is_some() && b.ready());
        let viewer = fx.b.viewer.as_ref().expect("the viewer");
        let window = &*viewer.embedded.window as *const dyn Window as *const () as usize;
        assert_eq!(
            window != previewed,
            loading,
            "loading {loading}: a viewer of its own only with loading=1"
        );
        assert_eq!(
            crate::gui::text::embedded_loading(),
            Some(loading),
            "loading {loading}: what the viewer was handed"
        );
        assert!(
            fx.drv.find_text("// a comment").is_some(),
            "loading {loading}"
        );
    }
}

/// P12 (text): a text file past the 4 MiB budget previews its start and says so; Enter makes the
/// preview the viewer, which reads on: End is the file's last line.
#[test]
fn p12_a_long_text_previews_its_first_4_mib_and_enter_reads_on() {
    let mut fx = fixture();
    let mut body = String::new();
    let mut n = 0;
    while body.len() < (6 << 20) {
        body.push_str(&format!(
            "row {n:07} of a long file, padded out to a width\n"
        ));
        n += 1;
    }
    body.push_str("the last line\n");
    std::fs::write(format!("{}/long.txt", fx.root), &body).unwrap();
    fx.relist();
    fx.bar_on("long.txt");
    assert!(fx.on_right("row 0000000 of a long file, padded out to a width"));
    assert_eq!(
        fx.right_text("preview of the first").as_deref(),
        Some("preview of the first 4 MiB")
    );
    fx.key(Key::Enter);
    assert!(fx.b.viewer.is_some(), "handed over at once");
    fx.settle();
    fx.with(Key::End, command(false));
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert!(
        fx.drv.find_text("the last line").is_some(),
        "the viewer read on to the end"
    );
}

/// P13: the side keeps what it shows while the bar has not rested — a preview of the file
/// before, or the list — then shows the new file's preview; Tab commits at once.
#[test]
fn p13_the_side_keeps_its_display_until_the_bar_has_rested() {
    let mut fx = fixture();
    // From a preview of notes.txt.
    fx.bar_on("notes.txt");
    fx.key(Key::ArrowDown);
    assert_eq!(fx.current(), "photo.png");
    assert!(
        fx.on_right("plain notes"),
        "notes.txt's preview stays until the bar has rested"
    );
    fx.rest();
    assert!(!fx.on_right("plain notes"));
    assert!(fx.drv.pictures().iter().any(|r| r.left() >= 500.0));
    // From the list (the bar on a folder).
    fx.bar_on("beta");
    fx.typed("p");
    assert_eq!(fx.current(), "photo.png");
    assert!(
        fx.on_right("alpha"),
        "the list stays until the bar has rested"
    );
    assert!(fx.drv.pictures().is_empty());
    fx.rest();
    assert!(fx.drv.pictures().iter().any(|r| r.left() >= 500.0));
    // Tab within the 150 ms commits at once: the side left shows its list, for the other
    // list's bar is on `..`.
    fx.key(Key::ArrowUp);
    assert_eq!(fx.current(), "notes.txt");
    fx.key(Key::Tab);
    assert_eq!(fx.b.active, 1);
    assert!(fx.drv.pictures().is_empty(), "committed at once");
    assert!(fx.on_right("alpha") && !fx.on_right("plain notes"));
}

/// 02 §4.4's exits not covered above: Tab gives the preview's side its list back, with the
/// keyboard, and the side left follows the new bar; F4 keeps the preview; Enter on a folder
/// drops it.
#[test]
fn a_preview_gives_way_to_tab_and_a_folder_and_stays_for_f4() {
    let mut fx = fixture();
    fx.bar_on("notes.txt");
    fx.key(Key::F4);
    fx.wait(|b| b.launches.is_empty());
    assert_eq!(
        fx.launched.lock().unwrap().as_slice(),
        [(format!("{}/notes.txt", fx.root), false)]
    );
    assert!(fx.on_right("plain notes"), "F4 keeps the preview");
    fx.key(Key::Tab);
    assert_eq!(fx.b.active, 1);
    assert!(
        fx.on_right("alpha"),
        "the side has its list, and the keyboard"
    );
    fx.select("main.rs");
    fx.rest();
    let code = fx.drv.find_text("fn main() {").expect("the left side");
    assert!(code.right() <= 500.0, "the side left follows the new bar");
    fx.key(Key::Tab);
    fx.select("alpha");
    fx.rest();
    fx.key(Key::Enter);
    fx.settle();
    assert_eq!(fx.b.panels[0].dir, format!("{}/alpha", fx.root));
    assert!(fx.on_right("beta"), "in the folder, on ..: the side's list");
}

/// 02 §4.4: Enter on a file that shows an info card opens it as Enter does from the list —
/// through a stream of its own, from the start (06 §13): here an image over the preview's 64
/// megapixels, whose card cost its header's block, opens in the image window at its memory line
/// (with Hex view), and nothing reaches the cache.
#[test]
fn enter_on_an_info_card_opens_the_file() {
    let (mut fx, seen, _gate) = watched("");
    // An image the preview refuses by its header (over 64 megapixels).
    let header = {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8; 4], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut crc_input = kind.to_vec();
            crc_input.extend_from_slice(data);
            out.extend_from_slice(&crc_input);
            let mut crc = 0xFFFF_FFFFu32;
            for &b in &crc_input {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        0xEDB8_8320 ^ (crc >> 1)
                    } else {
                        crc >> 1
                    };
                }
            }
            out.extend_from_slice(&(!crc).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&30000u32.to_be_bytes());
        ihdr.extend_from_slice(&30000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01]);
        chunk(b"IEND", &[]);
        out
    };
    let header_len = header.len() as u64;
    std::fs::write(format!("{}/wide.png", fx.root), header).unwrap();
    fx.relist();
    fx.bar_on("wide.png");
    assert_eq!(
        fx.right_text("image is").as_deref(),
        Some("image is 30000×30000; Enter opens it")
    );
    assert_eq!(
        seen.lock().unwrap().streamed,
        crate::stream::BLOCK.min(header_len),
        "the card cost its header's block"
    );
    fx.key(Key::Enter);
    fx.wait(|b| b.opening.is_none() && b.viewer.is_some());
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while fx
        .drv
        .text_starting_with("decoding this image needs")
        .is_none()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "no memory line ({})",
            fx.b.status.text
        );
        fx.drv.idle(&mut fx.b, 1);
    }
    let line = fx
        .drv
        .text_starting_with("decoding this image needs")
        .unwrap();
    assert!(line.contains("Hex view (H)"), "{line}");
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.connects, 2,
        "the opening's own connection, beside the preview's"
    );
    assert_eq!(seen.streams.len(), 2, "{:?}", seen.streams);
    assert!(seen.reads.is_empty(), "never read whole");
    assert!(files_under(&fx.cache).is_empty());
}

/// 02 §4.4: the window closes while a preview reads: the read is cancelled and the window does
/// not wait for its worker; the stream goes with the worker's end, and nothing is on disk.
#[test]
fn closing_the_window_does_not_wait_for_a_preview() {
    // Bounded from outside: a `finish` that waited for the held worker would never return.
    bounded(30, || {
        let (mut fx, seen, gate) = watched("big.png");
        big_picture(&fx);
        fx.relist();
        fx.select("big.png");
        fx.drv.idle(&mut fx.b, REST);
        gate.reached();
        assert!(Window::close_requested(&mut fx.b));
        let leftover = fx.b.leftover.clone();
        let cache = fx.cache.clone();
        let Fx { b, _tmp, .. } = fx;
        drop(b);
        // The worker is still held in its read: this returns all the same.
        leftover.borrow_mut().finish();
        gate.release();
        // Its stream goes with the worker's end.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while seen.lock().unwrap().live > 0 {
            assert!(std::time::Instant::now() < deadline, "the stream was left");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(files_under(&cache).is_empty());
        drop(_tmp);
    });
}

/// A cache that cannot be written is no hindrance to a preview — nothing of one is written to
/// disk (06 §13: a remote file reads through its stream, a local one in place) — and Enter still
/// opens the file.
#[test]
fn a_cache_that_cannot_be_written_does_not_stop_a_preview() {
    let mut fx = fixture();
    // The cache root is a file: nothing can be made in it.
    std::fs::write(&fx.cache, b"in the way").unwrap();
    fx.bar_on("notes.txt");
    assert!(fx.on_right("plain notes"), "the preview");
    assert!(fx.b.viewer.is_none());
    // Enter still opens the file (a local file is read in place: no cache is needed).
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.viewer.is_some(), "{}", fx.b.status.text);
    assert!(fx.drv.find_text("plain notes").is_some());
    assert!(fx.label("Exit"), "the viewer's row");
}

/// 02 §9: `mode=dual` opens the browser in dual mode and `bar=NAME` puts the bar on an entry
/// once listed, without opening it; `SOPEN_KEYS` plays keys once the window has settled.
#[test]
fn the_mode_bar_and_keys_hooks_set_the_window_up() {
    let mut fx = fixture();
    fx.b.bar_first = Some("notes.txt".to_string());
    fx.relist();
    fx.rest();
    assert_eq!(fx.current(), "notes.txt");
    assert!(fx.b.viewer.is_none(), "bar= does not open it");
    assert!(fx.on_right("plain notes"), "its preview is the receipt");

    let mut fx = fixture();
    fx.b.mode = view::Mode::Dual;
    fx.b.bar_first = Some("notes.txt".to_string());
    fx.relist();
    fx.rest();
    assert!(fx.label("Prevw"));
    assert!(fx.on_right("alpha"), "dual mode: two lists");

    let mut fx = fixture();
    fx.b.script = parse_keys("Down,Down,Space,F5").unwrap().into();
    assert!(!fx.b.ready(), "not ready before the keys are played");
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    assert_eq!(fx.current(), ".hidden", "Down, Down, then Space stepped on");
    assert!(
        fx.drv.find_text("Copy beta").is_some(),
        "Space selected beta, F5 asks about it"
    );
    assert!(parse_keys("F99").is_err());
    assert!(parse_keys("text:*.rs,Shift+F6,Cmd+A,Enter").is_ok());
}

// ---------------------------------------------------------------------------------------------
// 04 §8: keys.

/// K-R1: the list's row names the keys that work in S1 — no slot for a key of a later slice —
/// and the viewer's row keeps its own.
#[test]
fn k_r1_the_rows_name_the_keys_that_work() {
    let mut fx = fixture();
    for label in ["Help", "Rename", "View", "Edit", "Copy", "Move", "Dual"] {
        assert!(fx.label(label), "{label} in the list's row");
    }
    for label in ["MkDir", "Delete", "Tree"] {
        assert!(!fx.label(label), "{label} has not landed");
    }
    fx.open("data.bin");
    for label in ["Help", "Exit", "Edit", "Save", "Goto", "Asc/hx"] {
        assert!(fx.label(label), "{label} in the hex viewer's row");
    }
    assert!(!fx.label("Rename") && !fx.label("Dual"));
}

/// K-R2: a click on a slot and Cmd/Ctrl+digit do what the F-key does, for every slot of the
/// list's row; with the rename field open, F5, Cmd/Ctrl+5 and `+` do nothing.
#[test]
fn k_r2_slots_and_command_digits_act_as_their_f_keys() {
    let mut fx = fixture();
    fx.other_into("alpha");
    fx.select("main.rs");
    let digit = |n: u8| [Key::Num1, Key::Num2, Key::Num5, Key::Num6, Key::Num9][usize::from(n)];
    // F5 three ways.
    for way in 0..3 {
        match way {
            0 => fx.key(Key::F5),
            1 => fx.drv.click_text(&mut fx.b, "Copy   "),
            _ => fx.with(digit(2), command(false)),
        }
        fx.drv.idle(&mut fx.b, 2);
        assert!(fx.drv.find_text("Copy main.rs").is_some(), "way {way}");
        fx.key(Key::Escape);
        assert!(fx.drv.find_text("Copy main.rs").is_none());
    }
    // F6.
    for way in 0..2 {
        match way {
            0 => fx.drv.click_text(&mut fx.b, "Move   "),
            _ => fx.with(digit(3), command(false)),
        }
        fx.drv.idle(&mut fx.b, 2);
        assert!(fx.drv.find_text("Move main.rs").is_some(), "way {way}");
        fx.key(Key::Escape);
    }
    // F9.
    fx.drv.click_text(&mut fx.b, "Dual   ");
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.label("Prevw"));
    fx.with(digit(4), command(false));
    assert!(fx.label("Dual"));
    // F1.
    for way in 0..2 {
        match way {
            0 => fx.drv.click_text(&mut fx.b, "Help   "),
            _ => fx.with(digit(0), command(false)),
        }
        fx.drv.idle(&mut fx.b, 2);
        assert!(crate::gui::help::is_open(&fx.drv.ctx), "way {way}");
        assert!(fx.drv.find_text("Keys in the file list").is_some());
        fx.key(Key::Escape);
        assert!(!crate::gui::help::is_open(&fx.drv.ctx));
    }
    // F4.
    fx.drv.click_text(&mut fx.b, "Edit   ");
    fx.wait(|b| b.launches.is_empty());
    assert_eq!(fx.launched.lock().unwrap().len(), 1, "F4 by its slot");
    // F3.
    fx.drv.click_text(&mut fx.b, "View   ");
    fx.settle();
    assert!(fx.b.viewer.is_some(), "F3 by its slot");
    fx.key(Key::Escape);
    // F2, and what its field keeps away.
    for way in 0..2 {
        match way {
            0 => fx.drv.click_text(&mut fx.b, "Rename "),
            _ => fx.with(digit(1), command(false)),
        }
        fx.drv.idle(&mut fx.b, 2);
        assert_eq!(fx.b.ops.renaming(), Some("main.rs"), "way {way}");
        fx.key(Key::F5);
        fx.with(digit(2), command(false));
        assert!(
            fx.drv.find_text("Copy main.rs").is_none(),
            "F5 and Cmd/Ctrl+5 wait for the field"
        );
        fx.typed("+");
        assert!(
            fx.drv.find_text("Select the names that match").is_none(),
            "+ is typed into the field"
        );
        assert_eq!(fx.b.ops.renaming(), Some("main.rs"));
        fx.key(Key::Escape);
        assert_eq!(fx.b.ops.renaming(), None);
    }
    assert!(Path::new(&format!("{}/main.rs", fx.root)).exists());
}

/// K-R3: exact modifiers — Shift+F6 renames and F6 moves; Shift+F5 and Alt+F2 do nothing; `+`
/// comes from the typed text, whatever keys made it.
#[test]
fn k_r3_modifiers_match_exactly_and_plus_is_typed_text() {
    let mut fx = fixture();
    fx.other_into("alpha");
    fx.select("main.rs");
    fx.with(Key::F6, Modifiers::SHIFT);
    assert_eq!(fx.b.ops.renaming(), Some("main.rs"), "Shift+F6 renames");
    fx.key(Key::Escape);
    fx.key(Key::F6);
    assert!(fx.drv.find_text("Move main.rs").is_some(), "F6 moves");
    fx.key(Key::Escape);
    fx.with(Key::F5, Modifiers::SHIFT);
    fx.with(Key::F2, Modifiers::ALT);
    fx.with(Key::F9, Modifiers::SHIFT);
    assert!(fx.drv.find_text("Copy main.rs").is_none(), "Shift+F5");
    assert_eq!(fx.b.ops.renaming(), None, "Alt+F2");
    assert!(fx.label("Dual"), "Shift+F9");
    assert!(!fx.b.ops.holds_keyboard());
    // `+` as a US layout types it (Shift held, the key event an `=`), then as a layout with a
    // key of its own does.
    for modifiers in [Modifiers::SHIFT, Modifiers::NONE] {
        fx.drv
            .frame(&mut fx.b, vec![Event::Text("+".into())], modifiers);
        fx.drv.idle(&mut fx.b, 2);
        assert!(
            fx.drv.find_text("Select the names that match").is_some(),
            "{modifiers:?}"
        );
        fx.key(Key::Escape);
    }
    assert_eq!(fx.current(), "main.rs", "+ is not a jump letter");
}

/// K-S1: Space selects and steps (yellow, counted in the foot); `+` with a pattern, `*`,
/// Cmd/Ctrl+A; Cmd/Ctrl+R re-reads and keeps the selection; another folder clears it.
#[test]
fn k_s1_selection_by_space_pattern_invert_and_all() {
    let mut fx = fixture();
    let selected = |fx: &Fx| -> Vec<String> {
        fx.b.panels[0]
            .selection()
            .iter()
            .map(|e| e.name.clone())
            .collect()
    };
    fx.key(Key::ArrowDown);
    for _ in 0..3 {
        fx.key(Key::Space);
    }
    assert_eq!(selected(&fx), ["alpha", "beta", ".hidden"]);
    assert_eq!(fx.current(), "data.bin", "each Space stepped down");
    assert_eq!(
        fx.drv.colors_of("alpha"),
        Some(vec![chrome::cga::YELLOW]),
        "selected rows are yellow"
    );
    let foot = fx
        .drv
        .text_starting_with("3 selected = ")
        .expect("the foot");
    assert!(foot.contains("Listed"), "{foot}");
    fx.with(Key::Insert, Modifiers::NONE);
    assert_eq!(selected(&fx).len(), 4, "Ins selects too");
    fx.typed("-");
    fx.drv.idle(&mut fx.b, 2);
    fx.key(Key::Enter);
    assert!(selected(&fx).is_empty(), "- * unselects all");
    fx.typed("+");
    fx.drv.idle(&mut fx.b, 2);
    fx.typed("*.RS");
    fx.key(Key::Enter);
    assert_eq!(selected(&fx), ["main.rs"]);
    fx.typed("*");
    assert_eq!(selected(&fx).len(), 7, "* inverts");
    fx.with(Key::A, command(false));
    assert_eq!(selected(&fx).len(), 8, "Cmd/Ctrl+A selects all");
    std::fs::remove_file(format!("{}/zeta.md", fx.root)).unwrap();
    fx.with(Key::R, command(false));
    fx.settle();
    assert_eq!(fx.names(0).len(), 8, "re-read");
    assert_eq!(
        selected(&fx).len(),
        7,
        "the selection of what is still there"
    );
    fx.select("alpha");
    fx.key(Key::Enter);
    fx.settle();
    assert!(selected(&fx).is_empty(), "another folder clears it");
}

/// 04 §3.1: Cmd/Ctrl+U swaps the panels, Cmd/Ctrl+← / → show this folder in the left / right
/// panel, Cmd/Ctrl+PgUp goes up.
#[test]
fn the_lists_other_command_keys() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    fx.with(Key::U, command(false));
    fx.settle();
    assert_eq!(fx.b.panels[0].dir, alpha, "swapped");
    assert_eq!(fx.b.panels[1].dir, fx.root);
    assert_eq!(fx.b.active, 1, "the keyboard went with its list");
    fx.with(Key::ArrowLeft, command(false));
    fx.settle();
    assert_eq!(fx.b.panels[0].dir, fx.root, "the left panel to this folder");
    fx.key(Key::Tab);
    fx.select("beta");
    fx.key(Key::Enter);
    fx.settle();
    fx.with(Key::ArrowRight, command(false));
    fx.settle();
    assert_eq!(fx.b.panels[1].dir, format!("{}/beta", fx.root));
    fx.with(Key::PageUp, command(false));
    fx.settle();
    assert_eq!(fx.b.panels[0].dir, fx.root);
    assert_eq!(fx.current(), "beta", "the bar on the folder left");
}

// ---------------------------------------------------------------------------------------------
// 04 §8: rename, copy, move — the real engine on this machine's file layer.

/// K-C1: F5 on a file asks, naming the other panel's folder; Enter copies it there — the same
/// bytes, mode and time, the source as it was — says so on the status row and both panels
/// re-read, the bar kept.
#[test]
fn k_c1_f5_copies_the_file_to_the_other_panels_folder() {
    use std::os::unix::fs::PermissionsExt;
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    let src = format!("{}/main.rs", fx.root);
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o640)).unwrap();
    let before = std::fs::metadata(&src).unwrap();
    fx.select("main.rs");
    fx.key(Key::F5);
    assert!(fx.drv.find_text("Copy main.rs").is_some(), "asked first");
    assert!(
        fx.laid_out(&alpha),
        "the target is the other panel's folder"
    );
    assert!(!Path::new(&format!("{alpha}/main.rs")).exists());
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    let copy = std::fs::metadata(format!("{alpha}/main.rs")).expect("the copy");
    assert_eq!(
        std::fs::read_to_string(format!("{alpha}/main.rs")).unwrap(),
        RUST
    );
    assert_eq!(copy.mode() & 0o7777, 0o640, "the mode");
    assert_eq!(copy.mtime(), before.mtime(), "the time");
    assert_eq!(std::fs::read_to_string(&src).unwrap(), RUST);
    assert_eq!(std::fs::metadata(&src).unwrap().ino(), before.ino());
    assert_eq!(fx.status(), format!("copied main.rs to {alpha}"));
    assert_eq!(fx.names(1), ["..", "inner.txt", "main.rs"], "re-read");
    assert_eq!(fx.current(), "main.rs", "the bar is kept");
    assert!(fx.b.viewer.is_none(), "the dialog's Enter opened nothing");
}

/// K-C2: a name in the way asks — Skip leaves both, Rename makes `<stem>-1<ext>`, Cancel stops —
/// and the file that was there is never touched; the report lists what happened.
#[test]
fn k_c2_a_name_in_the_way_asks_skip_rename_or_cancel() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    let there = format!("{alpha}/notes.txt");
    std::fs::write(&there, "what was there\n").unwrap();
    let before = std::fs::metadata(&there).unwrap().ino();
    let source = PathBuf::from(format!("{}/notes.txt", fx.root));
    let source_was = entry_state(&source);
    let untouched = |why: &str| {
        assert_eq!(
            std::fs::read_to_string(&there).unwrap(),
            "what was there\n",
            "{why}"
        );
        assert_eq!(std::fs::metadata(&there).unwrap().ino(), before, "{why}");
        assert_eq!(entry_state(&source), source_was, "the source, after {why}");
    };
    fx.select("notes.txt");
    let ask = |fx: &mut Fx| {
        fx.key(Key::F5);
        fx.key(Key::Enter);
        fx.wait(|b| b.ops.holds_keyboard());
        assert!(fx.drv.find_text("Name exists").is_some());
        assert!(fx.drv.find_text("Overwrite").is_none(), "never offered");
    };
    ask(&mut fx);
    fx.drv.click_text(&mut fx.b, "Skip");
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    let line = fx.drv.text_starting_with("skipped").expect("the report");
    assert!(line.ends_with("notes.txt"), "{line}");
    untouched("Skip");
    fx.key(Key::Enter);
    assert!(!fx.b.ops.holds_keyboard(), "the report closed");

    ask(&mut fx);
    assert!(fx.drv.find_text("notes-1.txt").is_some(), "the free name");
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert_eq!(
        std::fs::read_to_string(format!("{alpha}/notes-1.txt")).unwrap(),
        "plain notes\n"
    );
    untouched("Rename");
    let line = fx.drv.text_starting_with("renamed").expect("the report");
    assert!(line.ends_with("notes.txt → notes-1.txt"), "{line}");
    fx.key(Key::Enter);

    ask(&mut fx);
    fx.key(Key::Escape);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    untouched("Cancel");
    assert!(!Path::new(&format!("{alpha}/notes-2.txt")).exists());
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.holds_keyboard() && b.listing.is_empty());
    assert_eq!(fx.current(), "notes.txt");
}

/// K-C3: F5 on a folder holding a file, a subfolder, a link and a fifo copies the first three —
/// the link as a link — and the report lists the fifo as skipped.
#[test]
fn k_c3_a_folder_copy_keeps_its_link_and_reports_the_fifo() {
    let mut fx = fixture();
    let beta = fx.other_into("beta");
    let alpha = format!("{}/alpha", fx.root);
    std::fs::create_dir(format!("{alpha}/sub")).unwrap();
    std::fs::write(format!("{alpha}/sub/deep.txt"), "deep\n").unwrap();
    std::os::unix::fs::symlink("inner.txt", format!("{alpha}/link")).unwrap();
    let fifo = std::ffi::CString::new(format!("{alpha}/pipe")).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let source_was = tree_state(Path::new(&alpha));
    assert!(source_was
        .iter()
        .any(|(n, s)| n == "link" && s.starts_with("link to inner.txt")));
    assert!(source_was
        .iter()
        .any(|(n, s)| n == "pipe" && s.starts_with("special")));
    fx.select("alpha");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert_eq!(
        tree_state(Path::new(&alpha)),
        source_was,
        "the source folder is as it was: every file, the link and the fifo"
    );
    let copy = format!("{beta}/alpha");
    assert_eq!(
        std::fs::read_to_string(format!("{copy}/inner.txt")).unwrap(),
        "inside\n"
    );
    assert_eq!(
        std::fs::read_to_string(format!("{copy}/sub/deep.txt")).unwrap(),
        "deep\n"
    );
    let link = std::fs::symlink_metadata(format!("{copy}/link")).unwrap();
    assert!(
        link.file_type().is_symlink(),
        "the link is copied as a link"
    );
    assert_eq!(
        std::fs::read_link(format!("{copy}/link")).unwrap(),
        Path::new("inner.txt")
    );
    assert!(!Path::new(&format!("{copy}/pipe")).exists());
    let line = fx.drv.text_starting_with("skipped").expect("the report");
    assert!(line.contains("alpha/pipe"), "{line}");
    fx.key(Key::Enter);
    assert!(!fx.b.ops.holds_keyboard());
}

/// K-C4, the window's side: while an operation runs the status row says where it is and the
/// list's keys go on; Esc stops it (the engine's stop flag), and what it did is reported.
#[test]
fn k_c4_esc_stops_a_running_operation() {
    let mut fx = fixture();
    fx.other_into("alpha");
    let stopped = Arc::new(AtomicBool::new(false));
    let saw = stopped.clone();
    fx.b.set_ops_run(Arc::new(move |job: ops::Job, link: &ops::Link| {
        let ops::Job::Transfer { names, .. } = job else {
            return Report::default();
        };
        link.progress(Progress {
            verb: "copying",
            item: 1,
            items: names.len(),
            name: names[0].clone(),
            done: 0,
            total: 0,
        });
        // Held until Esc reaches the engine's end of the stop flag — or, a test gone wrong, for
        // 20 s: the worker then ends by itself, and the test fails on what it never saw.
        let flag = link.stop_flag();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !flag.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        saw.store(flag.load(Ordering::Relaxed), Ordering::Relaxed);
        Report {
            items: names
                .into_iter()
                .map(|name| Item {
                    name,
                    outcome: ItemOutcome::NotReached,
                })
                .collect(),
            notes: vec!["stopped (Esc) before main.rs: 2 item(s) not reached".to_string()],
        }
    }));
    fx.select("main.rs");
    fx.key(Key::Space);
    fx.key(Key::Space);
    fx.key(Key::F5);
    assert!(fx.drv.find_text("Copy 2 items").is_some());
    fx.key(Key::Enter);
    fx.wait(|b| b.status_line().text.starts_with("copying 1/2"));
    assert_eq!(fx.status(), "copying 1/2 · main.rs · Esc stops");
    assert!(!fx.b.ready(), "not settled while it runs");
    let at = fx.current();
    fx.key(Key::ArrowUp);
    assert_ne!(fx.current(), at, "the list's keys go on meanwhile");
    fx.key(Key::F5);
    assert!(fx.drv.find_text("Copy 2 items").is_none());
    assert!(
        fx.status().contains("copying"),
        "a second one waits: {}",
        fx.status()
    );
    fx.key(Key::Escape);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert!(stopped.load(Ordering::Relaxed), "Esc reached the engine");
    let line = fx
        .drv
        .text_starting_with("not reached")
        .expect("the report");
    assert!(line.ends_with("main.rs"), "{line}");
    assert!(fx.drv.text_starting_with("stopped (Esc)").is_some());
    fx.key(Key::Escape);
    assert!(!fx.b.ops.holds_keyboard());
}

/// The hosts the file operations of a test connected to, in order.
type Hosts = Arc<Mutex<Vec<Option<String>>>>;

/// Connections for file operations: this machine's file layer, and for any host a local
/// `sftp-server` (a real server over pipes). `None` where it is not installed.
fn through_sftp() -> Option<(OpsConnect, Hosts)> {
    drop(SftpRemote::local_server()?);
    let hosts = Hosts::default();
    let asked = hosts.clone();
    let connect: OpsConnect = Arc::new(move |host: Option<&str>| {
        asked.lock().unwrap().push(host.map(str::to_string));
        Ok(match host {
            Some(_) => Box::new(SftpRemote::local_server().expect("sftp-server"))
                as Box<dyn crate::remote::ops::FileOps + Send>,
            None => Box::new(LocalFs::new()),
        })
    });
    Some((connect, hosts))
}

/// K-C5: the target field takes `host:/abs`: the copy goes to that host over a connection of
/// its own, the bytes arriving whole.
#[test]
fn k_c5_the_target_field_takes_another_host() {
    let Some((connect, hosts)) = through_sftp() else {
        return;
    };
    let mut fx = fixture();
    fx.b.set_ops_connect(connect);
    let alpha = format!("{}/alpha", fx.root);
    fx.select("photo.png");
    fx.key(Key::F5);
    // The field's text is selected: typing replaces it.
    fx.typed(&format!("box:{alpha}"));
    assert!(fx.laid_out(&format!("box:{alpha}")));
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert_eq!(
        std::fs::read(format!("{alpha}/photo.png")).unwrap(),
        std::fs::read(format!("{}/photo.png", fx.root)).unwrap()
    );
    assert_eq!(fx.status(), format!("copied photo.png to box:{alpha}"));
    assert_eq!(
        hosts.lock().unwrap().as_slice(),
        [None, Some("box".to_string())],
        "a connection for each side"
    );
}

/// K-M1: F6 of a local file within one filesystem renames it in place (the inode is kept); of a
/// remote file it goes over one connection to that host (the server links and removes).
#[test]
fn k_m1_f6_moves_within_a_host_over_one_connection() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    let ino = std::fs::metadata(format!("{}/main.rs", fx.root))
        .unwrap()
        .ino();
    fx.select("main.rs");
    fx.key(Key::F6);
    assert!(fx.drv.find_text("Move main.rs").is_some());
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert!(!Path::new(&format!("{}/main.rs", fx.root)).exists());
    assert_eq!(
        std::fs::metadata(format!("{alpha}/main.rs")).unwrap().ino(),
        ino
    );
    assert_eq!(fx.status(), format!("moved main.rs to {alpha}"));
    assert!(
        !fx.names(0).contains(&"main.rs".to_string()),
        "both re-read"
    );
    assert!(fx.names(1).contains(&"main.rs".to_string()));

    let Some((connect, hosts)) = through_sftp() else {
        return;
    };
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    fx.b.set_ops_connect(connect);
    let alpha = fx.other_into("alpha");
    fx.select("notes.txt");
    fx.key(Key::F6);
    assert!(fx.laid_out(&format!("box:{alpha}")), "the remote form");
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert!(!Path::new(&format!("{}/notes.txt", fx.root)).exists());
    assert_eq!(
        std::fs::read_to_string(format!("{alpha}/notes.txt")).unwrap(),
        "plain notes\n"
    );
    assert_eq!(
        hosts.lock().unwrap().as_slice(),
        [Some("box".to_string())],
        "one connection: the host renames"
    );
}

/// K-M2: F6 to another host copies, checks and removes: the bytes are there and the source is
/// gone.
#[test]
fn k_m2_f6_to_another_host_copies_checks_and_removes() {
    let Some((connect, hosts)) = through_sftp() else {
        return;
    };
    let mut fx = fixture();
    fx.b.set_ops_connect(connect);
    let alpha = format!("{}/alpha", fx.root);
    fx.select("notes.txt");
    fx.key(Key::F6);
    fx.typed(&format!("box:{alpha}"));
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert_eq!(
        std::fs::read_to_string(format!("{alpha}/notes.txt")).unwrap(),
        "plain notes\n"
    );
    assert!(!Path::new(&format!("{}/notes.txt", fx.root)).exists());
    assert_eq!(hosts.lock().unwrap().len(), 2, "a connection for each side");
    assert!(!fx.names(0).contains(&"notes.txt".to_string()), "re-read");
}

/// K-N1: F2 makes the name a field in its row; a name that exists is an error on the status row
/// and the field stays; a free name renames, the bar on it; Esc cancels; a folder on a remote is
/// refused with the reason.
#[test]
fn k_n1_f2_renames_in_place() {
    let mut fx = fixture();
    fx.select("main.rs");
    fx.key(Key::F2);
    assert_eq!(fx.b.ops.renaming(), Some("main.rs"));
    // The name is selected, the extension after it: typing replaces `main`.
    fx.typed("notes");
    fx.with(Key::A, command(false));
    fx.typed("notes.txt");
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert_eq!(fx.b.ops.renaming(), Some("main.rs"), "the field stays");
    let said = fx.b.status_line();
    assert!(
        said.error && said.text.contains("notes.txt"),
        "{}",
        said.text
    );
    assert!(Path::new(&format!("{}/main.rs", fx.root)).exists());
    assert_eq!(
        std::fs::read_to_string(format!("{}/notes.txt", fx.root)).unwrap(),
        "plain notes\n",
        "nothing was replaced"
    );
    fx.with(Key::A, command(false));
    fx.typed("prog.rs");
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert_eq!(fx.b.ops.renaming(), None);
    assert!(!Path::new(&format!("{}/main.rs", fx.root)).exists());
    assert_eq!(
        std::fs::read_to_string(format!("{}/prog.rs", fx.root)).unwrap(),
        RUST
    );
    assert_eq!(fx.current(), "prog.rs", "the bar is on the new name");
    assert_eq!(fx.status(), "renamed main.rs → prog.rs");
    assert_eq!(fx.names(1), fx.names(0), "the other panel shows it too");
    // Esc cancels; `..` cannot be renamed.
    fx.key(Key::F2);
    fx.typed("gone");
    fx.key(Key::Escape);
    assert_eq!(fx.b.ops.renaming(), None);
    assert!(Path::new(&format!("{}/prog.rs", fx.root)).exists());
    fx.key(Key::Home);
    fx.key(Key::F2);
    assert_eq!(fx.b.ops.renaming(), None);
    assert!(fx.b.status_line().error);
    // A folder on a remote: the reason, nothing changed (until S6).
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    fx.select("alpha");
    fx.key(Key::F2);
    assert_eq!(fx.b.ops.renaming(), None);
    let said = fx.status();
    assert!(said.contains("cannot rename a folder on box yet"), "{said}");
}

/// K-E1 through the window: a folder copied into itself is refused before anything happens, and
/// the report says why.
#[test]
fn k_e1_a_folder_into_itself_is_refused_and_reported() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    fx.select("alpha");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert!(fx.b.ops.holds_keyboard(), "the report is up");
    assert!(fx.drv.text_starting_with("not reached").is_some());
    assert_eq!(
        files_under(Path::new(&alpha)),
        ["inner.txt"],
        "nothing was made"
    );
    fx.key(Key::Enter);
    assert!(!fx.b.ops.holds_keyboard());
}

/// 04 §5.1 "After", 02 §7: a finished operation tells the browser which folders changed
/// ([`BrowserEvent::FilesChanged`]); the panels showing one re-list, the bar kept on its name.
#[test]
fn files_changed_relists_the_panels_showing_those_folders() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    fx.select("notes.txt");
    std::fs::write(format!("{alpha}/new.txt"), "new\n").unwrap();
    std::fs::write(format!("{}/new.txt", fx.root), "new\n").unwrap();
    fx.b.event(
        &fx.drv.ctx.clone(),
        BrowserEvent::FilesChanged {
            host: None,
            dirs: vec![alpha.clone()],
        },
    );
    fx.settle();
    assert!(fx.names(1).contains(&"new.txt".to_string()), "re-listed");
    assert!(
        !fx.names(0).contains(&"new.txt".to_string()),
        "not this one"
    );
    fx.b.event(
        &fx.drv.ctx.clone(),
        BrowserEvent::FilesChanged {
            host: Some("elsewhere".to_string()),
            dirs: vec![fx.root.clone()],
        },
    );
    fx.settle();
    assert!(
        !fx.names(0).contains(&"new.txt".to_string()),
        "another host's"
    );
    assert_eq!(fx.current(), "notes.txt", "the bar is kept");
}

/// The file operations' connections are made for a host as the browser prints it: the label
/// reads back to the host `ssh` takes (an IPv6 host unbracketed, the user kept).
#[test]
fn a_host_label_reads_back_to_its_ssh_destination() {
    for (label, destination) in [
        ("box", "box"),
        ("me@box", "me@box"),
        ("me@[::1]", "me@::1"),
        ("[fe80::1]", "fe80::1"),
    ] {
        let spec = host_spec(label).unwrap();
        assert_eq!(spec.destination(), destination, "{label}");
        assert_eq!(spec.label(), label);
    }
    assert!(host_spec("").is_err());
}

/// File-operation connections on this machine's file layer that stop at `gate` before they are
/// handed to the engine (on the operation's worker).
fn gated_ops(gate: &Gate) -> OpsConnect {
    let gate = gate.clone();
    Arc::new(move |_: Option<&str>| {
        gate.hold();
        Ok(Box::new(LocalFs::new()) as Box<dyn crate::remote::ops::FileOps + Send>)
    })
}

/// K-C4 with the real engine: Esc while the operation's worker runs reaches the engine through
/// its own stop flag — nothing is copied, and the engine's report says it stopped and what it
/// did not reach.
#[test]
fn k_c4_esc_reaches_the_real_engine() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    let gate = Gate::default();
    fx.b.set_ops_connect(gated_ops(&gate));
    fx.select("main.rs");
    fx.key(Key::Space);
    fx.key(Key::Space);
    fx.key(Key::F5);
    fx.key(Key::Enter);
    gate.reached();
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.ops.busy());
    fx.key(Key::Escape);
    assert_eq!(fx.status(), "stopping…");
    gate.release();
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert_eq!(
        files_under(Path::new(&alpha)),
        ["inner.txt"],
        "nothing copied"
    );
    let note = fx
        .drv
        .text_starting_with("stopped (Esc)")
        .expect("the engine's own note");
    assert!(note.contains("2 item(s) not reached"), "{note}");
    assert_eq!(fx.drv.texts_starting_with("not reached").len(), 2);
    fx.key(Key::Enter);
    assert!(!fx.b.ops.holds_keyboard());
}

// ---------------------------------------------------------------------------------------------
// Round A.

/// The hex preview in the browser's half: no path row under the side's frame (which names the
/// file), and rows of eight bytes with their ASCII column whole.
#[test]
fn the_hex_preview_fits_the_side() {
    let mut fx = fixture();
    fx.bar_on("data.bin");
    assert!(fx.on_right("00 01 02 03 04 05 06 07"), "eight bytes a row");
    assert!(fx.on_right("0008(0008)"));
    let ascii: String = (0x30..0x38u8).map(chrome::cp437).collect();
    assert!(fx.on_right(&ascii), "the ASCII column is whole");
    let title = fx.framed(&format!("{}/data.bin", fx.root));
    let named = fx.drv.texts_starting_with(&title).len();
    assert_eq!(named, 1, "the file is named once, in the side's frame");
}

/// While a name-in-the-way question is open the status row names the item it asks about, not
/// the one before it.
#[test]
fn the_status_row_names_the_item_a_question_is_about() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    std::fs::write(format!("{alpha}/notes.txt"), "there\n").unwrap();
    fx.select("main.rs");
    fx.key(Key::Space);
    fx.key(Key::Space);
    fx.key(Key::F5);
    fx.key(Key::Enter);
    fx.wait(|b| b.ops.holds_keyboard());
    assert!(fx.drv.find_text("Name exists").is_some());
    assert_eq!(
        fx.status(),
        "copying 2/2 · notes.txt · waiting for an answer"
    );
    fx.drv.click_text(&mut fx.b, "Skip");
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    fx.key(Key::Enter);
}

/// S1's key help says what Esc does in S1: nothing of the pin (S2).
#[test]
fn the_key_help_names_no_key_of_a_later_slice() {
    let page = help::for_context(Context::List, view::Mode::Preview, &[]);
    let esc = page.lines.iter().find(|l| l.keys == "Esc").expect("Esc");
    assert_eq!(esc.what, "Stop a fetch or a file operation");
    assert!(
        page.lines.iter().all(|l| !l.what.contains("pin")),
        "{page:?}"
    );
}

/// A stand-in engine held at `gate`, then — `ask` — asking about its first name, and done.
fn held_engine(gate: &Gate, ask: bool) -> ops::Run {
    let gate = gate.clone();
    Arc::new(move |job: ops::Job, link: &ops::Link| {
        let ops::Job::Transfer { names, .. } = job else {
            return Report::default();
        };
        gate.hold();
        let name = names[0].clone();
        let outcome = if ask {
            let reply = link.ask(crate::fileops::Clash {
                name: name.clone(),
                free: "free.txt".to_string(),
            });
            match reply.answer {
                crate::fileops::Answer::Skip => ItemOutcome::Skipped,
                crate::fileops::Answer::Rename(to) => ItemOutcome::Renamed(to),
                crate::fileops::Answer::Cancel => ItemOutcome::NotReached,
            }
        } else {
            ItemOutcome::Done
        };
        Report {
            items: vec![Item { name, outcome }],
            notes: Vec::new(),
        }
    })
}

/// A file operation is polled whatever the window shows: its question arriving while a viewer
/// is open is said on the status row — the viewer is left as it is, no dialog over it — and is
/// asked once the list shows again.
#[test]
fn a_question_waits_for_the_list_while_a_viewer_is_open() {
    let mut fx = fixture();
    fx.other_into("alpha");
    let gate = Gate::default();
    fx.b.set_ops_run(held_engine(&gate, true));
    fx.select("main.rs");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    gate.reached();
    // (Not `open`: the window is not settled while the operation runs.)
    fx.select("notes.txt");
    fx.key(Key::Enter);
    fx.wait(|b| b.viewer.as_ref().is_some_and(|v| v.embedded.window.ready()));
    gate.release();
    fx.wait(|b| b.ops.holds_keyboard());
    assert!(fx.b.viewer.is_some());
    assert_eq!(
        fx.status(),
        "copy waiting for an answer — F3 back to the list"
    );
    assert!(
        fx.drv.find_text("Name exists").is_none(),
        "no dialog over it"
    );
    assert!(
        fx.drv.find_text("plain notes").is_some(),
        "the viewer as it was"
    );
    fx.key(Key::F3);
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.is_none());
    assert!(
        fx.drv.find_text("Name exists").is_some(),
        "asked in the list"
    );
    fx.drv.click_text(&mut fx.b, "Skip");
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert!(fx.drv.text_starting_with("skipped").is_some(), "the report");
    fx.key(Key::Enter);
}

/// An operation that ends while a viewer is open is told when the list shows again, and the
/// panels are re-read meanwhile.
#[test]
fn an_operation_ending_under_a_viewer_is_told_in_the_list() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    let gate = Gate::default();
    fx.b.set_ops_run(held_engine(&gate, false));
    fx.select("main.rs");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    gate.reached();
    // (Not `open`: the window is not settled while the operation runs.)
    fx.select("notes.txt");
    fx.key(Key::Enter);
    fx.wait(|b| b.viewer.as_ref().is_some_and(|v| v.embedded.window.ready()));
    std::fs::write(format!("{alpha}/main.rs"), RUST).unwrap();
    gate.release();
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    assert!(fx.b.viewer.is_some(), "the viewer stays");
    assert!(
        fx.names(1).contains(&"main.rs".to_string()),
        "re-read under it"
    );
    fx.key(Key::Escape);
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.status(), format!("copied main.rs to {alpha}"));
}

/// A re-read that finds the file under the bar changed (size or time) prepares its preview
/// again, the bar unmoved.
#[test]
fn a_reread_refreshes_the_preview_of_a_changed_file() {
    let mut fx = fixture();
    fx.bar_on("notes.txt");
    assert!(fx.on_right("plain notes"));
    let before = fx.b.preview.spawned();
    // Unchanged: a re-read prepares nothing.
    fx.with(Key::R, command(false));
    fx.rest();
    assert_eq!(fx.b.preview.spawned(), before, "unchanged: kept");
    std::fs::write(format!("{}/notes.txt", fx.root), "other words now\n").unwrap();
    fx.with(Key::R, command(false));
    fx.rest();
    assert_eq!(fx.current(), "notes.txt");
    assert!(fx.on_right("other words now"), "prepared again");
    assert!(!fx.on_right("plain notes"));
    assert_eq!(fx.b.preview.spawned(), before + 1);
}

/// 02 §4.3's deadline in the browser's own preview, on a clock of the test's (each look at it
/// is three seconds later): the preparation stops at five seconds after its read and the side
/// shows the card — no wall clock is waited for.
#[test]
fn the_previews_deadline_runs_on_an_injected_clock() {
    let mut fx = fixture();
    let start = std::time::Instant::now();
    let looks = Arc::new(AtomicU64::new(0));
    fx.b.preview.set_budget(Box::new(move || {
        let looks = looks.clone();
        prepare::Budget::with_clock(
            prepare::Limits::PREVIEW,
            Box::new(move || start + Duration::from_secs(3 * looks.fetch_add(1, Ordering::SeqCst))),
        )
    }));
    fx.bar_on("notes.txt");
    assert!(fx.on_right("File Info"), "the card");
    assert_eq!(
        fx.right_text("the preview took over").as_deref(),
        Some("the preview took over 5 s: Enter opens it")
    );
    assert!(!fx.on_right("plain notes"));
}

// ---------------------------------------------------------------------------------------------
// Round B (review round 1).

/// Runs `body` on a thread of its own and fails when it has not ended in `secs`: a test of
/// something that must not block (or must end) cannot hang the run.
fn bounded(secs: u64, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        body();
        let _ = tx.send(());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(()) => worker.join().unwrap(),
        // The body panicked: its own message.
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(worker.join().unwrap_err())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("not over in {secs} s"),
    }
}

/// Waits (bounded, without a pass of the window) until `done`.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "{what}: not in 20 s");
        std::thread::yield_now();
    }
}

/// A listing for `panel` that stays running until the test answers it.
fn held_listing(fx: &mut Fx, panel: usize, dir: &str) -> mpsc::Sender<Result<Listed>> {
    let (tx, rx) = mpsc::channel();
    fx.b.listing.push(Listing {
        panel: Some(panel),
        dir: dir.to_string(),
        select: None,
        keep: None,
        rx,
    });
    tx
}

/// B1: Esc stops a running file operation while a listing is still on its way: a slow or stuck
/// listing never makes a copy unstoppable.
#[test]
fn esc_stops_an_operation_while_a_listing_is_pending() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    let gate = Gate::default();
    fx.b.set_ops_run(held_engine(&gate, false));
    fx.select("main.rs");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    gate.reached();
    let listing = held_listing(&mut fx, 1, &alpha);
    fx.key(Key::Escape);
    assert_eq!(
        fx.b.ops.status().map(|s| s.text).as_deref(),
        Some("stopping…"),
        "Esc reached the operation, the listing still pending"
    );
    assert_eq!(fx.b.listing.len(), 1);
    // The list's other keys still wait for the listing.
    let at = fx.current();
    fx.key(Key::ArrowDown);
    assert_eq!(fx.current(), at);
    gate.release();
    drop(listing);
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
}

/// B2: a listing's failure stays said until that panel lists again — another panel's good
/// listing does not clear it, in either order of completion.
#[test]
fn a_panels_listing_error_outlives_another_panels_success() {
    let entries = || -> Result<Listed> { Ok((LocalFs::new().list_entries("/")?, None)) };
    for failure_first in [true, false] {
        let mut fx = fixture();
        let root = fx.root.clone();
        let bad = held_listing(&mut fx, 0, &root);
        let good = held_listing(&mut fx, 1, "/");
        let fail = || {
            bad.send(Err(anyhow::anyhow!("the left one failed")))
                .unwrap()
        };
        if failure_first {
            fail();
            fx.drv.idle(&mut fx.b, 1);
            good.send(entries()).unwrap();
        } else {
            good.send(entries()).unwrap();
            fx.drv.idle(&mut fx.b, 1);
            fail();
        }
        fx.drv.idle(&mut fx.b, 2);
        assert_eq!(fx.b.panels[1].dir, "/", "the good one is shown");
        let said = fx.b.status_line();
        assert!(
            said.error && said.text.contains("the left one failed"),
            "failure first {failure_first}: {}",
            said.text
        );
        // Both in one pass, too.
        let bad = held_listing(&mut fx, 0, &root);
        let good = held_listing(&mut fx, 1, "/");
        bad.send(Err(anyhow::anyhow!("failed again"))).unwrap();
        good.send(entries()).unwrap();
        fx.drv.idle(&mut fx.b, 2);
        let said = fx.b.status_line();
        assert!(
            said.error && said.text.contains("failed again"),
            "{}",
            said.text
        );
        // Its own panel's good listing clears it.
        fx.b.list(Some(0), root, None, None);
        fx.settle();
        let said = fx.b.status_line();
        assert!(!said.error, "{}", said.text);
    }
}

/// B3: a rename's new name gets the bar in the panel the rename was made in, whichever panel
/// has the keyboard when it completes. (No key moves the keyboard while the rename field is
/// open; the test moves it directly.)
#[test]
fn a_renames_bar_goes_to_the_panel_it_was_made_in() {
    let mut fx = fixture();
    let gate = Gate::default();
    fx.b.set_ops_connect(gated_ops(&gate));
    fx.select("main.rs");
    fx.key(Key::F2);
    fx.with(Key::A, command(false));
    fx.typed("prog.rs");
    fx.key(Key::Enter);
    gate.reached();
    fx.b.active = 1;
    gate.release();
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    let bar = |fx: &Fx, i: usize| fx.b.panels[i].current().unwrap().name.clone();
    assert_eq!(bar(&fx, 0), "prog.rs", "the panel the rename was made in");
    assert_eq!(bar(&fx, 1), "..", "the other panel's bar stays");
    assert_eq!(fx.b.ops.renaming(), None);
}

/// B7: Esc in the middle of a transfer, with the real engine: the worker is held after bytes
/// were written; Esc, then on — the source is as it was, nothing is under the target name, no
/// staging temp is left, and the report says it stopped.
#[test]
fn k_c4_esc_mid_transfer_leaves_nothing_half_made() {
    let mut fx = fixture();
    // Three steps of the engine's copy: progress comes between them.
    let size = 3 * crate::remote::ops::COPY_STEP;
    let src = format!("{}/large.bin", fx.root);
    std::fs::File::create(&src).unwrap().set_len(size).unwrap();
    std::fs::write(format!("{}/next.txt", fx.root), "after\n").unwrap();
    fx.relist();
    let alpha = fx.other_into("alpha");
    let before = std::fs::metadata(&src).unwrap();
    let gate = Gate::default();
    let (held, once) = (gate.clone(), Arc::new(AtomicBool::new(false)));
    let wrote = Arc::new(AtomicU64::new(0));
    let saw = wrote.clone();
    let local: OpsConnect = Arc::new(|_: Option<&str>| {
        Ok(Box::new(LocalFs::new()) as Box<dyn crate::remote::ops::FileOps + Send>)
    });
    fx.b.set_ops_seen(
        local,
        Arc::new(move |p: &Progress| {
            let mid = p.name == "large.bin" && p.done > 0 && p.done < p.total;
            if mid && !once.swap(true, Ordering::SeqCst) {
                saw.store(p.done, Ordering::SeqCst);
                held.hold();
            }
        }),
    );
    fx.select("large.bin");
    fx.key(Key::Space);
    fx.select("next.txt");
    fx.key(Key::Space);
    fx.key(Key::F5);
    assert!(fx.drv.find_text("Copy 2 items").is_some());
    fx.key(Key::Enter);
    gate.reached();
    let done = wrote.load(Ordering::SeqCst);
    assert!(
        done > 0 && done < size,
        "held mid-file, {done} of {size} written"
    );
    let staged: Vec<String> = files_under(Path::new(&alpha))
        .into_iter()
        .filter(|f| f.contains(".sopen-"))
        .collect();
    assert_eq!(staged.len(), 1, "the copy is being staged: {staged:?}");
    fx.key(Key::Escape);
    gate.release();
    fx.wait(|b| !b.ops.busy() && b.listing.is_empty());
    // Independently of the report: what is on disk.
    let after = std::fs::metadata(&src).unwrap();
    assert_eq!(
        (after.ino(), after.len(), after.mtime()),
        (before.ino(), size, before.mtime())
    );
    // Both sources hold what the fixture wrote: the large one all zeros, the next one whole.
    let large = std::fs::read(&src).unwrap();
    assert!(large.len() as u64 == size && large.iter().all(|b| *b == 0));
    assert_eq!(
        std::fs::read_to_string(format!("{}/next.txt", fx.root)).unwrap(),
        "after\n"
    );
    assert_eq!(
        files_under(Path::new(&alpha)),
        ["inner.txt"],
        "no target, no staging temp, nothing of the item not reached"
    );
    let line = fx
        .drv
        .text_starting_with("failed")
        .expect("the stopped item");
    assert!(
        line.contains("large.bin") && line.contains("stopped"),
        "{line}"
    );
    assert!(fx
        .drv
        .text_starting_with("not reached")
        .is_some_and(|l| l.ends_with("next.txt")));
    fx.key(Key::Enter);
}

/// B8: closing the window during a file operation: [`Leftover::finish`] waits for it. The
/// operation asks about a name once the window is gone; only the wait answers that (a dropped
/// operation is told to stop instead), so an answer without the stop flag is the proof that
/// `finish` was waiting while the worker still ran — it then copies, and `finish` returns only
/// after that.
#[test]
fn closing_the_window_waits_for_a_running_operation() {
    bounded(60, || {
        let mut fx = fixture();
        let alpha = fx.other_into("alpha");
        let gate = Gate::default();
        let held = gate.clone();
        let answered_by_wait = Arc::new(Mutex::new(None));
        let (answered, returned) = (answered_by_wait.clone(), Arc::new(AtomicBool::new(false)));
        let finish_returned = returned.clone();
        let local: OpsConnect = Arc::new(|_: Option<&str>| {
            Ok(Box::new(LocalFs::new()) as Box<dyn crate::remote::ops::FileOps + Send>)
        });
        let real = engine(local);
        fx.b.set_ops_run(Arc::new(move |job: ops::Job, link: &ops::Link| {
            held.hold();
            let reply = link.ask(crate::fileops::Clash {
                name: "main.rs".to_string(),
                free: "main-1.rs".to_string(),
            });
            let by_wait = !link.stopped() && reply.answer == crate::fileops::Answer::Cancel;
            *answered.lock().unwrap() = Some((by_wait, finish_returned.load(Ordering::SeqCst)));
            real(job, link)
        }));
        fx.select("main.rs");
        fx.key(Key::F5);
        fx.key(Key::Enter);
        gate.reached();
        assert!(Window::close_requested(&mut fx.b));
        let (leftover, root) = (fx.b.leftover.clone(), fx.root.clone());
        let Fx { b, _tmp, .. } = fx;
        drop(b);
        assert!(!Path::new(&format!("{alpha}/main.rs")).exists());
        gate.release();
        leftover.borrow_mut().finish();
        returned.store(true, Ordering::SeqCst);
        assert_eq!(
            *answered_by_wait.lock().unwrap(),
            Some((true, false)),
            "the wait answered the worker's question, before finish returned"
        );
        assert_eq!(
            std::fs::read_to_string(format!("{alpha}/main.rs")).unwrap(),
            RUST
        );
        assert_eq!(
            std::fs::read_to_string(format!("{root}/main.rs")).unwrap(),
            RUST
        );
        drop(_tmp);
    });
}

/// B14: a preparation that finished just before the bar left its file is not shown: the side
/// stays frozen on its last progress frame until the display commits (02 §3.1).
#[test]
fn a_cancelled_previews_queued_result_does_not_replace_the_frozen_side() {
    let (mut fx, _seen, gate) = watched("big.png");
    big_picture(&fx);
    fx.relist();
    fx.select("big.png");
    fx.drv.idle(&mut fx.b, REST);
    gate.reached();
    fx.drv.idle(&mut fx.b, 2);
    let frame = fx
        .right_text("reading big.png")
        .expect("the progress frame");
    // The worker runs to its end with no pass of the window: its result waits in the channel.
    gate.release();
    until("the worker's end", || fx.b.preview.worker_ended());
    // The bar leaves: this pass cancels the preparation and finds its result.
    fx.key(Key::ArrowDown);
    assert_ne!(fx.current(), "big.png");
    let frozen = fx
        .right_text("reading big.png")
        .expect("the side stays frozen");
    assert_eq!(frozen, format!("{} …", frame.trim_end_matches('…')));
    assert!(fx.drv.pictures().is_empty(), "its viewer is not shown");
    fx.rest();
    assert!(fx.right_text("reading big.png").is_none(), "committed");
}

/// B15, and B5's proof for F9: F9 during a preview's read — the bar unmoved — cancels the
/// preparation: its read stops before the file's end.
#[test]
fn f9_cancels_a_running_preview() {
    // The preparation held where its stream opens: still there when F9 comes.
    let (mut fx, seen, gate) = watched("open:big.png");
    let size = big_picture(&fx);
    fx.relist();
    fx.select("big.png");
    fx.drv.idle(&mut fx.b, REST);
    gate.reached();
    fx.key(Key::F9);
    assert_eq!(fx.current(), "big.png", "the bar did not move");
    assert!(fx.b.preview.cancelling(), "F9 told the preparation to stop");
    gate.release();
    fx.wait(|b| !b.preview.busy());
    let read = seen.lock().unwrap().streamed;
    assert!(
        read < size,
        "the read stopped before the end: {read} of {size}"
    );
    assert!(files_under(&fx.cache).is_empty(), "nothing on disk");
}

/// B10, P5 for text and hex: a click, a drag, the wheel, typing, an editing key and a paste
/// over a text preview and over a hex preview change neither — the same rows, nothing edited
/// (handed over, each leaves without an unsaved-changes prompt) — and the typing reached the
/// list.
#[test]
fn p5_text_and_hex_previews_take_no_input() {
    let mut fx = fixture();
    let centre = Pos2::new(750.0, 300.0);
    let inputs = |fx: &mut Fx| {
        fx.drv.click(&mut fx.b, centre);
        fx.drv.drag(
            &mut fx.b,
            PointerButton::Primary,
            centre - Vec2::new(60.0, 40.0),
            centre + Vec2::new(60.0, 40.0),
        );
        fx.drv.wheel(&mut fx.b, centre, -6.0);
        fx.drv.paste_text(&mut fx.b, "pasted");
        fx.with(Key::Delete, Modifiers::NONE);
        fx.with(Key::W, Modifiers::NONE);
        fx.drv.idle(&mut fx.b, 2);
    };
    let shown = |fx: &Fx| -> Vec<String> {
        fx.drv
            .texts()
            .into_iter()
            .filter(|(r, _, _)| r.left() >= 500.0 && r.top() < 650.0)
            .map(|(_, _, t)| t)
            .collect()
    };
    for (name, first, typed, lands_on) in [
        ("data.bin", "00 01 02 03 04 05 06 07", "4", "data.bin"),
        ("main.rs", "// a comment", "9", "main.rs"),
    ] {
        fx.bar_on(name);
        assert!(fx.on_right(first), "{name}: {first}");
        let before = shown(&fx);
        inputs(&mut fx);
        // A digit is a hex digit to the hex window and text to an editor; here it is the
        // list's jump letter, with no name to jump to. `W` (the text viewer's wrap key) is the
        // list's too: no name starts with it.
        fx.typed(typed);
        assert_eq!(fx.current(), lands_on, "{name}: the bar stayed");
        assert_eq!(shown(&fx), before, "{name}: the preview is as it was");
        assert!(fx.b.viewer.is_none() && fx.b.active == 0, "{name}");
        // Nothing was edited: this very preview — the window that was given all of that, not
        // one prepared anew — is handed over at once, and leaves without a prompt.
        let previewed = fx.b.preview.shown_window().expect("the preview's window");
        let prepared = fx.b.preview.spawned();
        fx.key(Key::Enter);
        let viewer = fx.b.viewer.as_ref().expect("handed over at once");
        assert_eq!(
            &*viewer.embedded.window as *const dyn Window as *const () as usize, previewed,
            "{name}: the viewer is the preview's own window"
        );
        assert_eq!(fx.b.preview.spawned(), prepared, "{name}: nothing new");
        fx.settle();
        fx.key(Key::Escape);
        assert!(fx.drv.find_text("Unsaved changes").is_none(), "{name}");
        assert!(fx.b.viewer.is_none(), "{name}: left without a prompt");
    }
    assert_eq!(
        std::fs::read(format!("{}/data.bin", fx.root)).unwrap()[..4],
        [0, 1, 2, 3]
    );
    assert_eq!(
        std::fs::read_to_string(format!("{}/main.rs", fx.root)).unwrap(),
        RUST
    );
}

/// The list's keys act while a preview shows: a typed letter jumps (the preview takes none).
#[test]
fn a_letter_jumps_while_a_preview_shows() {
    let mut fx = fixture();
    for name in ["data.bin", "main.rs"] {
        fx.bar_on(name);
        assert!(fx.b.preview.shown_window().is_some(), "{name}: previewed");
        fx.typed("z");
        assert_eq!(fx.current(), "zeta.md", "{name}: the list has the keys");
    }
}

/// The window closes while a name-in-the-way question is open: the wait answers it Cancel, and
/// the report printed after the window says the item was cancelled and not copied — never
/// "skipped", which nobody chose.
#[test]
fn closing_at_a_question_reports_the_item_as_cancelled() {
    // Bounded: a wait that never answered the question would block for ever.
    bounded(60, || {
        let mut fx = fixture();
        let alpha = fx.other_into("alpha");
        std::fs::write(format!("{alpha}/notes.txt"), "there\n").unwrap();
        fx.select("notes.txt");
        fx.key(Key::Space);
        fx.key(Key::Space);
        fx.key(Key::F5);
        fx.key(Key::Enter);
        fx.wait(|b| b.ops.holds_keyboard());
        assert!(fx.drv.find_text("Name exists").is_some());
        assert!(Window::close_requested(&mut fx.b));
        let leftover = fx.b.leftover.clone();
        let Fx { b, _tmp, .. } = fx;
        drop(b);
        let waited = leftover.borrow_mut().ops.take().and_then(ops::Ops::wait);
        let lines = report_lines(&waited.expect("the operation was waited for"));
        assert!(
            lines.contains(&"notes.txt: cancelled: nothing of it was copied".to_string()),
            "{lines:?}"
        );
        assert!(lines.iter().all(|l| !l.contains("skipped")), "{lines:?}");
        assert!(
            lines.contains(&"photo.png: not reached".to_string()),
            "{lines:?}"
        );
        assert_eq!(
            std::fs::read_to_string(format!("{alpha}/notes.txt")).unwrap(),
            "there\n"
        );
        drop(_tmp);
    });
}

// ---------------------------------------------------------------------------------------------
// Slice S1-M: the media preview through the browser's own window (02 P4, P5, P6 and P12 for
// video and audio). Time is a clock the test moves, a program that stalls is a stand-in held at
// a named pipe, and processes are counted as the system lists them
// (`crate::gui::media::fixtures`).

use crate::ffmpeg::{spawns, Tools};
use crate::gui::media::fixtures::{
    bounded as watched_over, clip, index_only, median, most_at_once, none_left, quiet_then_loud,
    stand_in, started_under, still_running, tone, tools as media_tools, Census, Hand,
};
use crate::gui::media::wave;

/// Nothing is left under `dir` — by its name as given and as the system resolves it (the tree
/// is listed by its resolved path, the cache by the given one).
fn assert_none_left(dir: &Path) {
    let resolved = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    for dir in [dir, resolved.as_path()] {
        if let Err(left) = none_left(dir) {
            panic!("{left}");
        }
    }
}

/// The folder a fixture's tree and cache are under.
fn base(fx: &Fx) -> PathBuf {
    fx.cache.parent().unwrap().to_path_buf()
}

/// Each preparation of the browser's previews runs `tools`, within 02 §4.3's limits on `hand`.
fn previews_run(fx: &mut Fx, tools: Tools, hand: &Hand) {
    let hand = hand.clone();
    fx.b.preview.set_budget(Box::new(move || {
        let hand = hand.clone();
        prepare::Budget::with_clock(prepare::Limits::PREVIEW, Box::new(move || hand.now()))
            .with_tools(tools.clone())
    }));
}

/// P4: the bar swept over 50 files of every kind, twice. Down, each preview is shown and left.
/// Back up, every video's and every sound's preparation is held alive (its `ffprobe` waits
/// where the test holds it) when the bar moves on, so each of them is really cancelled with a
/// process running. Through all of it at most one child is alive at any moment — by the
/// children's own start and end, and by the system's process list, looked at throughout — none
/// outlives its preview, and nothing is written to the cache. (On a local folder: a child is
/// found by the file it reads, and a remote one reads a URL of its byte server, which names no
/// file. The remote side of P4 — one preview session, never two streams at once — is
/// `w8a_twenty_quick_moves_over_streams_use_one_session` and its image and media kin.)
#[test]
fn p4_a_sweep_over_fifty_files_leaves_no_ffmpeg_and_nothing_in_the_cache() {
    let Some(tools) = media_tools() else { return };
    watched_over(180, move |t| {
        let mut fx = fixture();
        t.also(&base(&fx));
        let root = PathBuf::from(&fx.root);
        clip(&tools, &root.join("v00.mp4"), (160, 120));
        tone(&tools, &root.join("s00.wav"), 3);
        for i in 1..12 {
            std::fs::copy(root.join("v00.mp4"), root.join(format!("v{i:02}.mp4"))).unwrap();
            std::fs::copy(root.join("s00.wav"), root.join(format!("s{i:02}.wav"))).unwrap();
        }
        for i in 0..10 {
            std::fs::write(root.join(format!("t{i:02}.txt")), format!("text {i}\n")).unwrap();
            std::fs::copy(root.join("photo.png"), root.join(format!("p{i:02}.png"))).unwrap();
        }
        fx.relist();
        let files: Vec<String> = fx
            .names(0)
            .into_iter()
            .filter(|n| !["..", "alpha", "beta"].contains(&n.as_str()))
            .collect();
        assert_eq!(files.len(), 50, "{files:?}");
        let media = |name: &str| name.ends_with(".mp4") || name.ends_with(".wav");
        // Every child reads a file in the tree.
        let cache = root.clone();
        let census = Census::start(&cache);

        // Down through all of them, each preview shown.
        fx.select(&files[0]);
        for (i, name) in files.iter().enumerate() {
            assert_eq!(&fx.current(), name);
            fx.rest();
            assert!(!fx.b.preview.busy(), "{name}: settled");
            assert_eq!(
                still_running(&started_under(&cache)),
                0,
                "{name}: a process outlived its preparation"
            );
            if name.ends_with(".mp4") {
                assert!(fx.right_text("160×120").is_some(), "{name}: its frame");
            }
            if name.ends_with(".wav") {
                assert!(
                    fx.right_text("audio · mono").is_some(),
                    "{name}: its waveform"
                );
            }
            if i + 1 < files.len() {
                fx.key(Key::ArrowDown);
            }
        }
        let shown = started_under(&cache).len();
        assert_eq!(
            shown,
            12 * 2 + 12 * 2,
            "each video and each sound: its probe, and its frame or its waveform"
        );
        assert_none_left(&cache);

        // Back up, with every probe held where it starts: each video and each sound has a
        // process alive when the bar leaves it.
        let hold = t.hold(&base(&fx), "hold");
        let held = Tools {
            ffmpeg: tools.ffmpeg.clone(),
            ffprobe: stand_in(&base(&fx), "held-ffprobe", &hold.stalls()),
        };
        previews_run(&mut fx, held, &t.hand());
        let mut cancelled = 0;
        for name in files.iter().rev().skip(1) {
            let before = started_under(&cache).len();
            fx.key(Key::ArrowUp);
            assert_eq!(&fx.current(), name);
            fx.drv.idle(&mut fx.b, REST);
            if media(name) {
                fx.wait(|_| started_under(&cache).len() > before);
                let started = started_under(&cache);
                assert_eq!(started.len(), before + 1, "{name}: its probe, held");
                assert_eq!(
                    still_running(&started),
                    1,
                    "{name}: alive as the bar leaves"
                );
                cancelled += 1;
            }
        }
        // The last held one is left by the bar too: onto `..`.
        fx.key(Key::ArrowUp);
        fx.rest();
        fx.wait(|b| !b.preview.busy());
        assert_eq!(cancelled, 24, "every video and every sound");
        assert_eq!(
            started_under(&cache).len(),
            shown + cancelled,
            "each cancelled at its probe: no frame, no waveform was started for it"
        );
        assert_none_left(&cache);
        assert_eq!(
            most_at_once(&cache),
            1,
            "never two children alive at once, by their own start and end"
        );
        let (most, looks) = census.end();
        assert!(looks > 20, "the process list was looked at: {looks} times");
        assert_eq!(most, 1, "never two in the system's process list");
        let kept = files_under(&fx.cache);
        assert!(kept.is_empty(), "nothing in the cache: {kept:?}");
    });
}

/// P5 (video and audio, in the browser): clicks, a drag, the wheel and Space over a previewed
/// video or sound change nothing — no viewer, nothing played, no ffmpeg started — and the keys
/// are still the list's.
#[test]
fn p5_a_media_preview_takes_no_input_and_the_list_keeps_its_keys() {
    let Some(tools) = media_tools() else { return };
    let mut fx = fixture();
    let root = PathBuf::from(&fx.root);
    clip(&tools, &root.join("clip.mp4"), (160, 120));
    tone(&tools, &root.join("tone.wav"), 3);
    fx.relist();
    let wave_colour = chrome::PALETTE.wave;
    for name in ["clip.mp4", "tone.wav"] {
        fx.bar_on(name);
        let runs = spawns::of(&root.join(name));
        let started = started_under(&root);
        let painted = |fx: &Fx| {
            let wave: Vec<_> = fx
                .drv
                .segments()
                .into_iter()
                .filter(|(_, c)| *c == wave_colour)
                .map(|(line, _)| line)
                .collect();
            (
                fx.drv.pictures(),
                wave,
                fx.right_text("audio · "),
                fx.right_text("160×120"),
            )
        };
        let before = painted(&fx);
        assert!(
            before.2.is_some() || before.3.is_some(),
            "{name}: its preview shows"
        );
        assert!(
            !before.0.is_empty() || before.1.len() > 300,
            "{name}: its frame or its waveform is painted"
        );
        for at in [
            Pos2::new(750.0, 350.0),
            Pos2::new(540.0, 640.0),
            Pos2::new(560.0, 610.0),
            Pos2::new(700.0, 580.0),
        ] {
            fx.drv.click(&mut fx.b, at);
        }
        let centre = Pos2::new(750.0, 350.0);
        fx.drv.drag(
            &mut fx.b,
            PointerButton::Primary,
            centre - Vec2::new(80.0, 60.0),
            centre + Vec2::new(80.0, 60.0),
        );
        fx.drv.wheel(&mut fx.b, centre, -5.0);
        // Space is the list's (it selects the entry), not the preview's Play.
        fx.key(Key::Space);
        fx.drv.idle(&mut fx.b, 3);
        assert_eq!(painted(&fx), before, "{name}: the preview changed");
        assert!(fx.b.viewer.is_none(), "{name}");
        assert_eq!(fx.b.active, 0, "{name}: the keyboard stays in the list");
        assert_eq!(spawns::of(&root.join(name)), runs, "{name}: nothing ran");
        assert_eq!(started_under(&root), started, "{name}: nothing started");
        assert!(fx.drv.find_text("Pause").is_none(), "{name}: not playing");
        fx.key(Key::Space);
    }
    assert!(Window::close_requested(&mut fx.b), "nothing is asked");
    assert_none_left(&base(&fx));
}

/// P6 (video): Enter on a previewed video hands the live preview over — the media window is
/// there at once, with the frame the preview decoded: no second probe, no second decode.
#[test]
fn p6_enter_on_a_previewed_video_hands_it_over_without_decoding_again() {
    let Some(tools) = media_tools() else { return };
    let mut fx = fixture();
    let path = PathBuf::from(format!("{}/clip.mp4", fx.root));
    clip(&tools, &path, (160, 120));
    fx.relist();
    fx.bar_on("clip.mp4");
    let window = fx.b.preview.shown_window().expect("the preview");
    let runs = spawns::of(&path);
    assert_eq!((runs.probe, runs.preview), (1, 1), "one probe, one frame");
    let started = started_under(Path::new(&fx.root));
    fx.key(Key::Enter);
    let viewer = fx.b.viewer.as_ref().expect("the viewer is there at once");
    assert_eq!(
        &*viewer.embedded.window as *const dyn Window as *const () as usize, window,
        "the same window"
    );
    fx.settle();
    fx.drv.idle(&mut fx.b, 3);
    assert!(fx.label("Exit"), "a viewer now: its row");
    assert!(
        fx.drv.find_text("Play").is_some(),
        "the media window's bars"
    );
    assert!(
        fx.drv.pictures().iter().any(|r| r.width() > 160.0),
        "its frame"
    );
    assert_eq!(
        spawns::of(&path),
        runs,
        "nothing was probed or decoded again"
    );
    assert_eq!(started_under(Path::new(&fx.root)), started);
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "clip.mp4");
}

/// P12 (audio): a 20-minute sound previews the waveform of its first 10 minutes, said with the
/// whole duration; Enter → the media window, those 10 minutes kept and the rest decoded from
/// 10:00 — by one more run that starts there.
#[test]
fn p12_a_long_sound_previews_ten_minutes_and_enter_decodes_on_from_there() {
    let Some(tools) = media_tools() else { return };
    let mut fx = fixture();
    let path = PathBuf::from(format!("{}/long.wav", fx.root));
    // Quiet for its first 10 minutes — all the preview reads — and loud from 10:00.
    quiet_then_loud(&tools, &path, 1200, 600);
    fx.relist();
    fx.bar_on("long.wav");
    let said = fx.right_text("audio · mono").expect("the sound's line");
    assert!(
        said.contains("20:00.00") && said.contains("preview of the first 10 minutes"),
        "{said}"
    );
    assert_eq!(wave::starts(&path), [(0.0, Some(600.0))]);
    fx.key(Key::Enter);
    assert!(fx.b.viewer.is_some(), "the viewer is there at once");
    // Ready once the whole waveform is read.
    fx.settle();
    assert!(fx.label("Exit"));
    assert_eq!(
        wave::starts(&path),
        [(0.0, Some(600.0)), (600.0, None)],
        "the first 10 minutes stay; the rest is decoded from 10:00"
    );
    let runs = spawns::of(&path);
    assert_eq!((runs.probe, runs.preview), (1, 2), "{runs:?}");
    // Painted, in the viewer: up to the 10:00 mark — half its width — the quiet the preview
    // read; past it the loud part, decoded after the hand-over.
    fx.drv.idle(&mut fx.b, 2);
    let wave_colour = chrome::PALETTE.wave;
    let mut painted: Vec<(f32, f32)> = fx
        .drv
        .segments()
        .into_iter()
        .filter(|([a, b], c)| *c == wave_colour && a.x == b.x)
        .map(|([a, b], _)| (a.x, (a.y - b.y).abs()))
        .collect();
    painted.sort_by(|a, b| a.0.total_cmp(&b.0));
    assert!(
        painted.len() > 600,
        "the waveform: {} columns",
        painted.len()
    );
    let (left, right) = (painted[0].0, painted[painted.len() - 1].0);
    let mark = (left + right) / 2.0;
    let heights = |from: f32, to: f32| -> Vec<f32> {
        let within = painted.iter().filter(|(x, _)| *x >= from && *x <= to);
        within.map(|(_, h)| *h).collect()
    };
    let before_mark = median(heights(left, mark - 8.0));
    let past_mark = heights(mark + 8.0, right);
    assert!(before_mark > 1.0, "the first 10 minutes: {before_mark}");
    assert!(
        past_mark.iter().all(|h| *h > before_mark * 5.0),
        "past the 10:00 mark every column shows the loud part: {} against {before_mark}",
        median(past_mark.clone())
    );
    fx.key(Key::Escape);
    fx.rest();
    assert_none_left(&base(&fx));
}

/// Lets a held clock go when it is dropped: a test that fails leaves no worker waiting at it.
struct LetGo(Gate, Arc<AtomicBool>);

impl Drop for LetGo {
    fn drop(&mut self) {
        self.1.store(false, Ordering::SeqCst);
        self.0.release();
    }
}

/// 02 §4.4, the window closing while a video is probed (a stand-in `ffprobe` that never
/// answers): nothing waits for the preview's worker, so its ffprobe is stopped as the window
/// goes — by the window, not by the worker's next look (the worker is held where it reads the
/// time) — and closing does not wait for anything.
#[test]
fn closing_the_window_kills_a_previews_ffprobe_at_once() {
    watched_over(180, |t| {
        let mut fx = fixture();
        t.also(&base(&fx));
        let hold = t.hold(&base(&fx), "hold");
        let stalls = stand_in(&base(&fx), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        let gate = Gate::default();
        let held = Arc::new(AtomicBool::new(false));
        let _let_go = LetGo(gate.clone(), held.clone());
        let (g, h) = (gate.clone(), held.clone());
        fx.b.preview.set_budget(Box::new(move || {
            let (g, h) = (g.clone(), h.clone());
            let clock = Box::new(move || {
                if h.load(Ordering::SeqCst) {
                    g.hold();
                }
                std::time::Instant::now()
            });
            prepare::Budget::with_clock(prepare::Limits::PREVIEW, clock).with_tools(tools.clone())
        }));
        let root = PathBuf::from(&fx.root);
        std::fs::write(root.join("hangs.mp4"), b"whatever is in it").unwrap();
        fx.relist();
        fx.select("hangs.mp4");
        fx.drv.idle(&mut fx.b, REST);
        until("the probe starts", || !started_under(&root).is_empty());
        held.store(true, Ordering::SeqCst);
        gate.reached();
        let pids = started_under(&root);
        assert_eq!(still_running(&pids), 1, "the probe runs: {pids:?}");
        assert!(Window::close_requested(&mut fx.b), "nothing is asked");
        let Fx { b, _tmp, .. } = fx;
        // The worker is held where it reads the time: that this returns at all, with the
        // probe stopped, is the window not waiting for it.
        drop(b);
        // (The window only signals: the kill lands a moment after it — waited for, not
        // assumed. The worker is still held: nothing but the window's signal ends the probe.)
        until("stopped with the window", || still_running(&pids) == 0);
        // Its worker, let go, reaps it.
        drop(_let_go);
        assert_none_left(_tmp.path());
    });
}

/// P6 (video): Enter while a video is still being probed (an `ffprobe` the test holds): the
/// same preparation goes on as the viewer's opening — past the probe's time, which the test's
/// clock says is long over — one worker, one probe, and the media window shows when the probe
/// answers.
#[test]
fn p6_enter_during_a_videos_probe_continues_it_as_the_opening() {
    let Some(real) = media_tools() else { return };
    watched_over(180, move |t| {
        let mut fx = fixture();
        t.also(&base(&fx));
        let path = PathBuf::from(format!("{}/clip.mp4", fx.root));
        clip(&real, &path, (160, 120));
        let hold = t.hold(&base(&fx), "hold");
        let tools = Tools {
            ffmpeg: real.ffmpeg.clone(),
            ffprobe: stand_in(&base(&fx), "ffprobe", &hold.then(&real.ffprobe)),
        };
        let hand = t.hand();
        previews_run(&mut fx, tools, &hand);
        fx.relist();
        fx.select("clip.mp4");
        fx.drv.idle(&mut fx.b, REST);
        let root = PathBuf::from(&fx.root);
        until("the probe starts", || !started_under(&root).is_empty());
        fx.key(Key::Enter);
        assert!(fx.b.opening.is_some(), "the preparation is the opening now");
        // Long past a preview's 5 s: the opening has no such time.
        hand.advance(Duration::from_secs(60));
        std::thread::sleep(Duration::from_millis(100));
        fx.drv.idle(&mut fx.b, 3);
        assert!(fx.b.opening.is_some(), "still opening");
        assert_eq!(still_running(&started_under(&root)), 1, "the same probe");
        hold.release();
        fx.wait(|b| b.viewer.is_some());
        fx.settle();
        assert!(fx.drv.find_text("Play").is_some(), "the media window");
        assert_eq!(fx.b.preview.spawned(), 1, "the same worker");
        let runs = spawns::of(&path);
        assert_eq!((runs.probe, runs.preview), (1, 1), "one probe, one frame");
        assert_none_left(&base(&fx));
    });
}

/// The window closes while a preparation taken over by Enter still probes (an `ffprobe` that
/// never answers): the opening is stopped, its ffprobe killed and reaped, and what waits for
/// the worker after the window returns.
#[test]
fn closing_during_a_taken_over_probe_leaves_no_process() {
    watched_over(180, |t| {
        let mut fx = fixture();
        t.also(&base(&fx));
        let hold = t.hold(&base(&fx), "hold");
        let stalls = stand_in(&base(&fx), "ffprobe", &hold.stalls());
        let tools = Tools {
            ffmpeg: stalls.clone(),
            ffprobe: stalls,
        };
        previews_run(&mut fx, tools, &t.hand());
        let root = PathBuf::from(&fx.root);
        std::fs::write(root.join("hangs.mp4"), b"whatever is in it").unwrap();
        fx.relist();
        fx.select("hangs.mp4");
        fx.drv.idle(&mut fx.b, REST);
        until("the probe starts", || !started_under(&root).is_empty());
        fx.key(Key::Enter);
        assert!(fx.b.opening.is_some(), "the preparation is the opening now");
        let pids = started_under(&root);
        assert_eq!(still_running(&pids), 1, "still probing");
        let leftover = fx.b.leftover.clone();
        let Fx { b, _tmp, .. } = fx;
        drop(b);
        // The opening's worker is waited for after the window: it has ended, its ffprobe
        // killed and reaped, when that returns.
        leftover.borrow_mut().finish();
        assert_none_left(_tmp.path());
    });
}

/// A video whose index is whole and whose pictures are missing (the live probe's `head -c` of a
/// faststart MP4): its preview is a card saying there is no frame, and Enter on it does not
/// leave a media window decoding for ever — the open fails, the list is back, and the status
/// row says why, in red.
#[test]
fn enter_on_a_video_with_no_frame_comes_back_to_the_list_with_the_reason() {
    let Some(tools) = media_tools() else { return };
    watched_over(180, move |t| {
        let mut fx = fixture();
        t.also(&base(&fx));
        index_only(&tools, Path::new(&format!("{}/cut.mp4", fx.root)));
        fx.relist();
        fx.bar_on("cut.mp4");
        assert!(fx.on_right("File Info"), "its card");
        let why = fx.right_text("no frame at 0:00.00").expect("why");
        assert!(!why.contains("0x") && !why.contains('['), "{why}");
        fx.key(Key::Enter);
        fx.wait(|b| b.opening.is_none() && b.viewer.is_none() && b.status.error);
        assert!(fx.status().contains("no frame"), "{}", fx.status());
        assert_eq!(fx.current(), "cut.mp4", "back in the list");
        assert_none_left(&base(&fx));
    });
}
