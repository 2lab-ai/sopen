//! The image window driven by real input events (pointer drags, wheel, keys with modifiers,
//! button clicks) through its `ui` — the wiring the owner uses, not the model directly.
//!
//! The test image encodes its own coordinates (red = x, green = y), so where a crop landed is
//! read from the pixels, independent of the screen ↔ image mapping.

use super::*;
use crate::gui::testkit::{command, key_up, Driver};
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
    open_noticed(name, bytes, w, h, Vec::new())
}

/// As `open` does it, with `notices` (where earlier copies' unsaved edits went).
fn open_noticed(name: &str, bytes: Vec<u8>, w: f32, h: f32, notices: Vec<String>) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(name);
    std::fs::write(&path, &bytes).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = ImageFile::new(origin, bytes, &tmp.path().join("cache"));
    let doc = Doc::decode(file.original()).unwrap();
    let win = ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, notices);
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

    /// A primary-button drag between two image pixels, on a settled layout. The image must not
    /// move under the pointer: checked after the press, after every held move, and after the
    /// release.
    fn select(&mut self, from: (f32, f32), to: (f32, f32)) {
        self.settle();
        let map = self.win.map;
        let (a, b) = (self.at(from.0, from.1), self.at(to.0, to.1));
        let mut step = 0;
        self.drv
            .drag_watching(&mut self.win, PointerButton::Primary, a, b, |w| {
                assert_eq!(
                    w.map, map,
                    "the image moved under the held pointer (step {step})"
                );
                step += 1;
            });
        assert_eq!(
            self.win.map, map,
            "the image stayed put through the release"
        );
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
        fx.win.saving_or_queued() || fx.win.status.text.starts_with("saved "),
        "no save started: {}",
        fx.win.status.text
    );
    fx.drv.until(&mut fx.win, 20, |w| !w.saving_or_queued());
}

#[test]
fn save_button_and_command_s_save_to_disk() {
    let mut fx = open();
    let original = std::fs::read(&fx.path).unwrap();
    fx.select((0.0, 0.0), (120.0, 80.0));
    fx.key(Key::C, Modifiers::NONE);
    fx.drv.idle(&mut fx.win, 1);
    // The toolbar's Save, the highest: the F-key row's 5Save (cells) reads `Save` too.
    let save = (fx.drv.texts().into_iter())
        .filter(|(_, _, t)| t == "Save")
        .map(|(r, _, _)| r)
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("the toolbar's Save");
    let row = fx.drv.find_text("Exit").expect("the F-key row");
    assert!(
        save.bottom() <= row.top(),
        "the toolbar's, not the F-key row's"
    );
    fx.drv.click(&mut fx.win, save.center());
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
    let backups = files(&crate::cache::backup_root(&fx.tmp.path().join("cache")));
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

/// A save result that lands while the button is held: the picture stays put (its disclosure row
/// would shrink the picture's panel) until the rectangle is let go; then the row appears, whole.
#[test]
fn a_save_result_landing_mid_drag_waits_for_the_release() {
    let mut fx = open();
    fx.settle();
    let map = fx.win.map;
    let note = "not kept: XMP, comment".to_string();
    // The result is handed in by the test, so it lands exactly while the button is down.
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        rev: fx.win.doc.rev(),
        rx,
    });
    let (a, b) = (fx.at(10.0, 10.0), fx.at(90.0, 60.0));
    fx.drv.move_to(&mut fx.win, a);
    fx.drv.press(&mut fx.win, PointerButton::Primary);
    fx.drv.move_to(&mut fx.win, b);
    assert!(fx.win.sel.dragging());
    tx.send(Ok(JobDone::Saved(Saved::Written {
        remote: "box:/shot.png".to_string(),
        backup: PathBuf::from("/backup/shot.png"),
        notes: Vec::new(),
        not_kept: vec![note.clone()],
    })))
    .unwrap();
    for pass in 0..5 {
        fx.drv.idle(&mut fx.win, 1);
        assert_eq!(
            fx.win.map, map,
            "the image moved under the held pointer ({pass})"
        );
    }
    assert!(
        fx.win.job.is_none(),
        "the result was taken while the button was held"
    );
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    assert!(
        fx.drv.find_text(&note).is_none(),
        "not while the button is held"
    );
    fx.drv.release(&mut fx.win, PointerButton::Primary);
    fx.settle();
    assert!(
        fx.drv.find_text(&note).is_some(),
        "shown whole after the release"
    );
    assert_eq!(fx.rect(), Some([10, 10, 80, 50]));
}

/// The busy spinner never moves the image: with a save running the image sits where it sits
/// with none, at every width — including 637–664 px, where a spinner in the toolbar used to wrap
/// it onto a second line (640 px is the default width for any image up to 640 px). It is in the
/// status bar now, which it must not make taller.
#[test]
fn a_running_save_never_moves_the_image_at_any_width() {
    let mut moved = Vec::new();
    for w in (600..=700).chain((480..=1400).step_by(20)) {
        let mut fx = open_sized(w as f32, 700.0);
        fx.settle();
        let idle = fx.win.map;
        let (_tx, rx) = mpsc::channel();
        fx.win.job = Some(Job {
            kind: JobKind::SaveCopy { close_after: false },
            rev: fx.win.doc.rev(),
            rx,
        });
        fx.settle();
        if fx.win.map != idle {
            moved.push(w);
        }
    }
    assert!(moved.is_empty(), "the image moved at widths {moved:?}");
}

