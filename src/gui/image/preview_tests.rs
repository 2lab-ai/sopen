//! The image window as a preview (02 §4.2–§4.4): told it is one (`set_preview(true)`), it shows
//! the picture fitted in a single-lined frame, without its toolbars, status row or disclosures,
//! and no drag, wheel, key, clipboard shortcut or click changes it — on its own, with nothing
//! around it disabling it or withholding events; handed over (`set_preview(false)`), it is the
//! image window again, the picture it decoded kept.

use super::*;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window as _;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Key, Modifiers, PointerButton};

const W: u32 = 240;
const H: u32 = 160;

/// A PNG with alpha (so the transparent choice would be offered) of `W`×`H`.
fn png() -> Vec<u8> {
    let img = DynamicImage::ImageRgba8(ImageBuffer::from_fn(W, H, |x, y| {
        image::Rgba([x as u8, y as u8, 0, 255])
    }));
    encode(&img, ImageFormat::Png, None, None).unwrap().bytes
}

struct Fx {
    _tmp: tempfile::TempDir,
    win: ImageWindow,
    drv: Driver,
}

/// The window on a local PNG, told it is a preview before its first pass.
fn previewed() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("shot.png");
    let bytes = png();
    std::fs::write(&path, &bytes).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = ImageFile::new(origin, bytes, &tmp.path().join("cache"));
    let doc = Doc::decode(file.original()).unwrap();
    let mut win = ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, Vec::new());
    win.set_preview(true);
    let mut fx = Fx {
        _tmp: tmp,
        win,
        drv: Driver::new(600.0, 500.0),
    };
    fx.drv.until(&mut fx.win, 5, |w| w.ready());
    fx
}

/// What input could change in the image window.
fn state(win: &ImageWindow) -> String {
    format!(
        "rev={} dirty={} rect={:?} scale={:?} centre={:?} job={} prompt={} status={:?} \
         fit={:?} transparent={}",
        win.doc.rev(),
        win.doc.dirty(),
        win.rect(),
        win.view.scale,
        win.view.center,
        win.job.is_some(),
        win.prompt,
        win.status.text,
        win.fit,
        win.transparent,
    )
}

/// P5 (image): drags with each button, the wheel, every key the window knows, Cmd/Ctrl+C, X and
/// V (text and a picture), clicks where the toolbars would be — nothing changes, nothing goes to
/// the clipboard, nothing is saved, and dropping it asks nothing. (E3 measured Cmd/Ctrl+V editing
/// the image through `ui.disable()` and a drag selecting through withheld events.)
#[test]
fn a_previewed_image_takes_no_input() {
    let mut fx = previewed();
    let before = state(&fx.win);
    let (a, b) = (Pos2::new(200.0, 150.0), Pos2::new(400.0, 350.0));
    for button in [
        PointerButton::Primary,
        PointerButton::Secondary,
        PointerButton::Middle,
    ] {
        fx.drv.drag(&mut fx.win, button, a, b);
    }
    fx.drv.wheel(&mut fx.win, Pos2::new(300.0, 250.0), 3.0);
    for (key, mods) in [
        (Key::C, Modifiers::NONE),
        (Key::P, Modifiers::NONE),
        (Key::F, Modifiers::NONE),
        (Key::Num1, Modifiers::NONE),
        (Key::F5, Modifiers::NONE),
        (Key::Escape, Modifiers::NONE),
        (Key::Z, command(false)),
        (Key::Z, command(true)),
        (Key::S, command(false)),
        (Key::S, command(true)),
    ] {
        fx.drv.key(&mut fx.win, key, mods);
    }
    fx.drv.take_commands();
    fx.drv.copy(&mut fx.win);
    fx.drv.cut(&mut fx.win);
    fx.drv.paste_text(&mut fx.win, "text");
    let picture = egui::ColorImage::new([8, 8], vec![Color32::RED; 64]);
    fx.drv.paste_image(&mut fx.win, picture);
    for at in [
        Pos2::new(30.0, 12.0),
        Pos2::new(120.0, 12.0),
        Pos2::new(60.0, 40.0),
    ] {
        fx.drv.click(&mut fx.win, at);
    }
    fx.drv.idle(&mut fx.win, 2);
    assert_eq!(state(&fx.win), before, "the preview changed");
    assert!(
        fx.drv.take_commands().is_empty(),
        "nothing went to the clipboard"
    );
    assert!(fx.win.close_requested(), "dropping it asks nothing");
}

