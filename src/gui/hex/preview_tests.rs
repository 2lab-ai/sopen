//! The hex window as a preview (02 §4.2–§4.4): told it is one (`set_preview(true)`), it shows
//! its first rows in a single-lined frame, without its toolbar, status row, cursor or
//! disclosures; no key, typing, click, drag or wheel changes it, and it never takes the keyboard
//! (E3: the grid asked for focus on every pass, disabled or not, hex.rs grid); handed over
//! (`set_preview(false)`), it is the hex window again.

use super::*;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::{command, Driver};
use crate::gui::Window as _;
use crate::remote::{LocalFs, RemoteFs};
use eframe::egui::{Event, Modifiers, PointerButton, Pos2};

struct Fx {
    _tmp: tempfile::TempDir,
    win: HexWindow,
    drv: Driver,
}

/// A local file of 64 KiB opened in place, told it is a preview before its first pass.
fn previewed() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("data.bin");
    let bytes: Vec<u8> = (0..=255u8).cycle().take(64 << 10).collect();
    std::fs::write(&path, bytes).unwrap();
    let abs = path.to_str().unwrap().to_string();
    let mut fs = LocalFs::new();
    let at = fs.stat(&abs).unwrap();
    let origin = Origin::new(Box::new(fs), None, None, abs, at);
    let file = HexFile::open(origin, &path, None, &tmp.path().join("cache")).unwrap();
    let doc = HexDoc::new(file.base(), file.size());
    let mut win = HexWindow::new(
        file,
        doc,
        Cursor::default(),
        Vec::new(),
        Arc::default(),
        Arc::default(),
    );
    win.set_preview(true);
    let mut fx = Fx {
        _tmp: tmp,
        win,
        drv: Driver::new(700.0, 500.0),
    };
    fx.drv.until(&mut fx.win, 5, |w| w.ready());
    fx
}

/// What input could change in the hex window, and who has the keyboard.
fn state(fx: &Fx) -> String {
    let w = &fx.win;
    format!(
        "rev={} patches={} cursor={:?} top={} goto={:?} job={} prompt={} status={:?} focus={:?}",
        w.doc.rev(),
        w.doc.patches().len(),
        w.cursor,
        w.top_row,
        w.goto,
        w.job.is_some(),
        w.prompt,
        w.status.text,
        fx.drv.ctx.memory(|m| m.focused()),
    )
}

/// P5 (hex): typing, the keys the window knows, Cmd/Ctrl+G and F6, clicks on a byte, a drag, the
/// wheel and the scroll bar change nothing, and the grid never takes the keyboard; dropping it
/// asks nothing.
#[test]
fn a_previewed_hex_viewer_takes_no_input_and_never_takes_the_keyboard() {
    let mut fx = previewed();
    assert_eq!(fx.drv.ctx.memory(|m| m.focused()), None, "no focus asked");
    let before = state(&fx);
    fx.drv.frame(
        &mut fx.win,
        vec![Event::Text("41ab".into())],
        Modifiers::NONE,
    );
    for (key, mods) in [
        (Key::ArrowDown, Modifiers::NONE),
        (Key::ArrowRight, Modifiers::NONE),
        (Key::PageDown, Modifiers::NONE),
        (Key::End, Modifiers::NONE),
        (Key::Tab, Modifiers::NONE),
        (Key::F8, Modifiers::NONE),
        (Key::F5, Modifiers::NONE),
        (Key::F6, Modifiers::NONE),
        (Key::End, command(false)),
        (Key::G, command(false)),
        (Key::Z, command(false)),
        (Key::S, command(false)),
    ] {
        fx.drv.key(&mut fx.win, key, mods);
    }
    fx.drv.paste_text(&mut fx.win, "ff");
    for at in [Pos2::new(200.0, 120.0), Pos2::new(560.0, 140.0)] {
        fx.drv.click(&mut fx.win, at);
    }
    fx.drv.drag(
        &mut fx.win,
        PointerButton::Primary,
        Pos2::new(150.0, 100.0),
        Pos2::new(400.0, 300.0),
    );
    fx.drv.wheel(&mut fx.win, Pos2::new(300.0, 250.0), -5.0);
    // Where the scroll bar is, on the frame's right border.
    fx.drv.drag(
        &mut fx.win,
        PointerButton::Primary,
        Pos2::new(687.0, 60.0),
        Pos2::new(687.0, 400.0),
    );
    fx.drv.idle(&mut fx.win, 2);
    assert_eq!(state(&fx), before, "the preview changed");
    assert!(fx.win.close_requested(), "dropping it asks nothing");
}

/// P5 (hex): the first rows in a single-lined frame; no Go to field, no Save, no status row, no
/// cursor.
#[test]
fn a_previewed_hex_viewer_has_no_bars_cursor_or_active_frame() {
    let fx = previewed();
    assert!(
        fx.drv.text_starting_with("00 01 02 03").is_some(),
        "the first row's bytes"
    );
    for text in ["Go to", "[ Save ]", "[ Save copy ]", "[ Undo ]", "Save"] {
        assert!(fx.drv.find_text(text).is_none(), "{text} in a preview");
    }
    assert_eq!(fx.drv.text_starting_with("0x0 (0)"), None, "no status row");
    assert_eq!(
        fx.drv.colors_of(TITLE),
        Some(vec![PALETTE.frame_idle.color])
    );
    let cursor = fx
        .drv
        .rects()
        .into_iter()
        .any(|(_, fill, _)| fill == PALETTE.bar);
    assert!(!cursor, "no cursor cell in a preview");
}