/// The toolbar is one row tall wherever its buttons fit on one row: nothing hidden while idle
/// wraps alone onto a second, empty-looking row (the save spinner's reserved slot did, just
/// past the widths where "Save copy" fits — 640 px, the default width for any image up to
/// 640 px, among them).
#[test]
fn the_toolbar_is_one_row_tall_wherever_its_buttons_fit_on_one() {
    // A short window fits the picture by height, so its top is the toolbar's bottom edge.
    let measure = |w: f32| {
        let mut fx = open_sized(w, 300.0);
        fx.settle();
        let disp = fx.win.map.expect("drawn").disp;
        assert!(
            disp.left() > 1.0,
            "fitted by width at {w} px, not by height"
        );
        let row = |label: &str| {
            fx.drv
                .find_text(label)
                .unwrap_or_else(|| panic!("no {label:?} on screen at {w} px"))
                .center()
                .y
        };
        let one_line = (row("Crop  C") - row("Save copy")).abs() < 0.5;
        (disp.top(), one_line)
    };
    // Wide enough for anything to fit on one row.
    let (one_row, one_line) = measure(3000.0);
    assert!(one_line);
    let (mut fits, mut taller) = (Vec::new(), Vec::new());
    for w in (600..=700).chain((480..=1400).step_by(20)) {
        let (top, one_line) = measure(w as f32);
        if one_line {
            fits.push(w);
            if (top - one_row).abs() > 0.5 {
                taller.push(w);
            }
        }
    }
    assert!(
        fits.contains(&640) && fits.contains(&1280),
        "the buttons fit on one row only at {fits:?}"
    );
    assert!(
        taller.is_empty(),
        "the toolbar is more than one row tall at widths {taller:?}"
    );
}

/// While a save runs, its spinner turns in the status bar, just before its "saving…" and on its
/// line; at rest none is painted there.
#[test]
fn a_running_save_spins_beside_its_status() {
    let text = "saving a copy…";
    // The status text, and the arcs painted just before it on its line (within a spacing and a
    // spinner's width: where on its circle the arc is depends on the time).
    let status = |fx: &Fixture| {
        let at = fx.drv.find_text(text).expect("status shown");
        let arcs: Vec<Rect> = fx
            .drv
            .strokes()
            .into_iter()
            .filter(|r| r.right() <= at.left() && r.left() >= at.left() - 30.0)
            .filter(|r| r.max.y >= at.min.y && r.min.y <= at.max.y)
            .collect();
        (at, arcs)
    };
    let mut fx = open_sized(640.0, 420.0);
    fx.win.status = Status::info(text);
    fx.settle();
    let (idle, arcs) = status(&fx);
    assert!(arcs.is_empty(), "a spinner at rest: {arcs:?}");
    let (_tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::SaveCopy { close_after: false },
        rev: fx.win.doc.rev(),
        rx,
    });
    fx.settle();
    let (busy, arcs) = status(&fx);
    assert_eq!(arcs.len(), 1, "one spinner painted before the status");
    assert!(
        busy.left() > idle.left() + 10.0,
        "nothing before the status: {idle:?} → {busy:?}"
    );
    assert!((busy.center().y - idle.center().y).abs() < 0.5);
}

/// Held from the press, not from the drag: a save result that lands after the button went down
/// but before the pointer moved far enough to count as a drag still waits for the release.
#[test]
fn a_save_result_landing_between_press_and_drag_waits_for_the_release() {
    let mut fx = open();
    fx.settle();
    let map = fx.win.map;
    let note = "not kept: XMP, comment".to_string();
    let (tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        rev: fx.win.doc.rev(),
        rx,
    });
    let a = fx.at(10.0, 10.0);
    fx.drv.move_to(&mut fx.win, a);
    fx.drv.press(&mut fx.win, PointerButton::Primary);
    assert!(!fx.win.sel.dragging(), "not a drag yet");
    tx.send(Ok(JobDone::Saved(Saved::Written {
        remote: "box:/shot.png".to_string(),
        backup: PathBuf::from("/backup/shot.png"),
        notes: Vec::new(),
        not_kept: vec![note.clone()],
    })))
    .unwrap();
    for pass in 0..5 {
        fx.drv.idle(&mut fx.win, 1);
        assert_eq!(
            fx.win.map, map,
            "the image moved under the pressed pointer ({pass})"
        );
    }
    assert!(
        fx.win.job.is_none(),
        "the result was taken while the button was down"
    );
    fx.drv.release(&mut fx.win, PointerButton::Primary);
    fx.settle();
    assert!(
        fx.drv.find_text(&note).is_some(),
        "shown whole after the release"
    );
}

/// Where an earlier copy's unsaved edits went is read whole on open, at the default width for
/// a small image (640 px), in the always-visible rows — and stays there after a save reports;
/// the status line stays free for short news.
#[test]
fn the_kept_copy_notice_is_shown_whole_on_open_and_stays() {
    let notice = "the previous copy had unsaved edits; kept them at <cache>/example-host/srv/\
                  shared/archive/photographs/2026/november/IMG_0001.png.conflict-20261116T101010Z"
        .to_string();
    let bytes = encode(&coordinates(), ImageFormat::Png, None, None)
        .unwrap()
        .bytes;
    let mut fx = open_noticed("shot.png", bytes, 640.0, 560.0, vec![notice.clone()]);
    fx.settle();
    assert!(fx.drv.find_text(&notice).is_some(), "shown whole on open");
    assert!(fx.win.status.text.is_empty(), "{}", fx.win.status.text);
    fx.select((0.0, 0.0), (120.0, 80.0));
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
        fx.drv.find_text(&notice).is_some(),
        "still shown after the save"
    );
}

/// Closing during a save on a narrow window: that the window closes when the save is done is
/// read whole.
#[test]
fn the_close_during_save_hint_is_shown_whole_on_a_narrow_window() {
    let mut fx = open_sized(420.0, 560.0);
    let (_tx, rx) = mpsc::channel();
    fx.win.job = Some(Job {
        kind: JobKind::Save { close_after: false },
        rev: fx.win.doc.rev(),
        rx,
    });
    use crate::gui::Window;
    assert!(!fx.win.close_requested());
    fx.settle();
    assert!(
        fx.drv.find_text(CLOSES_AFTER_SAVE).is_some(),
        "shown whole: {CLOSES_AFTER_SAVE}"
    );
}

