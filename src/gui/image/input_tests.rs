//! The image window driven by real input events (pointer drags, wheel, keys with modifiers,
//! button clicks) through its `ui` — the wiring the owner uses, not the model directly.
//!
//! The test image encodes its own coordinates (red = x, green = y), so where a crop landed is
//! read from the pixels, independent of the screen ↔ image mapping.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Key, Modifiers, PointerButton};
use image::{GenericImageView, Rgb};

const W: u32 = 240;
const H: u32 = 160;

fn coordinates() -> DynamicImage {
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(W, H, |x, y| {
        Rgb([x as u8, y as u8, 0])
    }))
}

struct Fixture {
    tmp: tempfile::TempDir,
    path: PathBuf,
    win: ImageWindow,
    drv: Driver,
}

fn open() -> Fixture {
    open_sized(1000.0, 700.0)
}

fn open_sized(w: f32, h: f32) -> Fixture {
    let bytes = encode(&coordinates(), ImageFormat::Png, None, None)
        .unwrap()
        .bytes;
    open_bytes("shot.png", bytes, w, h)
}

/// A JPEG of the coordinate image carrying XMP (APP1) and a comment (COM): Save keeps neither.
fn xmp_jpeg() -> Vec<u8> {
    let jpeg = encode(&coordinates(), ImageFormat::Jpeg, None, None)
        .unwrap()
        .bytes;
    let mut out = jpeg[..2].to_vec();
    let xmp = [
        b"http://ns.adobe.com/xap/1.0/\0".as_slice(),
        b"<x:xmpmeta/>",
    ]
    .concat();
    for (marker, data) in [(0xE1u8, xmp), (0xFE, b"a comment".to_vec())] {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&data);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn open_bytes(name: &str, bytes: Vec<u8>, w: f32, h: f32) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(name);
    std::fs::write(&path, &bytes).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = ImageFile::new(origin, bytes, &tmp.path().join("cache"));
    let doc = Doc::decode(file.original()).unwrap();
    let win = ImageWindow {
        title: file.origin.display(),
        can_replace: true,
        doc,
        file: Arc::new(Mutex::new(file)),
        tex: None,
        tex_rev: None,
        sel: Selection::default(),
        block: DEFAULT_BLOCK,
        view: View::default(),
        status: Status::default(),
        job: None,
        outcome: Arc::default(),
        prompt: false,
        close_ok: false,
        close_when_done: false,
        leftover: Arc::default(),
        painted: false,
        zoom: 1.0,
        map: None,
        notices: Vec::new(),
        pending_notices: None,
    };
    let mut fx = Fixture {
        tmp,
        path,
        win,
        drv: Driver::new(w, h),
    };
    fx.drv.idle(&mut fx.win, 2);
    fx
}

impl Fixture {
    /// Screen position of image pixel `(x, y)` as last drawn.
    fn at(&self, x: f32, y: f32) -> Pos2 {
        self.win.map.expect("drawn").to_disp(Pos2::new(x, y))
    }

    /// Passes until the image's place on screen stops moving (a panel whose text just changed
    /// resizes over a pass or two, as on screen; how many depends on the fonts).
    fn settle(&mut self) {
        let mut stable = 0;
        for _ in 0..30 {
            let before = self.win.map;
            self.drv.idle(&mut self.win, 1);
            stable = if self.win.map == before {
                stable + 1
            } else {
                0
            };
            if stable >= 3 {
                return;
            }
        }
        panic!("the layout never settled");
    }

    /// A primary-button drag between two image pixels, on a settled layout.
    fn select(&mut self, from: (f32, f32), to: (f32, f32)) {
        self.settle();
        let map = self.win.map;
        let (a, b) = (self.at(from.0, from.1), self.at(to.0, to.1));
        self.drv.drag(&mut self.win, PointerButton::Primary, a, b);
        assert_eq!(self.win.map, map, "the image stayed put during the drag");
    }

    fn rect(&self) -> Option<[u32; 4]> {
        self.win.rect()
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) {
        self.drv.key(&mut self.win, key, modifiers);
    }
}