/// P5 (image): the picture fitted in the side, in a single-lined frame, with no toolbar, paste
/// row, status row or disclosures.
#[test]
fn a_previewed_image_has_no_bars_and_an_inactive_frame() {
    let fx = previewed();
    assert!(!fx.drv.pictures().is_empty(), "the picture is shown");
    for text in ["Crop  C", "Save copy", "Paste", "Undo"] {
        assert!(fx.drv.find_text(text).is_none(), "{text} in a preview");
    }
    assert_eq!(
        fx.drv.text_starting_with(&format!("{W}×{H}")),
        None,
        "no status row"
    );
    assert_eq!(
        fx.drv.colors_of(TITLE),
        Some(vec![super::super::chrome::PALETTE.frame_idle.color])
    );
    // Fitted: the picture fills the frame's room on one axis.
    let shown = fx.drv.pictures()[0];
    let room = fx.win.map.expect("drawn").disp;
    assert_eq!(shown, room);
    assert!(shown.width() > W as f32, "fitted up to the side, not 1:1");
}

/// The hand-over: `set_preview(false)` brings the toolbar, the status row and the active frame
/// back, and a drag selects again — the picture is the one decoded for the preview.
#[test]
fn a_handed_over_image_takes_input_and_shows_its_bars() {
    let mut fx = previewed();
    let rev = fx.win.doc.rev();
    fx.win.set_preview(false);
    fx.drv.until(&mut fx.win, 5, |w| w.ready());
    fx.drv.idle(&mut fx.win, 3);
    assert!(fx.drv.find_text("Crop  C").is_some(), "the toolbar");
    assert!(
        fx.drv.text_starting_with(&format!("{W}×{H}")).is_some(),
        "the status row"
    );
    assert_eq!(
        fx.drv.colors_of(TITLE),
        Some(vec![super::super::chrome::PALETTE.frame_active.color])
    );
    let map = fx.win.map.expect("drawn");
    let (a, b) = (
        map.to_disp(Pos2::new(20.0, 20.0)),
        map.to_disp(Pos2::new(120.0, 100.0)),
    );
    fx.drv.drag(&mut fx.win, PointerButton::Primary, a, b);
    assert!(fx.win.rect().is_some(), "a drag selects again");
    assert_eq!(fx.win.doc.rev(), rev, "the same picture, not decoded again");
}

/// An image wider than a preview shows (3000×300): previewed, its texture is the fitted picture
/// made for it (no side over [`PREVIEW_SIDE`]); handed over, the texture is made again from the
/// image itself, and crop, paste and save work on its full-resolution pixels.
#[test]
fn a_large_image_is_previewed_fitted_and_handed_over_whole() {
    let (w, h) = (3000u32, 300u32);
    let img = DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
        image::Rgb([(x / 12) as u8, y as u8, 40])
    }));
    let bytes = encode(&img, ImageFormat::Png, None, None).unwrap().bytes;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("wide.png");
    std::fs::write(&path, &bytes).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = ImageFile::new(origin, bytes, &tmp.path().join("cache"));
    let doc = Doc::decode(file.original()).unwrap();
    let shown = fitted(&doc);
    assert_eq!(
        shown.as_ref().map(|p| p.dimensions()),
        Some((PREVIEW_SIDE, 205))
    );
    let mut win = ImageWindow::previewing(file, doc, shown);
    let mut drv = Driver::new(700.0, 400.0);
    drv.until(&mut win, 5, |w| w.ready());
    let side = |win: &ImageWindow| win.tex.as_ref().map(|t| t.size());
    assert_eq!(side(&win), Some([PREVIEW_SIDE as usize, 205]), "fitted");
    win.set_preview(false);
    drv.until(&mut win, 5, |w| w.ready());
    drv.idle(&mut win, 3);
    assert_eq!(
        side(&win),
        Some([w as usize, h as usize]),
        "the image itself"
    );
    // Crop on full-resolution pixels: wider than the preview's picture was.
    let map = win.map.expect("drawn");
    let (a, b) = (
        map.to_disp(Pos2::new(100.0, 50.0)),
        map.to_disp(Pos2::new(2900.0, 250.0)),
    );
    drv.drag(&mut win, PointerButton::Primary, a, b);
    let rect = win.rect().expect("selected");
    drv.key(&mut win, Key::C, Modifiers::NONE);
    let (cw, ch) = win.doc.size();
    assert_eq!((cw, ch), (rect[2], rect[3]), "cropped to the rectangle");
    assert!(cw > PREVIEW_SIDE, "full resolution: {cw}×{ch}");
    // Paste a picture into the whole image, then save.
    drv.idle(&mut win, 3);
    let map = win.map.expect("drawn");
    let (a, b) = (
        map.to_disp(Pos2::new(0.0, 0.0)),
        map.to_disp(Pos2::new(cw as f32, ch as f32)),
    );
    drv.drag(&mut win, PointerButton::Primary, a, b);
    let rev = win.doc.rev();
    let red = egui::ColorImage::new([8, 8], vec![Color32::RED; 64]);
    drv.paste_image(&mut win, red);
    assert!(win.doc.rev() > rev, "pasted: {}", win.status.text);
    drv.key(&mut win, Key::S, command(false));
    drv.until(&mut win, 20, |w| {
        w.job.is_none() && w.status.text.starts_with("saved")
    });
    let saved = image::open(&path).unwrap();
    assert_eq!((saved.width(), saved.height()), (cw, ch), "saved whole");
}