// ---------------------------------------------------------------------------------------------
// The clipboard (spec §1 message 12): Cmd/Ctrl+C, X and V arrive as egui-winit sends them
// ([`Driver::copy`] and the rest), and what goes to the system clipboard is read off the
// commands eframe hands back to egui-winit.

const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

/// A `w`×`h` picture, blue on its left half and red on its right, as egui-winit hands a
/// pasted image over.
fn halves(w: u32, h: u32) -> egui::ColorImage {
    let img =
        image::RgbaImage::from_fn(w, h, |x, _| image::Rgba(if x < w / 2 { BLUE } else { RED }));
    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw())
}

/// The images the passes since the last call put on the clipboard.
fn copied_images(fx: &mut Fixture) -> Vec<egui::ColorImage> {
    fx.drv
        .take_commands()
        .into_iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyImage(image) => Some(image),
            _ => None,
        })
        .collect()
}

fn pixel(img: &egui::ColorImage, x: usize, y: usize) -> [u8; 4] {
    img.pixels[y * img.width() + x].to_srgba_unmultiplied()
}

/// Every pixel of `img` outside `rect` is as in `before`.
fn untouched_outside(img: &DynamicImage, before: &DynamicImage, [rx, ry, rw, rh]: [u32; 4]) {
    for (x, y, p) in img.pixels() {
        let inside = (rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y);
        if !inside {
            assert_eq!(
                p,
                before.get_pixel(x, y),
                "({x},{y}) is outside {rx},{ry} {rw}×{rh}"
            );
        }
    }
}

/// Cmd/Ctrl+C with a rectangle puts exactly its pixels on the clipboard as an image; the
/// picture is not touched (C alone crops — Cmd/Ctrl+C must not) and the rectangle stays.
#[test]
fn command_c_copies_the_rectangle_as_an_image() {
    let mut fx = open();
    fx.select((40.0, 30.0), (100.0, 80.0));
    let before = fx.win.doc.img.clone();
    fx.drv.take_commands();
    fx.drv.copy(&mut fx.win);
    let copied = copied_images(&mut fx);
    assert_eq!(copied.len(), 1, "one image to the clipboard");
    assert_eq!(copied[0].size, [60, 50]);
    assert_eq!(pixel(&copied[0], 0, 0), [40, 30, 0, 255]);
    assert_eq!(pixel(&copied[0], 59, 49), [99, 79, 0, 255]);
    assert_eq!(fx.win.doc.img, before, "not cropped, not changed");
    assert!(!fx.win.doc.can_undo());
    assert_eq!(fx.rect(), Some([40, 30, 60, 50]), "still selected");
    assert_eq!(fx.win.status.text, "copied 60×50");
}

#[test]
fn command_c_without_a_rectangle_copies_the_whole_image() {
    let mut fx = open();
    fx.drv.take_commands();
    fx.drv.copy(&mut fx.win);
    let copied = copied_images(&mut fx);
    assert_eq!(copied.len(), 1);
    assert_eq!(copied[0].size, [W as usize, H as usize]);
    assert_eq!(pixel(&copied[0], 239, 159), [239, 159, 0, 255]);
    assert_eq!(fx.win.status.text, "copied 240×160");
}

/// Cmd/Ctrl+X: the rectangle's pixels go to the clipboard and the rectangle — only it — is left
/// in the background colour (white by default); Cmd/Ctrl+Z brings back exactly what it had.
#[test]
fn command_x_cuts_the_rectangle_to_the_clipboard_and_leaves_the_background() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.take_commands();
    fx.drv.cut(&mut fx.win);
    let copied = copied_images(&mut fx);
    assert_eq!(copied.len(), 1, "one image to the clipboard");
    assert_eq!(copied[0].size, [60, 50]);
    assert_eq!(pixel(&copied[0], 0, 0), [40, 30, 0, 255]);
    let img = &fx.win.doc.img;
    for (x, y) in [(40, 30), (99, 79), (70, 55)] {
        assert_eq!(img.get_pixel(x, y).0, WHITE, "({x},{y})");
    }
    untouched_outside(img, &before, [40, 30, 60, 50]);
    assert_eq!(img.color(), before.color(), "pixel type kept");
    assert_eq!(fx.win.status.text, "cut 60×50");
    assert_eq!(fx.rect(), Some([40, 30, 60, 50]), "still selected");
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.img, before, "undo restores the exact pixels");
}

#[test]
fn command_x_without_a_rectangle_cuts_nothing() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.drv.take_commands();
    fx.drv.cut(&mut fx.win);
    assert!(copied_images(&mut fx).is_empty());
    assert_eq!(fx.win.doc.img, before);
    assert!(!fx.win.doc.can_undo());
    assert_eq!(fx.win.status.text, "draw a rectangle first");
}

/// Cmd/Ctrl+V with an image on the clipboard stretches it over the rectangle (the default
/// choice), one undo step.
#[test]
fn command_v_stretches_a_pasted_image_over_the_rectangle_by_default() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.paste_image(&mut fx.win, halves(30, 25));
    let img = &fx.win.doc.img;
    assert_eq!(img.get_pixel(40, 30).0, BLUE);
    assert_eq!(img.get_pixel(41, 78).0, BLUE);
    assert_eq!(img.get_pixel(98, 31).0, RED);
    assert_eq!(img.get_pixel(99, 79).0, RED);
    untouched_outside(img, &before, [40, 30, 60, 50]);
    assert_eq!(img.color(), before.color(), "pixel type kept");
    assert_eq!(fx.win.status.text, "pasted image 30×25 → 60×50 (stretch)");
    assert_eq!(fx.rect(), Some([40, 30, 60, 50]), "still selected");
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.img, before, "one undo step, exact pixels back");
}