#[test]
fn drag_selects_image_pixels_at_fit_zoom_and_c_crops_there() {
    let mut fx = open();
    let map = fx.win.map.unwrap();
    // Fitted: the whole image is on screen, aspect kept, scaled up to the panel.
    assert!((map.disp.width() / map.disp.height() - W as f32 / H as f32).abs() < 0.01);
    assert!(map.scale() > 1.0, "scale {}", map.scale());
    fx.select((40.0, 30.0), (100.0, 80.0));
    assert_eq!(fx.rect(), Some([40, 30, 60, 50]));
    fx.key(Key::C, Modifiers::NONE);
    assert_eq!(fx.win.doc.size(), (60, 50));
    assert_eq!(
        fx.win.doc.img.get_pixel(0, 0).0[..2],
        [40, 30],
        "crop origin"
    );
    assert_eq!(fx.rect(), None, "the rectangle goes with the crop");
}

#[test]
fn drag_selects_image_pixels_after_zoom_and_pan() {
    let mut fx = open();
    let fit = fx.win.map.unwrap().scale();
    let centre = fx.at(120.0, 80.0);
    fx.drv.wheel(&mut fx.win, centre, 4.0);
    let zoomed = fx.win.map.unwrap().scale();
    assert!(zoomed > fit * 1.5, "wheel zoomed in: {fit} → {zoomed}");
    // Pan with the secondary button: the image moves with the pointer.
    let before = fx.at(120.0, 80.0);
    fx.drv.drag(
        &mut fx.win,
        PointerButton::Secondary,
        centre,
        centre + Vec2::new(-150.0, -90.0),
    );
    let after = fx.at(120.0, 80.0);
    assert!(
        (after - before - Vec2::new(-150.0, -90.0)).length() < 2.0,
        "{before:?} → {after:?}"
    );
    assert_eq!(fx.rect(), None, "panning does not select");
    fx.select((130.0, 90.0), (150.0, 104.0));
    assert_eq!(fx.rect(), Some([130, 90, 20, 14]));
    fx.key(Key::C, Modifiers::NONE);
    assert_eq!(fx.win.doc.size(), (20, 14));
    assert_eq!(fx.win.doc.img.get_pixel(0, 0).0[..2], [130, 90]);
}

#[test]
fn p_pixelizes_inside_only_and_undo_redo_keys_work() {
    let mut fx = open();
    fx.drv.frame(&mut fx.win, Vec::new(), Modifiers::NONE);
    fx.select((32.0, 32.0), (96.0, 96.0));
    fx.key(Key::P, Modifiers::NONE);
    let img = &fx.win.doc.img;
    // One 16 px cell (32..48) is a single colour, its mean (39.5 rounds to 40); outside the
    // rectangle is as before.
    assert_eq!(img.get_pixel(32, 32), img.get_pixel(47, 47));
    assert_eq!(img.get_pixel(32, 32).0[..2], [40, 40], "mean of 32..=47");
    assert_eq!(img.get_pixel(31, 31).0[..2], [31, 31]);
    assert_eq!(img.get_pixel(96, 96).0[..2], [96, 96]);
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.img.get_pixel(32, 32).0[..2], [32, 32], "undone");
    fx.key(Key::Z, command(true));
    assert_eq!(fx.win.doc.img.get_pixel(33, 33).0[..2], [40, 40], "redone");
}

#[test]
fn escape_clears_and_a_handle_drag_resizes() {
    let mut fx = open();
    fx.select((40.0, 30.0), (100.0, 80.0));
    // The south-east handle sits on the corner; dragging it moves only that corner.
    let corner = fx.at(100.0, 80.0);
    let target = fx.at(120.0, 90.0);
    fx.drv
        .drag(&mut fx.win, PointerButton::Primary, corner, target);
    assert_eq!(fx.rect(), Some([40, 30, 80, 60]));
    // Dragging the inside moves the rectangle, keeping its size.
    let inside = fx.at(60.0, 50.0);
    let moved = fx.at(70.0, 55.0);
    fx.drv
        .drag(&mut fx.win, PointerButton::Primary, inside, moved);
    assert_eq!(fx.rect(), Some([50, 35, 80, 60]));
    fx.key(Key::Escape, Modifiers::NONE);
    assert_eq!(fx.rect(), None);
}