/// The hand-over: `set_preview(false)` brings the toolbar, the status row and the cursor back,
/// the grid takes the keyboard, and typing overwrites again.
#[test]
fn a_handed_over_hex_viewer_takes_input_and_shows_its_bars() {
    let mut fx = previewed();
    fx.win.set_preview(false);
    fx.drv.idle(&mut fx.win, 3);
    assert!(fx.drv.find_text("Go to").is_some(), "the toolbar");
    assert!(
        fx.drv.text_starting_with("0x0 (0)").is_some(),
        "the status row"
    );
    assert_eq!(
        fx.drv.colors_of(TITLE),
        Some(vec![PALETTE.frame_active.color])
    );
    assert_eq!(fx.drv.ctx.memory(|m| m.focused()), Some(grid_id()));
    fx.drv
        .frame(&mut fx.win, vec![Event::Text("41".into())], Modifiers::NONE);
    fx.drv.idle(&mut fx.win, 1);
    assert_eq!(fx.win.doc.byte(0), Some(0x41), "typing overwrites again");
}

/// A list beside the preview, as the browser has it: it holds the keyboard and reads its own
/// keys (↓ counted), then the previewed hex window is drawn — on its own, not disabled.
struct ListAndPreview {
    win: HexWindow,
    downs: usize,
}

fn list_id() -> egui::Id {
    egui::Id::unique("the-list-beside")
}

impl crate::gui::Window for ListAndPreview {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let list = ui.interact(
            egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(10.0, 10.0)),
            list_id(),
            egui::Sense::click(),
        );
        if !list.has_focus() {
            list.request_focus();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown)) {
            self.downs += 1;
        }
        self.win.ui(ui);
    }
    fn ready(&self) -> bool {
        self.win.ready()
    }
}

/// E3's leak: with the list holding the keyboard, a previewed hex window never takes it — after
/// every pass the list has it — and the list's keys still reach it.
#[test]
fn a_previewed_hex_viewer_leaves_the_lists_keyboard_alone() {
    let fx = previewed();
    let mut host = ListAndPreview {
        win: fx.win,
        downs: 0,
    };
    let mut drv = Driver::new(700.0, 500.0);
    for _ in 0..5 {
        drv.idle(&mut host, 1);
        assert_eq!(
            drv.ctx.memory(|m| m.focused()),
            Some(list_id()),
            "the list has it"
        );
    }
    let before = host.win.cursor;
    for _ in 0..3 {
        drv.key(&mut host, Key::ArrowDown, Modifiers::NONE);
        assert_eq!(drv.ctx.memory(|m| m.focused()), Some(list_id()));
    }
    assert_eq!(host.downs, 3, "the list got its keys");
    assert_eq!(host.win.cursor, before, "the preview did not");
}

/// 02 §4.2: a preview has no bars — no path row of its own (the side's frame names the file) —
/// and its rows fit the side: sixteen bytes a row where they fit, else eight, the ASCII column
/// whole either way. The full window keeps its path row and sixteen bytes at any width.
#[test]
fn a_previewed_hex_viewer_fits_its_rows_and_draws_no_path_row() {
    let mut fx = previewed();
    let path = fx.win.title.clone();
    let whole = |drv: &Driver, text: &str| drv.find_text(text).is_some();
    // 700 points: sixteen bytes fit.
    assert!(whole(
        &fx.drv,
        "00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F"
    ));
    assert!(whole(&fx.drv, "00016(0010)"), "the second row starts at 16");
    assert!(fx.drv.text_starting_with(&path).is_none(), "no path row");
    // Half of the browser's default window: eight bytes a row, the ASCII column whole.
    fx.drv.resize(496.0, 500.0);
    fx.drv.idle(&mut fx.win, 2);
    assert!(
        whole(&fx.drv, "00 01 02 03 04 05 06 07"),
        "eight bytes a row"
    );
    assert!(whole(&fx.drv, "00008(0008)"), "the second row starts at 8");
    assert!(whole(&fx.drv, "08 09 0A 0B 0C 0D 0E 0F"));
    let ascii: String = (0x30..0x38u8).map(chrome::cp437).collect();
    assert!(whole(&fx.drv, &ascii), "the ASCII column is whole: {ascii}");
    assert!(fx.drv.text_starting_with(&path).is_none(), "no path row");
    // Handed over, it is the hex window: its path row, sixteen bytes, cut where it must be.
    fx.win.set_preview(false);
    fx.drv.idle(&mut fx.win, 2);
    assert!(fx.drv.text_starting_with("00016(0010)").is_some());
    assert!(!whole(&fx.drv, "00008(0008)"));
    fx.drv.resize(900.0, 500.0);
    fx.drv.idle(&mut fx.win, 2);
    // The path row is cells (`texts`: its run; `galleys` holds one-character glyphs).
    let path_row =
        |t: &str| t == path || t.strip_prefix('…').is_some_and(|end| path.ends_with(end));
    assert!(fx.drv.texts().iter().any(|(_, _, t)| path_row(t)), "{path}");
}