/// "Fit" (a toolbar choice): the picture keeps its aspect, centred, and the rest of the
/// rectangle gets the background colour.
#[test]
fn the_fit_choice_keeps_the_aspect_and_fills_the_rest_with_the_background() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.settle();
    fx.drv.click_text(&mut fx.win, "Fit");
    fx.select((40.0, 30.0), (100.0, 80.0));
    // 30×10 into 60×50: 60×20, rows 45..65.
    fx.drv.paste_image(&mut fx.win, halves(30, 10));
    let img = &fx.win.doc.img;
    assert_eq!(img.get_pixel(41, 31).0, WHITE, "above the picture");
    assert_eq!(img.get_pixel(41, 44).0, WHITE, "above the picture");
    assert_eq!(img.get_pixel(41, 45).0, BLUE);
    assert_eq!(img.get_pixel(98, 64).0, RED);
    assert_eq!(img.get_pixel(98, 65).0, WHITE, "below the picture");
    assert_eq!(img.get_pixel(99, 79).0, WHITE, "below the picture");
    untouched_outside(img, &before, [40, 30, 60, 50]);
    assert_eq!(fx.win.status.text, "pasted image 30×10 → 60×20 (fit)");
}

/// "1:1": unscaled at the rectangle's top-left, cut off at its edges; what it does not cover
/// keeps its pixels.
#[test]
fn the_one_to_one_choice_pastes_unscaled_at_the_top_left_clipped_to_the_rectangle() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.settle();
    fx.drv.click_text(&mut fx.win, "1:1");
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.paste_image(&mut fx.win, halves(80, 20));
    let img = &fx.win.doc.img;
    assert_eq!(img.get_pixel(40, 30).0, BLUE);
    assert_eq!(img.get_pixel(79, 49).0, BLUE);
    assert_eq!(img.get_pixel(80, 30).0, RED);
    assert_eq!(img.get_pixel(99, 49).0, RED);
    // Below the picture, inside the rectangle: as before.
    untouched_outside(img, &before, [40, 30, 60, 20]);
    assert_eq!(fx.win.status.text, "pasted image 80×20 → 60×20 (1:1)");
}

/// No rectangle: the picture goes 1:1 at the image's top-left, cut off at its edges, and the
/// pasted area becomes the selection.
#[test]
fn command_v_without_a_rectangle_pastes_at_the_top_left_and_selects_it() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.drv.paste_image(&mut fx.win, halves(300, 50));
    let img = &fx.win.doc.img;
    assert_eq!(img.get_pixel(0, 0).0, BLUE);
    assert_eq!(img.get_pixel(149, 49).0, BLUE);
    assert_eq!(img.get_pixel(150, 0).0, RED);
    assert_eq!(img.get_pixel(239, 49).0, RED);
    untouched_outside(img, &before, [0, 0, 240, 50]);
    assert_eq!(
        fx.rect(),
        Some([0, 0, 240, 50]),
        "the pasted area is selected"
    );
    assert_eq!(fx.win.status.text, "pasted image 300×50 → 240×50 (1:1)");
}

/// Text on the clipboard is drawn into the rectangle in the text colour (black by default) —
/// nothing outside it changes — and is one undo step.
#[test]
fn command_v_draws_pasted_text_inside_the_rectangle_only() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.select((40.0, 30.0), (200.0, 120.0));
    fx.drv.paste_text(&mut fx.win, "sopen 한글");
    // Exactly what the text drawing makes of it (its own tests check the glyphs).
    let mut expected = before.clone();
    let faces: Vec<&fonts::Face> = fonts::hangul()
        .into_iter()
        .chain([fonts::fallback()])
        .collect();
    let rect = [40, 30, 160, 90];
    clip::draw_text(
        &mut expected,
        rect,
        "sopen 한글",
        [0, 0, 0, 255],
        &faces,
        clip::TextSize::Fill,
    );
    assert!(
        fx.win.doc.img == expected,
        "the window drew other pixels than draw_text"
    );
    let img = &fx.win.doc.img;
    let dark = img
        .pixels()
        .filter(|(x, y, p)| p != &before.get_pixel(*x, *y) && p.0[..3].iter().all(|c| *c < 60))
        .count();
    assert!(dark > 100, "text drawn: {dark} dark pixels");
    untouched_outside(img, &before, [40, 30, 160, 90]);
    if fonts::hangul().is_some() {
        // Every letter drawn, 한 and 글 included.
        assert_eq!(fx.win.status.text, "pasted text (8 chars)");
    } else {
        assert_eq!(
            fx.win.status.text,
            "pasted text (8 chars) · no Hangul font found, so Hangul may not render"
        );
    }
    fx.key(Key::Z, command(false));
    assert_eq!(fx.win.doc.img, before, "one undo step, exact pixels back");
}

/// Korean as Finder copies a file name — decomposed into conjoining jamo (NFD) — pastes the
/// same pixels and the same status as the composed text.
#[test]
fn command_v_with_decomposed_korean_pastes_like_composed_korean() {
    let nfd = "\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}";
    let paste = |text: &str| {
        let mut fx = open();
        fx.select((40.0, 30.0), (200.0, 120.0));
        fx.drv.paste_text(&mut fx.win, text);
        (fx.win.doc.img.clone(), fx.win.status.text.clone())
    };
    let (composed, said) = paste("한글");
    let (decomposed, says) = paste(nfd);
    assert_eq!(says, said);
    assert!(
        decomposed == composed,
        "NFD 한글 drew other pixels than NFC 한글"
    );
}