/// The save the last input started has finished. A small save can finish within the passes
/// that delivered the key or click, so "started" is the job running or its result already in.
fn wait_for_save(fx: &mut Fixture) {
    assert!(
        fx.win.job.is_some() || fx.win.status.text.starts_with("saved "),
        "no save started: {}",
        fx.win.status.text
    );
    fx.drv.until(&mut fx.win, 20, |w| w.job.is_none());
}

#[test]
fn save_button_and_command_s_save_to_disk() {
    let mut fx = open();
    let original = std::fs::read(&fx.path).unwrap();
    fx.select((0.0, 0.0), (120.0, 80.0));
    fx.key(Key::C, Modifiers::NONE);
    fx.drv.idle(&mut fx.win, 1);
    fx.drv.click_text(&mut fx.win, "Save");
    wait_for_save(&mut fx);
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    let saved = image::load_from_memory(&std::fs::read(&fx.path).unwrap()).unwrap();
    assert_eq!((saved.width(), saved.height()), (120, 80));
    assert!(!fx.win.doc.dirty());
    // Cmd/Ctrl+S after another edit saves again over our own write.
    fx.select((10.0, 10.0), (60.0, 40.0));
    fx.key(Key::C, Modifiers::NONE);
    fx.key(Key::S, command(false));
    wait_for_save(&mut fx);
    let saved = image::load_from_memory(&std::fs::read(&fx.path).unwrap()).unwrap();
    assert_eq!((saved.width(), saved.height()), (50, 30));
    assert_eq!(saved.get_pixel(0, 0).0[..2], [10, 10]);
    // The original was backed up (once) before the first overwrite.
    fn files(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.is_dir() {
                out.extend(files(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
    let backups = files(&fx.tmp.path().join("cache/backup"));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
}

/// The image must not move under the pointer while a rectangle is drawn. On a narrow window
/// with a long status line, the "rect …" readout that appears with the rectangle wrapped the
/// status bar onto a second line mid-drag, so the image shifted up and the rectangle landed on
/// other pixels than the ones under the pointer (seen on Linux, 2026-11-16).
#[test]
fn image_stays_put_while_a_rectangle_is_drawn_next_to_a_long_status() {
    let mut fx = open_sized(640.0, 480.0);
    fx.settle();
    let quiet = fx.win.map;
    // However long the status and whatever the readouts, the status bar keeps one line.
    fx.win.status = Status::info(format!(
        "saved {} · original backed up at {}",
        "/a/rather/long/path".repeat(4),
        "/the/backup/path".repeat(4)
    ));
    fx.select((10.0, 10.0), (60.0, 40.0));
    assert_eq!(fx.rect(), Some([10, 10, 50, 30]));
    fx.settle();
    assert_eq!(fx.win.map, quiet, "the status bar changed height");
}

/// What Save will not keep is never cut off: on a 420 px window with a long disclosure the
/// whole text is laid out (its own row, wrapping), and that row does not move the image while a
/// rectangle is drawn. After Save, the result's "not kept" lands in that row too, whole, and the
/// one-line status says only "saved …".
#[test]
fn loss_disclosure_is_shown_whole_on_a_narrow_window_and_the_image_stays_put() {
    let mut fx = open_bytes("x.jpg", xmp_jpeg(), 420.0, 560.0);
    fx.win.doc.lost.push(
        "ICC profile not kept: a long reason that will not fit on one line of a narrow window"
            .to_string(),
    );
    fx.settle();
    let line = fx.win.doc.not_kept_line().unwrap();
    assert!(
        line.starts_with("Save will not keep: XMP, comment"),
        "{line}"
    );
    assert!(fx.drv.find_text(&line).is_some(), "shown whole: {line}");
    let quiet = fx.win.map;
    fx.select((10.0, 10.0), (60.0, 40.0)); // asserts the image stayed put during the drag
    fx.settle();
    assert_eq!(fx.win.map, quiet, "the image moved");
    assert_eq!(fx.rect(), Some([10, 10, 50, 30]));

    fx.key(Key::P, Modifiers::NONE);
    fx.key(Key::S, command(false));
    wait_for_save(&mut fx);
    fx.settle();
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    assert!(
        !fx.win.status.text.contains("not kept"),
        "{}",
        fx.win.status.text
    );
    assert!(
        fx.drv.find_text("not kept: XMP, comment").is_some(),
        "the save result's disclosure is shown whole"
    );
}
