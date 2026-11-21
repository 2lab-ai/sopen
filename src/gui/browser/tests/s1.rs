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

    /// Passes until `done` (at most 20 s), then two more.
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
    /// The files `read_to` was started on, in order.
    reads: Vec<String>,
    live: usize,
    peak: usize,
}

type Shared = Arc<Mutex<Seen>>;

/// The local disk posing as a remote that counts its reads; a read of a file named `hold` stops
/// at the gate after its first chunk.
struct Watching {
    fs: LocalFs,
    seen: Shared,
    gate: Gate,
    hold: &'static str,
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
}

fn watching(seen: &Shared, gate: &Gate, hold: &'static str) -> Connect {
    let (seen, gate) = (seen.clone(), gate.clone());
    Arc::new(move || {
        seen.lock().unwrap().connects += 1;
        Ok(Box::new(Watching {
            fs: LocalFs::new(),
            seen: seen.clone(),
            gate: gate.clone(),
            hold,
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

/// The window's preview copies: the files under `<cache>/.preview/<window>/`.
fn preview_copies(cache: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for window in std::fs::read_dir(cache.join(".preview"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if window.path().is_dir() {
            out.extend(
                std::fs::read_dir(window.path())
                    .unwrap()
                    .flatten()
                    .map(|e| e.path()),
            );
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// 02 §4.5: the preview.

/// P1: the window opens in preview mode, the bar on `..`, slot 9 reading `Dual`; the bar resting
/// on a file shows it on the other side — text, an image, hex rows, and a video as its info card
/// (S1) — in a single-lined frame titled with the file; the side's own list is back when the bar
/// rests on a folder.
#[test]
fn p1_the_other_side_previews_the_file_under_the_bar() {
    let mut fx = fixture();
    std::fs::write(format!("{}/clip.mp4", fx.root), b"not really a video").unwrap();
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

    fx.bar_on("clip.mp4");
    assert!(fx.on_right("File Info"), "a video is an info card in S1");
    let why = fx.right_text("no preview of video").expect("why");
    assert!(why.ends_with("Enter opens it"), "{why}");

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
        seen.lock().unwrap().reads.is_empty(),
        "nothing is read while the bar moves"
    );
    fx.rest();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.reads, [format!("{}/photo.png", fx.root)]);
    assert_eq!(seen.connects, 1, "one preview connection");
    assert_eq!(fx.b.preview.spawned(), 1, "one preparation");
}

/// P3: a remote file over the read budget is its info card, decided from the stat: nothing of
/// it is read.
#[test]
fn p3_a_file_over_the_preview_limit_is_its_card_and_nothing_is_read() {
    let (mut fx, seen, _gate) = watched("");
    let big = std::fs::File::create(format!("{}/huge.bin", fx.root)).unwrap();
    big.set_len(33 << 20).unwrap();
    fx.relist();
    fx.bar_on("huge.bin");
    assert!(fx.on_right("File Info"));
    assert_eq!(
        fx.right_text("over the preview limit").as_deref(),
        Some("over the preview limit: Enter opens it")
    );
    assert!(seen.lock().unwrap().reads.is_empty(), "nothing was read");
}

/// P4: the bar leaves a preview in the middle of its read: the read is cancelled, the side
/// keeps what it showed until the commit, the next preparation starts only once the first
/// worker has ended (never two at once, one session), and nothing of the cancelled one is left
/// in the cache.
#[test]
fn p4_leaving_a_preview_mid_read_cancels_it_and_the_next_waits_its_turn() {
    let (mut fx, seen, gate) = watched("big.bin");
    std::fs::write(format!("{}/big.bin", fx.root), vec![7u8; 2 << 20]).unwrap();
    fx.relist();
    fx.select("big.bin");
    fx.drv.idle(&mut fx.b, REST);
    gate.reached();
    fx.drv.idle(&mut fx.b, 2);
    assert!(
        fx.right_text("reading big.bin").is_some(),
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
    assert_eq!(seen.lock().unwrap().reads.len(), 1);
    gate.release();
    fx.rest();
    assert!(fx.on_right("plain notes"), "then the next preview shows");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.peak, 1, "never two reads at once");
    assert_eq!(seen.connects, 1, "one preview session");
    assert_eq!(fx.b.preview.spawned(), 2);
    let copies = preview_copies(&fx.cache);
    assert_eq!(copies.len(), 1, "only the shown preview's copy: {copies:?}");
    let kept: Vec<String> = files_under(&fx.cache)
        .into_iter()
        .filter(|f| !f.starts_with(".preview"))
        .collect();
    assert!(kept.is_empty(), "a preview publishes nothing: {kept:?}");
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
/// once, the file is not read again, and the cached copy is the preview's own copy, linked.
#[test]
fn p6_enter_on_a_previewed_file_hands_the_preview_over() {
    let (mut fx, seen, _gate) = watched("");
    fx.bar_on("photo.png");
    let copies = preview_copies(&fx.cache);
    assert_eq!(copies.len(), 1, "{copies:?}");
    fx.key(Key::Enter);
    assert!(fx.b.viewer.is_some(), "the viewer is there at once");
    fx.settle();
    assert!(fx.label("Exit"), "a viewer now: its row");
    assert_eq!(seen.lock().unwrap().reads.len(), 1, "read once");
    let cached = crate::cache::cache_path(&fx.cache, "box", &format!("{}/photo.png", fx.root));
    let (a, b) = (
        std::fs::metadata(&cached).expect("claimed in the cache"),
        std::fs::metadata(&copies[0]).unwrap(),
    );
    assert_eq!(a.ino(), b.ino(), "published by a hard link");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "photo.png");
}

/// P6: Enter while the preview still reads: the same read goes on as the viewer's opening — its
/// progress on the status row, no second read — and the viewer shows when it ends.
#[test]
fn p6_enter_during_a_previews_read_continues_that_read_as_the_opening() {
    let (mut fx, seen, gate) = watched("big.bin");
    std::fs::write(format!("{}/big.bin", fx.root), vec![7u8; 2 << 20]).unwrap();
    fx.relist();
    fx.select("big.bin");
    fx.drv.idle(&mut fx.b, REST);
    gate.reached();
    fx.key(Key::Enter);
    let shown = fx.status();
    assert!(
        shown.starts_with("fetching big.bin ") && shown.ends_with("Esc cancels"),
        "{shown}"
    );
    gate.release();
    fx.wait(|b| b.viewer.is_some());
    fx.settle();
    assert_eq!(seen.lock().unwrap().reads.len(), 1, "the same read");
    assert_eq!(fx.b.preview.spawned(), 1, "the same worker");
    assert!(fx.label("Exit"), "the viewer's row");
    assert!(
        fx.drv.find_text("Text Viewer").is_some(),
        "the file's viewer"
    );
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

/// 02 §4.4: Enter on a file that shows an info card opens it as Enter does from the list; a
/// card's complete private copy is published instead of read again.
#[test]
fn enter_on_an_info_card_opens_the_file() {
    let (mut fx, seen, _gate) = watched("");
    // An image the preview reads whole and then refuses by its header (over 64 megapixels).
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
    std::fs::write(format!("{}/wide.png", fx.root), header).unwrap();
    fx.relist();
    fx.bar_on("wide.png");
    assert_eq!(
        fx.right_text("image is").as_deref(),
        Some("image is 30000×30000; Enter opens it")
    );
    fx.key(Key::Enter);
    fx.wait(|b| b.opening.is_none());
    // The ordinary open: it fails as Enter on it fails in the list (the image does not decode),
    // said on the status row; the card's copy was published, not fetched again.
    assert!(fx.b.viewer.is_none());
    assert!(fx.b.status.error, "{}", fx.b.status.text);
    // The preview is back once the opening is over (the bar is still on the file): its read is
    // the second; the opening itself read nothing.
    fx.rest();
    assert_eq!(
        seen.lock().unwrap().reads.len(),
        2,
        "the opening read nothing"
    );
    let cached = crate::cache::cache_path(&fx.cache, "box", &format!("{}/wide.png", fx.root));
    assert!(cached.exists(), "the card's copy was published");
}

/// 02 §4.4: the window closes while a preview reads: the read is cancelled and the window does
/// not wait for its worker.
#[test]
fn closing_the_window_does_not_wait_for_a_preview() {
    let (mut fx, _seen, gate) = watched("big.bin");
    std::fs::write(format!("{}/big.bin", fx.root), vec![7u8; 2 << 20]).unwrap();
    fx.relist();
    fx.select("big.bin");
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
    // Its copy goes with the worker's end.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !preview_copies(&cache).is_empty() {
        assert!(std::time::Instant::now() < deadline, "the copy was left");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(_tmp);
}

/// A cache that cannot hold the preview folder: the side says why, in place of the preview —
/// never a blank side, never a panic — and Enter still opens the file.
#[test]
fn a_preview_space_that_cannot_be_made_is_said_in_the_side() {
    let mut fx = fixture();
    // The cache root is a file: no `.preview` folder can be made in it.
    std::fs::write(&fx.cache, b"in the way").unwrap();
    fx.bar_on("notes.txt");
    let said = fx
        .right_text("no preview:")
        .expect("the reason, in the side");
    assert!(said.len() > "no preview:".len() + 4, "{said}");
    assert!(
        !fx.on_right("alpha"),
        "the side is the preview's, not the list"
    );
    assert!(fx.b.viewer.is_none());
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
    let untouched = |why: &str| {
        assert_eq!(
            std::fs::read_to_string(&there).unwrap(),
            "what was there\n",
            "{why}"
        );
        assert_eq!(std::fs::metadata(&there).unwrap().ino(), before, "{why}");
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
    fx.wait(|b| !b.ops.busy());
    let line = fx.drv.text_starting_with("skipped").expect("the report");
    assert!(line.ends_with("notes.txt"), "{line}");
    untouched("Skip");
    fx.key(Key::Enter);
    assert!(!fx.b.ops.holds_keyboard(), "the report closed");

    ask(&mut fx);
    assert!(fx.drv.find_text("notes-1.txt").is_some(), "the free name");
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy());
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
    fx.wait(|b| !b.ops.busy());
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
    fx.select("alpha");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    fx.wait(|b| !b.ops.busy());
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
        // Held until Esc reaches the engine's end of the stop flag.
        let flag = link.stop_flag();
        while !flag.load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
        saw.store(true, Ordering::Relaxed);
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
    fx.wait(|b| !b.ops.busy());
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
    fx.wait(|b| !b.ops.busy());
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
    fx.wait(|b| !b.ops.busy());
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
    fx.wait(|b| !b.ops.busy());
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
    fx.wait(|b| !b.ops.busy());
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

/// 02 §6: closing the window during a file operation waits for it — it is not stopped: the
/// copy is there once [`open`]'s wait is over.
#[test]
fn closing_the_window_waits_for_a_running_operation() {
    let mut fx = fixture();
    let alpha = fx.other_into("alpha");
    let gate = Gate::default();
    fx.b.set_ops_connect(gated_ops(&gate));
    fx.select("main.rs");
    fx.key(Key::F5);
    fx.key(Key::Enter);
    gate.reached();
    assert!(Window::close_requested(&mut fx.b));
    let leftover = fx.b.leftover.clone();
    let Fx { b, _tmp, .. } = fx;
    drop(b);
    assert!(!Path::new(&format!("{alpha}/main.rs")).exists());
    gate.release();
    leftover.borrow_mut().finish();
    assert_eq!(
        std::fs::read_to_string(format!("{alpha}/main.rs")).unwrap(),
        RUST,
        "the operation ran to its end"
    );
    drop(_tmp);
}