/// Text copied on Windows (`\r\n` line breaks) pastes the same pixels and the same status as
/// the same text with `\n`.
#[test]
fn command_v_with_windows_line_breaks_pastes_like_newlines() {
    let paste = |text: &str| {
        let mut fx = open();
        fx.select((40.0, 30.0), (200.0, 120.0));
        fx.drv.paste_text(&mut fx.win, text);
        (fx.win.doc.img.clone(), fx.win.status.text.clone())
    };
    let (lf, said) = paste("one two three\nfour\n\nfive");
    let (crlf, says) = paste("one two three\r\nfour\r\n\r\nfive");
    assert_eq!(says, said);
    assert_eq!(said, "pasted text (24 chars)");
    assert!(crlf == lf, "CRLF drew other pixels than LF");
}

/// Text without a rectangle: at the top-left in letters a tenth of the image's height, and
/// the block it takes becomes the selection.
#[test]
fn command_v_with_text_and_no_rectangle_writes_at_the_top_left_and_selects_it() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.drv.paste_text(&mut fx.win, "hello");
    let rect = fx.rect().expect("the text's block is selected");
    let [x, y, w, h] = rect;
    assert_eq!([x, y], [0, 0]);
    assert!((16..=24).contains(&h), "a line of 16 px letters: {rect:?}");
    assert!(w > 20 && w < W / 2, "{rect:?}");
    assert_ne!(fx.win.doc.img, before);
    untouched_outside(&fx.win.doc.img, &before, rect);
    assert_eq!(fx.win.status.text, "pasted text (5 chars)");
}

/// An image with alpha offers a transparent background: a cut then leaves the rectangle
/// transparent. An image without alpha does not offer it.
#[test]
fn a_transparent_background_is_offered_for_images_with_alpha() {
    let rgb = open();
    assert!(rgb.drv.find_text("transparent").is_none(), "not for RGB");

    let rgba = DynamicImage::ImageRgba8(ImageBuffer::from_fn(W, H, |x, y| {
        image::Rgba([x as u8, y as u8, 0, 255])
    }));
    let bytes = encode(&rgba, ImageFormat::Png, None, None).unwrap().bytes;
    let mut fx = open_bytes("alpha.png", bytes, 1000.0, 700.0);
    fx.settle();
    fx.drv.click_text(&mut fx.win, "transparent");
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.cut(&mut fx.win);
    let img = &fx.win.doc.img;
    assert_eq!(img.get_pixel(40, 30).0[3], 0, "transparent");
    assert_eq!(img.get_pixel(99, 79).0[3], 0, "transparent");
    untouched_outside(img, &rgba, [40, 30, 60, 50]);
    assert_eq!(img.color(), image::ColorType::Rgba8);
}

/// A read-only image (an animated GIF) can be copied, not cut or pasted into.
#[test]
fn a_read_only_image_copies_but_refuses_cut_and_paste() {
    let mut anim = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut anim);
        for shade in [10u8, 200] {
            let frame = image::RgbaImage::from_pixel(6, 6, image::Rgba([shade, 0, 0, 255]));
            enc.encode_frame(image::Frame::new(frame)).unwrap();
        }
    }
    let mut fx = open_bytes("a.gif", anim, 1000.0, 700.0);
    let before = fx.win.doc.img.clone();
    fx.drv.take_commands();
    fx.drv.copy(&mut fx.win);
    let copied = copied_images(&mut fx);
    assert_eq!(copied.len(), 1);
    assert_eq!(copied[0].size, [6, 6]);
    fx.drv.paste_image(&mut fx.win, halves(4, 4));
    assert!(
        fx.win.status.text.starts_with("read-only — animated GIF"),
        "{}",
        fx.win.status.text
    );
    fx.drv.paste_text(&mut fx.win, "x");
    assert!(
        fx.win.status.text.starts_with("read-only"),
        "{}",
        fx.win.status.text
    );
    // No rectangle can be drawn on a read-only image; Cmd/Ctrl+X is refused for being one.
    fx.drv.take_commands();
    fx.drv.cut(&mut fx.win);
    assert!(
        copied_images(&mut fx).is_empty(),
        "nothing cut to the clipboard"
    );
    assert!(
        fx.win.status.text.starts_with("read-only — animated GIF"),
        "{}",
        fx.win.status.text
    );
    assert_eq!(fx.win.doc.img, before);
    assert!(!fx.win.doc.can_undo());
}

/// While the unsaved-changes prompt is up, the clipboard shortcuts do nothing.
#[test]
fn clipboard_shortcuts_wait_while_the_close_prompt_is_up() {
    let mut fx = open();
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.key(Key::P, Modifiers::NONE);
    use crate::gui::Window;
    assert!(!fx.win.close_requested(), "unsaved: asks");
    let before = fx.win.doc.img.clone();
    fx.drv.take_commands();
    fx.drv.copy(&mut fx.win);
    fx.drv.cut(&mut fx.win);
    fx.drv.paste_image(&mut fx.win, halves(4, 4));
    assert!(copied_images(&mut fx).is_empty());
    assert_eq!(fx.win.doc.img, before);
}

/// While the block size is being typed, Cmd/Ctrl+C, X and V belong to that text field.
#[test]
fn clipboard_shortcuts_leave_a_text_field_being_edited_alone() {
    let mut fx = open();
    fx.select((40.0, 30.0), (100.0, 80.0));
    // A click on the slider's number turns it into a text field with the keyboard.
    fx.drv.click_text(&mut fx.win, "16");
    assert!(fx.drv.ctx.text_edit_focused(), "editing the block size");
    let before = fx.win.doc.img.clone();
    fx.drv.take_commands();
    fx.drv.copy(&mut fx.win);
    fx.drv.cut(&mut fx.win);
    fx.drv.paste_image(&mut fx.win, halves(4, 4));
    assert!(copied_images(&mut fx).is_empty());
    assert_eq!(fx.win.doc.img, before);
    assert!(fx.win.status.text.is_empty(), "{}", fx.win.status.text);
}

/// A Cmd/Ctrl+X held until the key repeats cuts once: the clipboard gets the picture, not the
/// blank the first cut left, and one undo brings it all back.
#[test]
fn a_held_command_x_cuts_once() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.take_commands();
    fx.drv
        .shortcut(&mut fx.win, Key::X, Some(egui::Event::Cut), 3);
    let copied = copied_images(&mut fx);
    assert_eq!(copied.len(), 1, "one image to the clipboard");
    assert_eq!(
        pixel(&copied[0], 0, 0),
        [40, 30, 0, 255],
        "the picture, not the blank"
    );
    assert_eq!(fx.win.doc.img.get_pixel(40, 30).0, WHITE);
    assert!(fx.win.doc.undo());
    assert_eq!(fx.win.doc.img, before);
    assert!(!fx.win.doc.can_undo(), "one undo step");
}

/// A held Cmd/Ctrl+V pastes once — text into the rectangle, and a picture with no rectangle
/// (whose pasted area becomes the selection the repeats would otherwise paste into).
#[test]
fn a_held_command_v_pastes_once() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.select((40.0, 30.0), (200.0, 120.0));
    let text = egui::Event::Paste("sopen".to_string());
    fx.drv.shortcut(&mut fx.win, Key::V, Some(text), 3);
    assert!(fx.win.doc.undo());
    assert!(!fx.win.doc.can_undo(), "text: one undo step");
    assert_eq!(fx.win.doc.img, before);

    let mut fx = open();
    let before = fx.win.doc.img.clone();
    let picture = egui::Event::PasteImage(std::sync::Arc::new(halves(30, 20)));
    fx.drv.shortcut(&mut fx.win, Key::V, Some(picture), 3);
    assert_eq!(fx.rect(), Some([0, 0, 30, 20]));
    assert!(fx.win.doc.undo());
    assert!(!fx.win.doc.can_undo(), "picture: one undo step");
    assert_eq!(fx.win.doc.img, before);
}

/// Pasting the same thing again after changing a choice is a new paste, not a repeat.
#[test]
fn the_same_paste_after_another_fit_is_pasted_again() {
    let mut fx = open();
    fx.settle();
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.paste_image(&mut fx.win, halves(30, 10));
    assert_eq!(fx.win.doc.img.get_pixel(41, 31).0, BLUE, "stretched");
    fx.drv.click_text(&mut fx.win, "Fit");
    fx.drv.paste_image(&mut fx.win, halves(30, 10));
    assert_eq!(
        fx.win.doc.img.get_pixel(41, 31).0,
        WHITE,
        "fitted: above it"
    );
    assert_eq!(fx.win.status.text, "pasted image 30×10 → 60×20 (fit)");
    assert!(fx.win.doc.undo() && fx.win.doc.undo() && !fx.win.doc.can_undo());
}

/// The paste choices and colours sit on their own row, which fits on one line from the
/// smallest window width up (with the transparent choice an image with alpha adds).
#[test]
fn the_paste_row_is_one_line_at_every_window_width() {
    let rgba = DynamicImage::ImageRgba8(ImageBuffer::from_fn(W, H, |x, y| {
        image::Rgba([x as u8, y as u8, 0, 255])
    }));
    let bytes = encode(&rgba, ImageFormat::Png, None, None).unwrap().bytes;
    for w in [480.0, 640.0, 1400.0] {
        let mut fx = open_bytes("alpha.png", bytes.clone(), w, 400.0);
        fx.settle();
        let y = |label: &str| {
            fx.drv
                .find_text(label)
                .unwrap_or_else(|| panic!("no {label:?} at {w} px"))
                .center()
                .y
        };
        let row = y("Stretch");
        assert!((y("transparent") - row).abs() < 0.5, "{w} px");
        assert!((y("Text") - row).abs() < 0.5, "{w} px");
        // Both swatches whole on screen, on the row: white (background), black (text).
        for colour in [Color32::WHITE, Color32::BLACK] {
            let swatch = fx
                .drv
                .rects()
                .into_iter()
                .any(|(r, fill, _)| fill == colour && (r.center().y - row).abs() < 3.0);
            assert!(swatch, "{colour:?} swatch shown whole at {w} px");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A held shortcut against a new press: egui-winit sends a repeat the same way as a press.

/// Cmd/Ctrl held throughout, X pressed, let go, pressed again: two cuts.
#[test]
fn command_x_pressed_again_after_letting_go_cuts_again() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.take_commands();
    for _ in 0..2 {
        fx.drv
            .frame(&mut fx.win, vec![egui::Event::Cut], command(false));
        fx.drv
            .frame(&mut fx.win, vec![key_up(Key::X)], command(false));
    }
    assert_eq!(copied_images(&mut fx).len(), 2, "cut twice");
    assert!(fx.win.doc.undo() && fx.win.doc.undo(), "two undo steps");
    assert!(!fx.win.doc.can_undo());
    assert_eq!(fx.win.doc.img, before);
}

/// A second Cmd/Ctrl+V after the key was let go pastes again — the same clipboard, rectangle
/// and choices: text drawn twice, a half-transparent picture laid twice.
#[test]
fn command_v_pressed_again_after_letting_go_pastes_again() {
    let translucent = {
        let img = image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 128]));
        egui::ColorImage::from_rgba_unmultiplied([10, 10], img.as_raw())
    };
    let events = [
        egui::Event::Paste("sopen".to_string()),
        egui::Event::PasteImage(std::sync::Arc::new(translucent)),
    ];
    for event in events {
        let mut fx = open();
        let before = fx.win.doc.img.clone();
        fx.select((40.0, 30.0), (100.0, 80.0));
        fx.drv
            .frame(&mut fx.win, vec![event.clone()], command(false));
        fx.drv
            .frame(&mut fx.win, vec![key_up(Key::V)], command(false));
        let once = fx.win.doc.img.clone();
        fx.drv
            .frame(&mut fx.win, vec![event.clone()], command(false));
        fx.drv
            .frame(&mut fx.win, vec![key_up(Key::V)], command(false));
        assert_ne!(fx.win.doc.img, once, "laid again over the first");
        assert!(fx.win.doc.undo() && fx.win.doc.undo(), "two undo steps");
        assert!(!fx.win.doc.can_undo());
        assert_eq!(fx.win.doc.img, before);
    }
}

/// A paste, the rectangle moved away and back, a paste again: two pastes.
#[test]
fn a_paste_after_moving_the_rectangle_away_and_back_pastes_again() {
    let mut fx = open();
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.drv.paste_text(&mut fx.win, "sopen");
    let (inside, away) = (fx.at(70.0, 55.0), fx.at(90.0, 65.0));
    fx.drv
        .drag(&mut fx.win, PointerButton::Primary, inside, away);
    assert_eq!(fx.rect(), Some([60, 40, 60, 50]), "moved away");
    fx.drv
        .drag(&mut fx.win, PointerButton::Primary, away, inside);
    assert_eq!(fx.rect(), Some([40, 30, 60, 50]), "and back");
    fx.drv.paste_text(&mut fx.win, "sopen");
    assert!(fx.win.doc.undo() && fx.win.doc.undo(), "two undo steps");
    assert!(!fx.win.doc.can_undo());
}

/// The window losing focus, or Cmd/Ctrl going up, ends a held Cmd/Ctrl+V (its release may
/// never reach the window): the next press pastes.
#[test]
fn losing_focus_or_letting_go_of_command_ends_a_held_shortcut() {
    let paste = egui::Event::Paste("sopen".to_string());
    let gaps: [Vec<(Vec<egui::Event>, Modifiers)>; 2] = [
        vec![
            (vec![egui::Event::WindowFocused(false)], command(false)),
            (vec![egui::Event::WindowFocused(true)], command(false)),
        ],
        vec![(Vec::new(), Modifiers::NONE)],
    ];
    for (case, gap) in ["focus lost", "command let go"].into_iter().zip(gaps) {
        let mut fx = open();
        fx.select((40.0, 30.0), (100.0, 80.0));
        fx.drv
            .frame(&mut fx.win, vec![paste.clone()], command(false));
        for (events, mods) in gap {
            fx.drv.frame(&mut fx.win, events, mods);
        }
        fx.drv
            .frame(&mut fx.win, vec![paste.clone()], command(false));
        assert!(
            fx.win.doc.undo() && fx.win.doc.undo(),
            "{case}: two undo steps"
        );
        assert!(!fx.win.doc.can_undo(), "{case}");
    }
}

/// Drawing a rectangle is only that: no pixel, no undo step, nothing to the clipboard.
#[test]
fn drawing_a_rectangle_changes_and_copies_nothing() {
    let mut fx = open();
    let before = fx.win.doc.img.clone();
    fx.drv.take_commands();
    fx.select((40.0, 30.0), (100.0, 80.0));
    assert_eq!(fx.rect(), Some([40, 30, 60, 50]));
    assert_eq!(fx.win.doc.img, before);
    assert!(!fx.win.doc.can_undo());
    assert!(fx.drv.take_commands().is_empty(), "no clipboard command");
}

/// The background swatch, changed through its own picker (its "G 255" value dragged down to
/// 0), is what Cmd/Ctrl+X leaves.
#[test]
fn a_background_picked_on_the_swatch_is_what_a_cut_leaves() {
    let mut fx = open();
    fx.settle();
    fx.select((40.0, 30.0), (100.0, 80.0));
    let row = fx
        .drv
        .find_text("Background")
        .expect("the label")
        .center()
        .y;
    let swatch = fx
        .drv
        .rects()
        .into_iter()
        .find(|(r, fill, _)| *fill == Color32::WHITE && (r.center().y - row).abs() < 3.0)
        .expect("the white background swatch")
        .0;
    fx.drv.click(&mut fx.win, swatch.center());
    let g = fx
        .drv
        .find_text("G ")
        .expect("the picker's green value")
        .center();
    fx.drv.drag(
        &mut fx.win,
        PointerButton::Primary,
        g,
        g - Vec2::new(600.0, 0.0),
    );
    // The picker stays open (Esc would clear the rectangle too); the cut goes to the image.
    fx.drv.cut(&mut fx.win);
    assert_eq!(fx.win.status.text, "cut 60×50");
    assert_eq!(fx.win.doc.img.get_pixel(40, 30).0, [255, 0, 255, 255]);
    assert_eq!(fx.win.doc.img.get_pixel(99, 79).0, [255, 0, 255, 255]);
}

/// A Cmd/Ctrl+V let go while the shortcuts are off (here: the close prompt is up) still ends
/// the held paste: after Cancel — Cmd/Ctrl held all along — the next press pastes.
#[test]
fn a_key_let_go_while_the_close_prompt_is_up_still_ends_a_held_paste() {
    let mut fx = open();
    fx.select((40.0, 30.0), (100.0, 80.0));
    let cmd = command(false);
    let paste = egui::Event::Paste("sopen".to_string());
    fx.drv.frame(&mut fx.win, vec![paste.clone()], cmd);
    use crate::gui::Window;
    assert!(!fx.win.close_requested(), "unsaved: asks");
    fx.drv.frame(&mut fx.win, vec![key_up(Key::V)], cmd);
    for _ in 0..3 {
        fx.drv.frame(&mut fx.win, Vec::new(), cmd); // the prompt lays itself out
    }
    let cancel = fx.drv.find_text("Cancel").expect("the prompt").center();
    for event in [
        egui::Event::PointerMoved(cancel),
        egui::Event::PointerButton {
            pos: cancel,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: cmd,
        },
        egui::Event::PointerButton {
            pos: cancel,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: cmd,
        },
    ] {
        fx.drv.frame(&mut fx.win, vec![event], cmd);
    }
    assert!(!fx.win.prompt, "cancelled");
    fx.drv.frame(&mut fx.win, vec![paste], cmd);
    assert!(fx.win.doc.undo() && fx.win.doc.undo(), "two undo steps");
    assert!(!fx.win.doc.can_undo());
}

/// On its own the window has PC Shell's F-key row, with F3 Exit, and F3 goes through the real
/// close: with an edit the shell's close is refused and the unsaved-changes prompt comes up;
/// Discard closes. Without an edit it closes at once.
#[test]
fn f3_closes_the_window_through_its_prompt() {
    let mut fx = open();
    fx.drv.as_shell();
    fx.settle();
    assert!(fx.drv.find_text("Exit   ").is_some(), "the F-key row");
    fx.select((40.0, 30.0), (100.0, 80.0));
    fx.key(Key::P, Modifiers::NONE);
    assert!(fx.win.doc.unsaved(), "an edit");
    fx.key(Key::F3, Modifiers::NONE);
    assert_eq!(fx.drv.refused_closes, 1, "the close is refused");
    assert!(!fx.drv.window_closed);
    assert!(fx.drv.find_text("Unsaved changes").is_some(), "the prompt");
    fx.drv.click_text(&mut fx.win, "Discard");
    fx.drv.idle(&mut fx.win, 1);
    assert!(fx.drv.window_closed, "Discard closes it");
    let mut clean = open();
    clean.drv.as_shell();
    clean.settle();
    clean.key(Key::F3, Modifiers::NONE);
    assert_eq!(
        (clean.drv.refused_closes, clean.drv.window_closed),
        (0, true)
    );
}

/// F5 is PC Shell's Save, as Cmd/Ctrl+S: it saves an edit — and nothing while a text field
/// (the block size) has the typing, while the unsaved-changes prompt is up, or while the
/// picture is read-only. The F-key row lists it, and so does the browser's when it hosts the
/// window.
#[test]
fn f5_saves_as_command_s_does_and_only_where_it_would() {
    let mut fx = open();
    fx.settle();
    assert!(fx.drv.find_text("Save   ").is_some(), "5Save in the row");
    assert_eq!(
        crate::gui::Window::keys(&fx.win),
        crate::gui::chrome::IMAGE_KEYS
    );
    let original = std::fs::read(&fx.path).unwrap();
    fx.select((0.0, 0.0), (120.0, 80.0));
    fx.key(Key::C, Modifiers::NONE);
    // The block size field has the typing: F5 is not for it either.
    fx.drv.click_text(&mut fx.win, "16");
    assert!(fx.drv.ctx.text_edit_focused());
    fx.key(Key::F5, Modifiers::NONE);
    assert!(!fx.win.saving_or_queued(), "F5 in a field saves nothing");
    assert_eq!(std::fs::read(&fx.path).unwrap(), original);
    fx.drv.ctx.memory_mut(|m| m.stop_text_input());
    fx.drv
        .ctx
        .memory_mut(|m| m.focused().map(|id| m.surrender_focus(id)));
    fx.drv.idle(&mut fx.win, 2);
    assert!(!fx.drv.ctx.egui_wants_keyboard_input());
    // Under the prompt.
    assert!(!crate::gui::Window::close_requested(&mut fx.win));
    fx.drv.idle(&mut fx.win, 2);
    fx.key(Key::F5, Modifiers::NONE);
    assert!(
        !fx.win.saving_or_queued(),
        "F5 under the prompt saves nothing"
    );
    fx.drv.click_text(&mut fx.win, "Cancel");
    // F5 saves.
    fx.key(Key::F5, Modifiers::NONE);
    wait_for_save(&mut fx);
    assert!(
        fx.win.status.text.starts_with("saved "),
        "{}",
        fx.win.status.text
    );
    let saved = image::load_from_memory(&std::fs::read(&fx.path).unwrap()).unwrap();
    assert_eq!((saved.width(), saved.height()), (120, 80));
    // Read-only: nothing.
    fx.select((10.0, 10.0), (60.0, 40.0));
    fx.key(Key::C, Modifiers::NONE);
    let written = std::fs::read(&fx.path).unwrap();
    fx.win.doc.read_only = Some("a test".into());
    fx.key(Key::F5, Modifiers::NONE);
    assert!(!fx.win.saving_or_queued(), "F5 read-only saves nothing");
    assert_eq!(std::fs::read(&fx.path).unwrap(), written);
}

/// 04 §3: F5 saves with no modifier only; Shift+F5 and Alt+F5 start nothing.
#[test]
fn f5_saves_only_without_modifiers() {
    let mut fx = open();
    fx.select((10.0, 10.0), (60.0, 40.0));
    fx.key(Key::C, Modifiers::NONE);
    assert!(fx.win.doc.dirty());
    for mods in [Modifiers::SHIFT, Modifiers::ALT] {
        fx.key(Key::F5, mods);
        assert!(!fx.win.saving_or_queued(), "{mods:?}+F5 started a save");
    }
    fx.key(Key::F5, Modifiers::NONE);
    assert!(
        fx.win.saving_or_queued() || fx.win.status.text.starts_with("saved"),
        "F5 alone saves: {}",
        fx.win.status.text
    );
    fx.drv.until(&mut fx.win, 20, |w| !w.saving_or_queued());
}
