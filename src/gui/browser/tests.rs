//! The file browser driven by real egui key and pointer events through the test kit: the list
//! and its keys, a viewer in the same window and every way out of it through the viewer's own
//! `close_requested`, F4 through the real editor launch, a fetch stopped by Esc.

use super::*;
use crate::gui::testkit::{Driver, Meter};
use crate::remote::{LocalFs, Stat};
use eframe::egui::{Event, PointerButton, Vec2};
use std::io::Write;
use std::path::Path;

mod page;
mod s1;
mod s2;
mod s3;
mod s4;
mod s5;
mod s6;
mod vm7;
mod vm7b;
mod vm8;
mod x1a;
mod x1b;

const RUST: &str = "// a comment\nfn main() {\n    let answer = 42;\n}\n";

/// The folder every test lists: two folders, a hidden file, a binary, code, text, an image.
fn tree(root: &Path) {
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::write(root.join("alpha/inner.txt"), "inside\n").unwrap();
    std::fs::create_dir(root.join("beta")).unwrap();
    std::fs::write(root.join(".hidden"), "secret\n").unwrap();
    let bytes: Vec<u8> = (0..=255u8).cycle().take(512).collect();
    std::fs::write(root.join("data.bin"), bytes).unwrap();
    std::fs::write(root.join("main.rs"), RUST).unwrap();
    std::fs::write(root.join("notes.txt"), "plain notes\n").unwrap();
    std::fs::write(root.join("zeta.md"), "# zeta\n").unwrap();
    image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 90]))
        .save(root.join("photo.png"))
        .unwrap();
}

/// What F4 was asked to open, in order.
type Launched = Arc<Mutex<Vec<(String, bool)>>>;

struct Fx {
    _tmp: tempfile::TempDir,
    root: String,
    cache: PathBuf,
    drv: Driver,
    b: Browser,
    launched: Launched,
}

fn local_connect() -> Connect {
    Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>))
}

/// The browser on the tree, on this machine (`label` `None`) or posing as host `box` (files are
/// fetched into the cache as from a remote), with `connect` for the viewers' connections.
fn fixture_with(label: Option<&str>, connect: Connect, launch: Option<Launch>) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("root");
    tree(&root);
    let root = root.to_str().unwrap().to_string();
    let cache = tmp.path().join("cache");
    let launched: Launched = Arc::default();
    let record = launched.clone();
    let launch = launch.unwrap_or_else(|| {
        Arc::new(move |abs: &str, is_dir: bool| {
            record.lock().unwrap().push((abs.to_string(), is_dir));
            Ok(format!("opened {abs}"))
        })
    });
    let b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect,
        label: label.map(str::to_string),
        destination: label.map(str::to_string),
        abs: root.clone(),
        cache: cache.clone(),
        state_dir: tmp.path().join("state"),
        launch,
        git: None,
    });
    let mut fx = Fx {
        _tmp: tmp,
        root,
        cache,
        drv: Driver::new(1000.0, 700.0),
        b,
        launched,
    };
    fx.settle();
    fx
}

fn fixture() -> Fx {
    fixture_with(None, local_connect(), None)
}

impl Fx {
    /// Passes until the browser (and a viewer in it) is ready.
    fn settle(&mut self) {
        self.drv.until(&mut self.b, 20, |b| b.ready());
    }

    fn key(&mut self, key: Key) {
        self.drv.key(&mut self.b, key, Modifiers::NONE);
    }

    fn typed(&mut self, text: &str) {
        self.drv
            .frame(&mut self.b, vec![Event::Text(text.into())], Modifiers::NONE);
        self.drv.idle(&mut self.b, 1);
    }

    fn names(&self, panel: usize) -> Vec<String> {
        self.b.panels[panel]
            .entries
            .iter()
            .map(|e| e.name.clone())
            .collect()
    }

    /// The name under the bar of the active panel.
    fn current(&self) -> String {
        let panel = &self.b.panels[self.b.active];
        panel.current().map(|e| e.name.clone()).unwrap_or_default()
    }

    /// Puts the bar on `name` by typing its first letter until it is there.
    fn select(&mut self, name: &str) {
        let first = name.chars().next().unwrap().to_string();
        for _ in 0..10 {
            if self.current() == name {
                return;
            }
            self.typed(&first);
        }
        panic!("{name} not reached by typing; at {}", self.current());
    }

    /// `path` as the left side's frame shows it (a 1000-point window is 125 cells: the left side
    /// 62 of them, 496 points): whole, or its end after "…".
    fn framed(&self, path: &str) -> String {
        chrome::frame_title(&self.drv.ctx, path, 496.0)
    }

    /// `path` as the right side's frame shows it: that side has the odd column, 63 cells (504
    /// points). The frame the last pass drew there is held to that first, so the check is the
    /// 504-point side's whatever the path's length — a short path is whole in a 496-point frame
    /// too.
    fn framed_right(&self, path: &str) -> String {
        let right = panes(self).last().map(|r| (r.left(), r.width()));
        assert_eq!(right, Some((496.0, 504.0)), "the right side: 63 cells");
        chrome::frame_title(&self.drv.ctx, path, 504.0)
    }

    /// Opens `name` (Enter) and waits until its viewer is on screen and ready.
    fn open(&mut self, name: &str) {
        self.select(name);
        self.key(Key::Enter);
        self.settle();
        assert!(
            self.b.viewer.is_some(),
            "{name}: no viewer ({:?})",
            self.b.status.text
        );
    }
}

/// Folders first, then files, each by name; `..` on top; both panels on the opened folder.
#[test]
fn both_panels_list_the_folder_folders_first() {
    let fx = fixture();
    let want = [
        "..",
        "alpha",
        "beta",
        ".hidden",
        "data.bin",
        "main.rs",
        "notes.txt",
        "photo.png",
        "zeta.md",
    ];
    assert_eq!(fx.names(0), want);
    assert_eq!(fx.names(1), want);
    assert_eq!(fx.b.panels[0].dir, fx.root);
    // The footer: entries but `..`, the files' bytes, the free space.
    let footer = fx.b.panels[0].footer();
    assert!(footer.starts_with("8 Listed = "), "{footer}");
    assert!(footer.contains("Bytes Free"), "{footer}");
    // On screen: the path in the frame, the column heads, a hidden file dimmed, the F-keys.
    let framed = fx.framed(&fx.root);
    assert!(
        fx.drv.find_text(&framed).is_some(),
        "the path in the frame: {framed}"
    );
    assert!(fx.drv.find_text("Size").is_some());
    let dim = chrome::Palette::of(&fx.drv.ctx.global_style().visuals).dim;
    assert_eq!(
        fx.drv.colors_of(".hidden"),
        Some(vec![dim]),
        "a hidden file is dimmed"
    );
    assert!(fx.drv.find_text("View   ").is_some(), "the list's F-keys");
}

/// Arrows, Enter into a folder, Backspace back up (the bar on the folder left), Tab to the
/// other panel, a letter to the next name with it — through real key and text events.
#[test]
fn keys_move_enter_go_up_switch_panels_and_jump() {
    let mut fx = fixture();
    assert_eq!(fx.current(), "..");
    fx.key(Key::ArrowDown);
    assert_eq!(fx.current(), "alpha");
    fx.key(Key::Enter);
    fx.settle();
    assert_eq!(fx.b.panels[0].dir, format!("{}/alpha", fx.root));
    assert_eq!(fx.names(0), ["..", "inner.txt"]);
    let framed = fx.framed(&format!("{}/alpha", fx.root));
    assert!(framed.ends_with("/root/alpha"), "{framed}");
    assert!(
        fx.drv.find_text(&framed).is_some(),
        "the frame shows the folder entered"
    );
    assert_eq!(fx.current(), "..");
    fx.key(Key::Backspace);
    fx.settle();
    assert_eq!(fx.b.panels[0].dir, fx.root);
    assert_eq!(fx.current(), "alpha", "the bar is on the folder just left");
    // Into it again, then out through `..` with Enter.
    fx.key(Key::Enter);
    fx.settle();
    fx.key(Key::Enter);
    fx.settle();
    assert_eq!(
        (fx.b.panels[0].dir.as_str(), fx.current().as_str()),
        (fx.root.as_str(), "alpha")
    );
    // Letters jump to the next name with them, around the end, any case.
    fx.typed("n");
    assert_eq!(fx.current(), "notes.txt");
    fx.typed("Z");
    assert_eq!(fx.current(), "zeta.md");
    fx.typed("a");
    assert_eq!(fx.current(), "alpha", "around the end");
    fx.key(Key::End);
    assert_eq!(fx.current(), "zeta.md");
    fx.key(Key::Home);
    assert_eq!(fx.current(), "..");
    // Tab: the other panel, which kept its own bar and folder.
    fx.key(Key::Tab);
    assert_eq!(fx.b.active, 1);
    assert_eq!(fx.current(), "..");
    fx.typed("b");
    assert_eq!(fx.current(), "beta");
    assert_eq!(
        fx.b.panels[0].highlight, 0,
        "the first panel's bar did not move"
    );
    fx.key(Key::Tab);
    assert_eq!(fx.b.active, 0);
}

/// Enter on a text file shows it highlighted in the same window; Esc is back to the list, the
/// bar where it was.
#[test]
fn enter_on_code_shows_it_highlighted_and_esc_comes_back() {
    let mut fx = fixture();
    fx.open("main.rs");
    let colors = fx
        .drv
        .colors_of("fn main() {")
        .expect("the code is on screen");
    let distinct: std::collections::HashSet<_> = colors.iter().collect();
    assert!(distinct.len() >= 2, "highlighted: {colors:?}");
    assert!(fx.drv.find_text("Exit   ").is_some(), "the viewer's F-keys");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "main.rs", "the bar is kept");
    assert!(!fx.drv.closed(), "only the viewer went");
    // F3 opens a file as Enter does, and leaves the viewer too.
    fx.key(Key::F3);
    fx.settle();
    assert!(fx.b.viewer.is_some());
    fx.key(Key::F3);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "main.rs");
}

/// The text viewer's F2 joins the row: "Unwrap" while lines wrap (the default), "Wrap" once it
/// — or W — turned wrapping off; both work in the browser, and Esc still leaves.
#[test]
fn a_hosted_text_viewer_lists_its_wrap_key() {
    let mut fx = fixture();
    fx.open("notes.txt");
    for label in ["Exit   ", "Edit   ", "Unwrap "] {
        assert!(fx.drv.find_text(label).is_some(), "{label:?} in the row");
    }
    fx.key(Key::F2);
    assert!(fx.drv.find_text("Wrap   ").is_some(), "F2 unwrapped");
    assert!(fx.drv.find_text("Unwrap ").is_none());
    fx.key(Key::W);
    assert!(fx.drv.find_text("Unwrap ").is_some(), "W wrapped again");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert!(!fx.drv.closed(), "only the viewer went");
}

/// Drags a rectangle over the image in the viewer and crops to it: an unsaved edit.
fn edit_image(fx: &mut Fx) {
    let c = Pos2::new(500.0, 350.0);
    fx.drv.drag(
        &mut fx.b,
        PointerButton::Primary,
        c - Vec2::new(60.0, 40.0),
        c + Vec2::new(60.0, 40.0),
    );
    fx.drv.key(&mut fx.b, Key::C, Modifiers::NONE);
}

/// The close contract: an edited image is never dropped without its own prompt. Esc asks; the
/// prompt's Cancel keeps it; closing the browser window asks the same; Discard after Esc goes
/// back to the list (the window stays), Discard after closing the window closes it.
#[test]
fn an_edited_image_is_asked_before_it_goes() {
    let mut fx = fixture();
    // The session's journal is held between its folder and its lock, where a slow disk can keep
    // it a while: the image opened again below takes that folder, finds nothing in it and
    // removes it — no recovery row (07 §3), none with a Discard under the next prompt's.
    let gap = crate::journal::gap::arm(&fx.cache);
    fx.open("photo.png");
    edit_image(&mut fx);
    fx.drv.take_viewport_commands();
    fx.key(Key::Escape);
    assert!(
        fx.drv.find_text("Unsaved changes").is_some(),
        "Esc shows the image's prompt"
    );
    assert!(fx.b.viewer.is_some(), "and does not drop it");
    fx.drv.click_text(&mut fx.b, "Cancel");
    assert!(fx.b.viewer.is_some());
    assert!(fx.drv.find_text("Unsaved changes").is_none());
    // Closing the browser window: the image asks, the window stays.
    assert!(!Window::close_requested(&mut fx.b));
    fx.drv.idle(&mut fx.b, 1);
    assert!(fx.drv.find_text("Unsaved changes").is_some());
    fx.drv.click_text(&mut fx.b, "Cancel");
    // Esc again, then Discard: back to the list; the window does not close.
    fx.key(Key::Escape);
    fx.drv.click_text(&mut fx.b, "Discard");
    assert!(fx.b.viewer.is_none(), "Discard drops it");
    assert_eq!(fx.current(), "photo.png");
    assert!(
        !fx.drv.closed(),
        "a hosted Discard closes the viewer, not the window"
    );
    // Closing the window with an edit, then the prompt's Discard: the window closes.
    gap.reached();
    fx.key(Key::Enter);
    fx.settle();
    gap.release();
    let said: Vec<String> = fx.drv.texts().into_iter().map(|t| t.2).collect();
    assert!(
        !said.iter().any(|t| t.starts_with("unsaved edits")),
        "the discarded session's folder is offered: {said:?}"
    );
    edit_image(&mut fx);
    assert!(!Window::close_requested(&mut fx.b));
    fx.drv.idle(&mut fx.b, 1);
    fx.drv.click_text_on_top(&mut fx.b, "Discard");
    assert!(
        fx.b.viewer.is_none(),
        "the prompt's Discard drops it at once; on screen: {:?}",
        fx.drv.texts().into_iter().map(|t| t.2).collect::<Vec<_>>()
    );
    assert!(
        fx.drv.closed(),
        "Discard after closing the window closes it"
    );
    assert!(Window::close_requested(&mut fx.b));
    fx.drv.idle(&mut fx.b, 1);
    assert!(
        fx.b.leftover.borrow().closing.is_empty(),
        "nothing left running"
    );
}

/// Typing in the hex viewer (Tab, letters, arrows) is the viewer's: the list's bar and panel
/// stay where they were.
#[test]
fn keys_in_a_viewer_never_move_the_list() {
    let mut fx = fixture();
    fx.open("data.bin");
    // The hex view opens viewing (08 D35): Enter edits.
    fx.key(Key::Enter);
    fx.key(Key::Tab);
    fx.typed("n");
    fx.key(Key::ArrowDown);
    // Esc ends editing; the next one leaves.
    fx.key(Key::Escape);
    fx.key(Key::Escape);
    // The hex window's own journal write lands and its prompt gains a row: settled first.
    fx.settle();
    // The byte typed is an edit: the hex window asks.
    assert!(fx.drv.find_text("Unsaved changes").is_some());
    fx.drv.click_text(&mut fx.b, "[ Discard ]");
    assert!(fx.b.viewer.is_none());
    assert!(!fx.drv.closed());
    assert_eq!((fx.b.active, fx.current().as_str()), (0, "data.bin"));
}

/// A viewer's own F-keys join the browser's on its row (the hex window's Save, Goto and Asc/hx
/// beside Exit and Edit), and they work there; F3 still leaves the viewer, not the window.
#[test]
fn a_viewers_f_keys_join_the_browsers_row() {
    let mut fx = fixture();
    fx.open("data.bin");
    for label in ["Exit   ", "Edit   ", "Save   ", "Goto   ", "Asc/hx "] {
        assert!(fx.drv.find_text(label).is_some(), "{label:?} in the row");
    }
    // The first row is 00 01 02 …: F8 moves the typing to the ASCII column, where Z is a byte
    // once Enter has started editing (08 D35).
    fx.key(Key::F8);
    fx.key(Key::Enter);
    fx.typed("Z");
    assert!(
        fx.drv.text_starting_with("5A 01 02").is_some(),
        "F8 put the typing in the ASCII column"
    );
    fx.key(Key::F3);
    // The hex window's own journal write lands and its prompt gains a row: settled first.
    fx.settle();
    assert!(
        fx.drv.find_text("Unsaved changes").is_some(),
        "the hex window asks"
    );
    fx.drv.click_text(&mut fx.b, "[ Discard ]");
    assert!(fx.b.viewer.is_none());
    assert!(!fx.drv.closed(), "only the viewer went");
    assert!(
        fx.drv.find_text("View   ").is_some(),
        "the list's row again"
    );
    assert!(fx.drv.find_text("Asc/hx ").is_none());
}

/// In the browser the hex viewer's own F-keys work as in its own window: F5 saves an edit, F6
/// gives the Go to field the typing; under its unsaved-changes prompt F5 saves nothing.
#[test]
fn a_hosted_hex_viewers_f5_and_f6_work_and_respect_its_prompt() {
    let mut fx = fixture();
    let file = format!("{}/data.bin", fx.root);
    fx.open("data.bin");
    // The first row is 00 01 02 …: 7A over the first byte (Enter edits, 08 D35), then F5.
    fx.key(Key::Enter);
    fx.typed("7A");
    fx.key(Key::F5);
    fx.drv
        .until(&mut fx.b, 20, |_| std::fs::read(&file).unwrap()[0] == 0x7A);
    // F6: "16" goes into the Go to field (not over bytes 1–2), Enter goes there, FF over 16.
    fx.key(Key::F6);
    fx.typed("16");
    fx.key(Key::Enter);
    fx.typed("FF");
    assert!(
        fx.drv.text_starting_with("7A 01 02").is_some(),
        "row 0 as saved"
    );
    assert!(
        fx.drv.text_starting_with("FF 11 12").is_some(),
        "FF went to offset 16"
    );
    // Esc ends editing; the next one, with that edit: the prompt; F5 under it saves nothing.
    fx.key(Key::Escape);
    fx.key(Key::Escape);
    // The hex window's own journal write lands and its prompt gains a row: settled first.
    fx.settle();
    assert!(fx.drv.find_text("Unsaved changes").is_some());
    fx.key(Key::F5);
    fx.drv.idle(&mut fx.b, 5);
    assert_eq!(
        std::fs::read(&file).unwrap()[16],
        0x10,
        "F5 under the prompt"
    );
    fx.drv.click_text(&mut fx.b, "[ Discard ]");
    assert!(fx.b.viewer.is_none());
    assert_eq!(std::fs::read(&file).unwrap()[16], 0x10);
}

/// What a hosted hex viewer shows after Go to took `0x10` and Enter: no byte changed, and the
/// cursor at offset 16 — FF typed now goes over that byte.
fn went_to_16_with_nothing_changed(fx: &mut Fx) {
    let said: Vec<String> = fx.drv.texts().into_iter().map(|t| t.2).collect();
    let changed: Vec<&String> = said.iter().filter(|t| t.contains("changed")).collect();
    assert_eq!(changed, ["0 bytes changed"], "bytes were typed over");
    fx.typed("FF");
    assert!(
        fx.drv.text_starting_with("FF 11 12").is_some(),
        "the cursor went to offset 16"
    );
    assert!(fx.drv.find_text("1 byte changed").is_some(), "only that FF");
}

/// Go to in a hosted hex viewer keeps the typing when the platform says the window does not
/// have the keyboard: `SOPEN_KEYS` played into a window that came up behind another one (the
/// receipt's own route). The offset goes to the field and Enter goes there; no byte changes.
#[test]
fn a_hosted_hex_viewers_go_to_takes_played_keys_in_a_window_without_focus() {
    let mut fx = fixture();
    fx.open("data.bin");
    fx.drv.focused = false;
    // Editing first (Enter, 08 D35): a digit that missed the field would be a byte.
    fx.b.script = parse_keys("Enter,F6,text:0x10,Enter").unwrap().into();
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    fx.drv.focused = true;
    went_to_16_with_nothing_changed(&mut fx);
}

/// …and with a real keyboard: F6, the owner away in another application for a pass, back, the
/// offset, Enter.
#[test]
fn a_hosted_hex_viewers_go_to_is_still_open_after_the_window_lost_focus() {
    let mut fx = fixture();
    fx.open("data.bin");
    // Editing first (Enter, 08 D35): a digit that missed the field would be a byte.
    fx.key(Key::Enter);
    fx.key(Key::F6);
    fx.drv.focused = false;
    fx.drv.idle(&mut fx.b, 1);
    fx.drv.focused = true;
    fx.typed("0x10");
    fx.key(Key::Enter);
    went_to_16_with_nothing_changed(&mut fx);
}

/// The image viewer's F5 joins the browser's row beside Exit and Edit.
#[test]
fn a_hosted_image_viewer_lists_its_f5() {
    let mut fx = fixture();
    fx.open("photo.png");
    for label in ["Exit   ", "Edit   ", "Save   "] {
        assert!(fx.drv.find_text(label).is_some(), "{label:?} in the row");
    }
}

/// F3 while a viewer's text field has the typing (the hex window's Go to field) is the
/// field's: the browser does not leave the viewer.
#[test]
fn f3_in_a_viewers_text_field_does_not_leave_the_viewer() {
    let mut fx = fixture();
    fx.open("data.bin");
    fx.key(Key::F6);
    fx.typed("1");
    assert!(
        fx.drv.ctx.text_edit_focused(),
        "the Go to field has the typing"
    );
    fx.key(Key::F3);
    assert!(fx.b.viewer.is_some(), "still in the viewer");
    assert!(fx.drv.ctx.text_edit_focused(), "the field still has it");
    // Out of the field, F3 leaves as before.
    fx.key(Key::Enter);
    fx.key(Key::F3);
    assert!(fx.b.viewer.is_none(), "F3 leaves the viewer");
}

/// The panes the last pass drew in the central area: the bodies a frame fills in the skin's
/// background, the outermost ones — a preview's or a viewer's own frame inside a side is not
/// one — from the left. A body is taller than a row: the status row's cells are a run of the
/// body's colour of their own, one row tall.
fn panes(fx: &Fx) -> Vec<Rect> {
    let bodies: Vec<Rect> = (fx.drv.rects().into_iter())
        .filter(|&(r, fill, _)| {
            fill == chrome::PALETTE.back && r.width() >= 200.0 && r.height() > chrome::CELL.y
        })
        .map(|(r, _, _)| r)
        .collect();
    let mut out: Vec<Rect> = (bodies.iter().copied())
        .filter(|r| !bodies.iter().any(|o| o != r && o.contains_rect(*r)))
        .collect();
    out.sort_by(|a, b| a.left().total_cmp(&b.left()));
    out.dedup();
    out
}

/// Columns `x..x + w` of a 1000 × 700 window's grid in points (10 §4): the central area, under
/// the title row and above the status and F-key rows (a cell each), is 652 points tall from
/// y 16 — 40 rows of cells, the 12 points left at its foot the background.
fn grid_columns(x: u16, w: u16) -> Rect {
    let (left, width) = (f32::from(x) * 8.0, f32::from(w) * 8.0);
    Rect::from_min_size(Pos2::new(left, 16.0), Vec2::new(width, 40.0 * 16.0))
}

/// The sides stand on the central area's cell grid (10 §4): each side half its columns, the odd
/// one the right side's, a pane at its side's cells with no inset — in a 1000-point window, 125
/// columns: the left side 62 cells (496 points), the right 63 (504), lists and a preview alike;
/// the Enter viewer at all of the grid; 126 columns are two halves of 63.
#[test]
fn the_sides_and_the_viewer_stand_on_the_cell_grid() {
    let mut fx = fixture();
    let on_grid = |r: &Rect| {
        let whole = |v: f32, cell: f32| (v / cell).fract() == 0.0;
        let from = r.min - Pos2::new(0.0, 16.0);
        whole(from.x, 8.0)
            && whole(from.y, 16.0)
            && whole(r.width(), 8.0)
            && whole(r.height(), 16.0)
    };
    let sides = [grid_columns(0, 62), grid_columns(62, 63)];
    // Dual mode: two lists.
    fx.key(Key::F9);
    fx.settle();
    assert_eq!(fx.b.mode, view::Mode::Dual);
    let shown = panes(&fx);
    assert!(
        shown.iter().all(on_grid),
        "every pane at whole cells: {shown:?}"
    );
    assert_eq!(shown, sides, "two lists, 496 and 504 points");
    // Preview mode: the file under the bar on the right side, at its cells.
    fx.key(Key::F9);
    fx.select("notes.txt");
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    let display = fx.b.shown.clone().expect("a display");
    assert!(matches!(&display[1], view::Content::Preview(p) if p.ends_with("/notes.txt")));
    assert_eq!(
        panes(&fx),
        sides,
        "a list and a preview, 496 and 504 points"
    );
    // The viewer: the whole grid, in cells (X3-text) — its own bar on the first row and its
    // status row on the last, its frame between them.
    fx.key(Key::Enter);
    fx.settle();
    assert!(fx.b.viewer.is_some(), "the viewer");
    let all = grid_columns(0, 125);
    let layers = cells::layers(&fx.drv.ctx);
    let viewer = cells::find(&layers, "Text Viewer");
    let viewer = &layers[viewer.first().expect("the viewer in cells").layer];
    let a = viewer.buf.area;
    assert_eq!(viewer.rect(a), all, "the viewer at all the cells");
    let corners = [
        (a.left(), a.top() + 1, "╔"),
        (a.right() - 1, a.bottom() - 2, "╝"),
    ];
    for (x, y, corner) in corners {
        assert_eq!(viewer.buf[(x, y)].symbol(), corner, "its frame's ({x},{y})");
    }
    fx.key(Key::Escape);
    fx.settle();
    assert!(fx.b.viewer.is_none());
    // An even count of columns, with 4 points to spare: two equal halves.
    fx.drv.resize(1012.0, 700.0);
    fx.drv.idle(&mut fx.b, 3);
    let halves = [grid_columns(0, 63), grid_columns(63, 63)];
    assert_eq!(panes(&fx), halves, "126 columns: two of 63");
}

/// A window hosted as the viewer that keeps the room and the clip its last pass was given.
struct Probe(Arc<Mutex<Option<(Rect, Rect)>>>);

impl Window for Probe {
    fn ui(&mut self, ui: &mut egui::Ui) {
        *self.0.lock().unwrap() = Some((ui.max_rect(), ui.clip_rect()));
    }

    fn ready(&self) -> bool {
        true
    }
}

/// The viewer is given all of the grid's cells and is clipped to them: what a viewer draws past
/// its room is cut at the grid, never in the 8 points at its foot (10 §4).
#[test]
fn the_viewer_is_given_the_grid_and_clipped_to_it() {
    let mut fx = fixture();
    let given = Arc::new(Mutex::new(None));
    let probe = Embedded {
        window: Box::new(Probe(given.clone())),
        settle: Box::new(|_| Some(Outcome::default())),
    };
    let (ctx, abs) = (fx.drv.ctx.clone(), format!("{}/notes.txt", fx.root));
    let means = view_set::Means {
        path: abs.clone(),
        name: "notes.txt".to_string(),
        title: abs.clone(),
        connect: local_connect(),
        label: None,
        in_place: false,
    };
    let opened = view_set::OpenedAs {
        kind: crate::kind::Kind::Text,
        stat: LocalFs::new().stat(&abs).unwrap(),
        undecodable: None,
    };
    let views = view_set::open(&ctx, &opened, means, None);
    let made = view_set::Made {
        embedded: probe,
        hold: None,
        switch: None,
    };
    fx.b.view(&ctx, made, abs, false, views);
    fx.drv.idle(&mut fx.b, 2);
    let all = grid_columns(0, 125);
    assert_eq!(
        *given.lock().unwrap(),
        Some((all, all)),
        "its room, its clip"
    );
}

/// The git panel over both sides (Cmd/Ctrl+Shift+G) is at all of the grid's cells, as the
/// viewer is (10 §4).
#[test]
fn the_wide_git_panel_is_the_whole_grid() {
    let Some(git) = crate::git::history::fixture::git() else {
        return;
    };
    let mut g = s2::Gx::new(s2::small(&git));
    g.wait_for_the_panel();
    g.tab_into_git();
    g.with(Key::G, Modifiers::COMMAND | Modifiers::SHIFT);
    g.settle();
    assert_eq!(
        g.shown(),
        [g.git(), g.git()],
        "the git panel over both sides"
    );
    assert_eq!(panes(&g.fx), [grid_columns(0, 125)], "at all the cells");
}

/// The two sides share an edge (10 §4), and a point on it is one side's — the right one's, as a
/// cell's left edge is the cell's: one wheel notch at x 496 scrolls the right list alone, and
/// at y 656, the grid's foot, neither.
#[test]
fn a_point_on_the_edge_two_sides_share_is_one_sides() {
    let mut fx = fixture();
    fx.key(Key::F9);
    fx.settle();
    let many = format!("{}/alpha", fx.root);
    for i in 0..200 {
        std::fs::write(format!("{many}/f{i:03}.txt"), "x").unwrap();
    }
    fx.b.list(None, many, None, None);
    fx.settle();
    // The rows one notch scrolls at `x`, `y`, in the left list and the right one.
    let notch = |fx: &mut Fx, x: f32, y: f32| {
        let before = [fx.b.panels[0].top, fx.b.panels[1].top];
        fx.drv.wheel(&mut fx.b, Pos2::new(x, y), -1.0);
        [
            fx.b.panels[0].top - before[0],
            fx.b.panels[1].top - before[1],
        ]
    };
    let seen = [(496.0, 300.0), (250.0, 656.0), (495.0, 655.0)].map(|(x, y)| notch(&mut fx, x, y));
    assert_eq!(
        seen,
        [[0, 3], [0, 0], [3, 0]],
        "the edge the right side's, the grid's foot neither side's, the left side's last \
         point its own"
    );
}

/// A press on the edge the git panel on the left shares with the list on the right is the
/// list's: the keyboard does not go into the git panel (10 §4).
#[test]
fn a_press_on_the_edge_beside_the_git_panel_is_not_the_git_panels() {
    let Some(git) = crate::git::history::fixture::git() else {
        return;
    };
    let mut g = s2::Gx::new(s2::small(&git));
    g.wait_for_the_panel();
    // The right list active, its bar on `..`: the git panel on the left.
    g.fx.b.active = 1;
    let git_side = g.git();
    g.until("the git panel on the left", |b| {
        b.shown.as_ref().is_some_and(|s| s[0] == git_side)
    });
    g.settle();
    assert!(!g.fx.b.git.focus);
    g.fx.drv.click(&mut g.fx.b, Pos2::new(496.0, 300.0));
    assert!(!g.fx.b.git.focus, "the edge is the list's");
    g.fx.drv.click(&mut g.fx.b, Pos2::new(495.0, 300.0));
    assert!(g.fx.b.git.focus, "the git panel's last column is its own");
}

/// Cells `x..x + w` of row `y` of the central area's grid in points: under the title row (a
/// cell), whatever the window's size.
fn grid_cells(x: u16, y: u16, w: u16) -> Rect {
    let at = Pos2::new(f32::from(x) * 8.0, 16.0 + f32::from(y) * 16.0);
    Rect::from_min_size(at, Vec2::new(f32::from(w) * 8.0, 16.0))
}

/// The panel's foot never hides its count: on the narrowest window, and with a total too long
/// for the panel, "N Listed" is drawn whole inside each panel's frame, on its row above the
/// bottom border — centred on whole cells while it fits, the free space giving way first, then
/// the end of the total, from the frame's first cell inside.
#[test]
fn the_foot_always_shows_the_count_inside_the_frame() {
    let mut fx = fixture();
    // Wide enough, it is the whole line, centred.
    let whole = fx.b.panels[0].footer();
    assert!(whole.contains(" Bytes Free"), "{whole}");
    assert!(fx.drv.find_text(&whole).is_some(), "{whole}");
    fx.drv.resize(480.0, 320.0);
    fx.drv.idle(&mut fx.b, 3);
    // 60 columns: two sides of 30 cells, inside each frame cells 1..29 and 31..59; 272 points
    // under the title row and above the status and F-key rows: 17 rows, the foot in row 15.
    let feet = |fx: &Fx| {
        let feet = fx.drv.texts_starting_with("8 Listed = ");
        assert_eq!(feet.len(), 2, "both counts whole on screen: {feet:?}");
        feet
    };
    let shown = feet(&fx);
    let count = &shown[0].1;
    assert!(
        !count.contains("Bytes Free"),
        "the free space gave way: {count}"
    );
    assert!(count.ends_with(" bytes"), "{count}");
    // Centred in the 28 cells inside: the gap's odd cell is the right one's.
    let w = count.chars().count() as u16;
    let gap = (28 - w) / 2;
    let want = [grid_cells(1 + gap, 15, w), grid_cells(31 + gap, 15, w)];
    let got: Vec<Rect> = shown.iter().map(|(r, _)| *r).collect();
    assert_eq!(got, want, "{count:?} centred inside each frame");
    // A total far too long for the panel: its end cut, from the first cell inside.
    for panel in &mut fx.b.panels {
        for e in &mut panel.entries {
            if e.kind == EntryKind::File {
                e.size = u64::MAX;
            }
        }
    }
    fx.drv.idle(&mut fx.b, 2);
    let shown = feet(&fx);
    let cut = &shown[0].1;
    assert!(cut.ends_with('…'), "the end of the total gave way: {cut}");
    let got: Vec<Rect> = shown.iter().map(|(r, _)| *r).collect();
    assert_eq!(
        got,
        [grid_cells(1, 15, 28), grid_cells(31, 15, 28)],
        "{cut:?}"
    );
}

/// Every cell of a row fits its column at a glyph boundary — never half a glyph: a long Hangul
/// name (two cells a glyph) and a long ASCII name end in "…" inside the Name column, a long
/// extension inside Ext, each with the blank cell of the gap after it; a size too long for its
/// column is said in a larger unit, right-aligned in it.
#[test]
fn cells_fit_their_columns_at_a_glyph_boundary() {
    let mut fx = fixture();
    // The ideographic space is two cells, as the Hangul around it.
    let hangul = "한글\u{3000}폴더\u{3000}이름이\u{3000}아주\u{3000}길다";
    std::fs::create_dir(format!("{}/{hangul}", fx.root)).unwrap();
    let ascii = "a_very_long_ascii_file_name.verylongextension";
    std::fs::write(format!("{}/{ascii}", fx.root), b"x").unwrap();
    // Listed again: into alpha and back up.
    fx.select("alpha");
    fx.key(Key::Enter);
    fx.settle();
    fx.key(Key::Backspace);
    fx.settle();
    for panel in &mut fx.b.panels {
        for e in &mut panel.entries {
            if e.name == ascii {
                e.size = 1 << 40;
            }
        }
    }
    fx.drv.resize(480.0, 320.0);
    fx.drv.idle(&mut fx.b, 3);
    // The left panel's inside is cells 1..29: 28 cells — Name 6 (cells 1..7), a gap, Ext 5
    // (8..13), a gap, Size 14 (14..28); its rows are 1..15 of the grid's 17.
    let rows = |x: u16, w: u16| -> Vec<String> {
        let area = grid_cells(x, 1, w).union(grid_cells(x, 15, w));
        fx.drv.column(area).into_iter().map(|(_, t)| t).collect()
    };
    let (names, gap, exts, after_ext) = (rows(1, 6), rows(7, 1), rows(8, 5), rows(13, 1));
    assert_eq!(names.len(), 15, "a row of the list per row: {names:?}");
    // Each cut name, the cell after its column a blank of the gap — half a wide glyph would
    // leave it empty (a wide glyph's second cell shows nothing).
    let cut = |texts: &[String], full: &str| -> usize {
        let start: String = full.chars().take(1).collect();
        let y = (texts.iter())
            .position(|t| t.starts_with(&start) && t.trim_end().ends_with('…'))
            .unwrap_or_else(|| panic!("{full:?}: no cut cell in its column: {texts:?}"));
        let kept = texts[y].trim_end().trim_end_matches('…');
        assert!(
            !kept.is_empty() && full.starts_with(kept),
            "{:?} is a start of {full:?}",
            texts[y]
        );
        y
    };
    let y = cut(&names, hangul);
    assert_eq!(
        names[y], "한글… ",
        "two Hangul on four cells, … on the fifth"
    );
    assert_eq!(gap[y], " ", "the gap after the Hangul name");
    let y = cut(&names, "a_very_long_ascii_file_name");
    assert_eq!((names[y].as_str(), gap[y].as_str()), ("a_ver…", " "));
    assert_eq!(cut(&exts, "verylongextension"), y, "the same row");
    assert_eq!((exts[y].as_str(), after_ext[y].as_str()), ("very…", " "));
    // 1 TiB in bytes does not fit 14 cells: the smallest unit that does (MB), at the column's
    // right end.
    let sizes = rows(14, 14);
    assert_eq!(sizes[y], format!("{:>14}", "1,099,512 MB"));
}

/// F4 runs the real editor launch (`zed::launch`) — VS Code's CLI here is a script that writes
/// down its arguments — for the folder or file under the bar, and for the file in a viewer.
#[test]
fn f4_opens_the_entry_in_the_editor() {
    use std::os::unix::fs::PermissionsExt;
    let bin = tempfile::tempdir().unwrap();
    let record = bin.path().join("argv");
    let code = bin.path().join("code");
    std::fs::write(
        &code,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\n",
            record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&code, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Started once first: a stub just written can be "busy" on Linux for a moment (another test
    // thread's fork), and its first start on macOS can be slow.
    crate::testutil::run_fresh(&mut std::process::Command::new(&code));
    std::fs::remove_file(&record).unwrap();
    let editor = crate::zed::Editor::Code(code);
    let launch: Launch = Arc::new(move |abs: &str, is_dir: bool| {
        let name = crate::zed::launch(&editor, None, abs, is_dir)?;
        Ok(format!("opened {abs} in {name}"))
    });
    let mut fx = fixture_with(None, local_connect(), Some(launch));
    let seen = |n: usize, fx: &mut Fx| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            fx.drv.idle(&mut fx.b, 1);
            let text = std::fs::read_to_string(&record).unwrap_or_default();
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            if lines.len() >= n && fx.b.launches.is_empty() {
                return lines;
            }
            assert!(std::time::Instant::now() < deadline, "{lines:?}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    fx.select("alpha");
    fx.key(Key::F4);
    let lines = seen(1, &mut fx);
    assert_eq!(lines, [format!("{}/alpha", fx.root)]);
    assert!(
        fx.b.status.text.ends_with("in VS Code"),
        "{}",
        fx.b.status.text
    );
    fx.open("main.rs");
    fx.key(Key::F4);
    let lines = seen(2, &mut fx);
    assert_eq!(lines[1], format!("{}/main.rs", fx.root));
    assert!(fx.b.viewer.is_some(), "F4 leaves the viewer open");
}

/// A local image is copied into the cache as the standalone route copies it (Save needs the
/// bytes as opened), and the copy is released with the viewer — opening it again uses the same
/// copy, not a session copy of its own. (A remote one is read through a stream and reaches the
/// cache not at all: `w8b_a_remote_image_opens_through_a_stream_with_nothing_in_the_cache`.)
#[test]
fn a_viewer_holds_its_cached_copy_only_while_it_shows_it() {
    let mut fx = fixture();
    fx.open("photo.png");
    let copy = crate::cache::cache_path(
        &fx.cache,
        crate::cache::LOCAL_HOST,
        &format!("{}/photo.png", fx.root),
    );
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(format!("{}/photo.png", fx.root)).unwrap()
    );
    assert_eq!(fx.drv.pictures().len(), 1);
    fx.key(Key::Escape);
    fx.open("photo.png");
    assert!(
        !fx.cache.join(".sessions").exists(),
        "the second open got a session copy: the first one's claim was still held"
    );
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
}

/// Every file under `dir`: a `dir` that is not there has none, and anything else that keeps
/// it from being read fails the test (an empty answer would pass for "nothing was written").
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => panic!("cannot list {}: {e}", dir.display()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_in(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// 06 §13 W8a, the browser's Enter in the window: remote text and a remote binary open in
/// their viewers through a stream — the text is not followed as a local file is, nothing
/// reaches the cache — and Esc is back in the list. In preview mode the same Enter hands the
/// preview's stream over; here (`dual`) each opens from the start.
#[test]
fn w8a_enter_on_remote_text_and_binaries_views_them_through_streams() {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    fx.key(Key::F9);
    fx.open("notes.txt");
    assert!(fx.drv.find_text("Text Viewer").is_some());
    assert!(fx.drv.find_text("plain notes").is_some());
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new(), "text");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    fx.open("data.bin");
    assert!(
        fx.drv
            .find_text("00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F")
            .is_some(),
        "the hex rows"
    );
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new(), "hex");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "data.bin");
}

/// The browser "on host box" over real SFTP (this machine's `sftp-server`), every connection
/// metered by the fixture ([`Meter`]) with these budgets, `name` holding `size` bytes (sparse)
/// — or `body` — and listed. `None` (the test skips) where there is no `sftp-server`.
fn metered(
    stream_budget: u64,
    whole_budget: u64,
    name: &str,
    body: Result<&[u8], u64>,
) -> Option<(Fx, Arc<Meter>, String)> {
    crate::remote::SftpRemote::local_server()?;
    let meter = Meter::new(stream_budget, whole_budget);
    let connect = meter.connect(|| {
        let server = crate::remote::SftpRemote::local_server().expect("sftp-server");
        Ok(Box::new(server) as Box<dyn RemoteFs + Send>)
    });
    let mut fx = fixture_with(Some("box"), connect, None);
    let abs = format!("{}/{name}", fx.root);
    match body {
        Ok(bytes) => std::fs::write(&abs, bytes).unwrap(),
        Err(size) => std::fs::File::create(&abs).unwrap().set_len(size).unwrap(),
    }
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    // Dual mode: Enter opens from the start (in preview mode it hands the preview over).
    fx.key(Key::F9);
    Some((fx, meter, abs))
}

/// 06 §12 V1 at the browser's Enter, over real SFTP, by the fixture's own count: a remote
/// binary of the owner's size (24 GB, sparse) is on screen after one read of one block — the
/// sniff's 8 KiB before it, nothing read whole, nothing in the cache. The budget (that block
/// and 4 MiB) is not passed: a route that fetched the file would have failed at once.
#[test]
fn w8a_enter_on_a_24_gb_remote_binary_transfers_one_block() {
    const SIZE: u64 = 24_000_000_000;
    let block = crate::stream::BLOCK;
    let Some((mut fx, meter, abs)) = metered(block + (4 << 20), 0, "model.safetensors", Err(SIZE))
    else {
        return;
    };
    fx.open("model.safetensors");
    assert!(
        fx.drv
            .find_text("00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00")
            .is_some(),
        "the first rows"
    );
    assert_eq!(meter.reads_of(&abs), vec![(0, block as u32)], "one block");
    assert_eq!(meter.totals(), (block, 1, 0, 0), "what crossed");
    assert_eq!(meter.heads.load(Ordering::SeqCst), 8 << 10, "the sniff");
    assert!(!meter.tripped());
    assert_eq!(meter.connects.load(Ordering::SeqCst), 1, "one session");
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
}

/// A text body of exactly `size` bytes, in lines.
fn lines_of(size: usize) -> Vec<u8> {
    let mut body = Vec::with_capacity(size + 64);
    let mut n = 0;
    while body.len() < size {
        body.extend_from_slice(
            format!("line {n:08} of a remote text file, padded out\n").as_bytes(),
        );
        n += 1;
    }
    body.truncate(size - 1);
    body.push(b'\n');
    body
}

/// The browser's Enter on remote text at the limit, over real SFTP, by the fixture's count.
/// Exactly 16 MiB streams: the first screen's block is the first read, nothing is read whole,
/// and when the index pass is done the file has crossed once — the budget is the file's size,
/// so a byte read twice would have passed it. One byte more streams too (W5b): its first block
/// first, nothing read whole (its whole budget is nothing), nothing in the cache.
#[test]
fn w8a_enter_on_remote_text_at_16_mib_streams_and_one_byte_more_too() {
    let cap = crate::gui::text::HIGHLIGHT_MAX;
    let block = crate::stream::BLOCK;
    let body = lines_of(cap as usize);
    let Some((mut fx, meter, abs)) = metered(cap, 0, "at-limit.log", Ok(&body)) else {
        return;
    };
    fx.open("at-limit.log");
    assert!(fx.drv.find_text("Text Viewer").is_some());
    assert_eq!(meter.reads_of(&abs).first(), Some(&(0, block as u32)));
    let (bytes, _, whole_bytes, whole_reads) = meter.totals();
    assert!(
        bytes >= block && bytes <= cap,
        "{bytes} at the first screen"
    );
    assert_eq!((whole_bytes, whole_reads), (0, 0), "nothing read whole");
    fx.drv.until(&mut fx.b, 60, |_| {
        meter.stream_bytes.load(Ordering::SeqCst) >= cap
    });
    fx.drv.idle(&mut fx.b, 20);
    assert_eq!(
        meter.totals(),
        (cap, meter.totals().1, 0, 0),
        "crossed once"
    );
    assert!(!meter.tripped(), "no byte was asked for twice");
    assert_eq!(meter.connects.load(Ordering::SeqCst), 1);
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());

    let body = lines_of(cap as usize + 1);
    let Some((mut fx, meter, abs)) = metered(cap + 1, 0, "over-limit.log", Ok(&body)) else {
        return;
    };
    fx.open("over-limit.log");
    assert!(fx.drv.find_text("Text Viewer").is_some());
    assert_eq!(meter.reads_of(&abs).first(), Some(&(0, block as u32)));
    let (_, _, whole_bytes, whole_reads) = meter.totals();
    assert_eq!((whole_bytes, whole_reads), (0, 0), "nothing read whole");
    assert!(!meter.tripped(), "no byte was asked for twice");
    assert_eq!(meter.connects.load(Ordering::SeqCst), 1);
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
}

/// W8b, the browser's Enter on a remote image (dual mode: from the start): its window reads it
/// through a stream on the open's connection and shows the picture — nothing reaches the cache —
/// and Esc is back in the list.
#[test]
fn w8b_a_remote_image_opens_through_a_stream_with_nothing_in_the_cache() {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    fx.key(Key::F9);
    fx.open("photo.png");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while fx.drv.pictures().is_empty() {
        assert!(std::time::Instant::now() < deadline, "no picture");
        fx.drv.idle(&mut fx.b, 1);
    }
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "photo.png");
}

/// A PNG that says it is `w`×`h` and holds one empty row (its header is real, its pixel data is
/// not): a decode it asks for would be `w × h × 3` bytes.
fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
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
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

/// 06 §7.3, §12 V15 (W8b): a remote image that does not decode opens in the hex window, on
/// the stream it was read through — not the error row of before — and nothing is in the cache.
#[test]
fn w8b_a_remote_image_that_does_not_decode_opens_in_the_hex_window() {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
    broken.extend((0..200u32).map(|i| (i * 7) as u8));
    std::fs::write(format!("{}/broken.png", fx.root), &broken).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.key(Key::F9);
    fx.select("broken.png");
    fx.key(Key::Enter);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while fx
        .drv
        .find_text("89 50 4E 47 0D 0A 1A 0A  00 07 0E 15 1C 23 2A 31")
        .is_none()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "no hex rows ({:?})",
            fx.b.status.text
        );
        fx.drv.idle(&mut fx.b, 1);
    }
    assert!(fx.b.viewer.is_some(), "{:?}", fx.b.status.text);
    assert!(
        shows(&fx, "cannot decode the image"),
        "why, in the hex window's rows"
    );
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
    fx.settle();
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "broken.png");
}

/// One pass whose input holds every key of `keys`, pressed in that order (then their releases
/// on the next pass): what the owner's keys are when a pass is long (a loaded machine).
fn pressed_together(fx: &mut Fx, keys: &[Key]) {
    for pressed in [true, false] {
        let events = keys
            .iter()
            .map(|&key| Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: Modifiers::NONE,
            })
            .collect();
        fx.drv.frame(&mut fx.b, events, Modifiers::NONE);
    }
}

/// Passes, one at a time, until `done` (a bound that only catches a hang).
fn pass_until(fx: &mut Fx, what: &str, done: impl Fn(&Browser) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !done(&fx.b) {
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: not in 60 s ({:?})",
            fx.b.status.text
        );
        fx.drv.idle(&mut fx.b, 1);
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Esc on the pass right after a viewer's first one closes it. The hex window's grid takes the
/// keyboard on its first pass, and egui keeps Esc from taking the keyboard away only from the
/// second pass on (a widget's lock filter needs it focused the pass before): Esc on that
/// second pass takes the grid's focus — which is no text field's, so it is the owner's
/// leaving, not a field's Esc. Opened from the list (a remote binary, drawn on the pass it
/// opens) and given way to by an image that does not decode (drawn on the pass after the
/// switch). Each Esc is pressed on that exact pass, counted, never timed.
#[test]
fn w8b_esc_on_a_hex_windows_second_pass_closes_it() {
    let mut broken = b"\x89PNG\r\n\x1a\n".to_vec();
    broken.extend((0..200u32).map(|i| (i * 7) as u8));
    for (name, switched) in [("data.bin", false), ("broken.png", true)] {
        let mut fx = fixture_with(Some("box"), local_connect(), None);
        std::fs::write(format!("{}/broken.png", fx.root), &broken).unwrap();
        let root = fx.root.clone();
        fx.b.list(Some(0), root, None, None);
        fx.settle();
        fx.key(Key::F9);
        fx.select(name);
        fx.key(Key::Enter);
        if switched {
            // The pass that switched: the hex window is the viewer, not drawn yet.
            pass_until(&mut fx, "the switch", |b| {
                b.viewer.as_ref().is_some_and(|v| v.switch.is_none())
            });
            // Its first pass.
            fx.drv.idle(&mut fx.b, 1);
        } else {
            // The pass it opened on is its first.
            pass_until(&mut fx, "the hex window", |b| b.viewer.is_some());
        }
        assert!(
            fx.drv.find_text("Go to").is_some() || shows(&fx, "0x0 (0)"),
            "{name}: the hex window was drawn"
        );
        fx.key(Key::Escape);
        assert!(fx.b.viewer.is_none(), "{name}: Esc on its second pass");
        assert_eq!(fx.current(), name);
    }
}

/// W8b: H and Esc in one pass on a remote image's memory line — the owner left as the image
/// gave way — closes the viewer: the hex window is never installed after the owner left, and
/// nothing of it is shown.
#[test]
fn w8b_esc_in_the_pass_an_image_gives_way_leaves_it() {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    std::fs::write(
        format!("{}/huge.png", fx.root),
        png_claiming(100_000, 100_000),
    )
    .unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.key(Key::F9);
    fx.select("huge.png");
    fx.key(Key::Enter);
    pass_until(&mut fx, "the memory line", |b| {
        b.viewer.is_some() && b.ready()
    });
    fx.drv.idle(&mut fx.b, 1);
    assert!(shows(&fx, "Hex view (2)"), "the memory line");
    pressed_together(&mut fx, &[Key::H, Key::Escape]);
    fx.drv.idle(&mut fx.b, 3);
    assert!(fx.b.viewer.is_none(), "the owner left");
    assert!(
        fx.drv
            .find_text("89 50 4E 47 0D 0A 1A 0A  00 00 00 0D 49 48 44 52")
            .is_none(),
        "no hex rows"
    );
    assert_eq!(fx.current(), "huge.png");
}

/// Some text on screen holds `part` (a line of the window's rows, wrapped or not): a text egui
/// drew, or a row of cells — across its runs, so a highlighted line's colours do not split it
/// (the text viewer is cells, X3-text).
fn shows(fx: &Fx, part: &str) -> bool {
    let texts = fx.drv.texts();
    texts.iter().any(|(_, _, text)| text.contains(part))
        || !cells::find(&cells::layers(&fx.drv.ctx), part).is_empty()
}

/// 06 §7.3, §12 V15 (W8b): the memory line of a remote image too large to decode without asking
/// offers Hex view, and H switches to the hex window — the same stream, the file's first rows.
#[test]
fn w8b_hex_view_on_the_memory_line_switches_to_the_hex_window() {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    std::fs::write(
        format!("{}/huge.png", fx.root),
        png_claiming(100_000, 100_000),
    )
    .unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.key(Key::F9);
    fx.select("huge.png");
    fx.key(Key::Enter);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !shows(&fx, "Hex view (2)") {
        assert!(
            std::time::Instant::now() < deadline,
            "no memory line with Hex view ({:?})",
            fx.b.status.text
        );
        fx.drv.idle(&mut fx.b, 1);
    }
    fx.key(Key::H);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while fx
        .drv
        .find_text("89 50 4E 47 0D 0A 1A 0A  00 00 00 0D 49 48 44 52")
        .is_none()
    {
        assert!(std::time::Instant::now() < deadline, "no hex rows");
        fx.drv.idle(&mut fx.b, 1);
    }
    assert!(fx.b.viewer.is_some());
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
}

/// W8b: the private preview copies' root an older sopen made (`<cache>/.preview`) is swept when
/// a browser window opens — the folders of windows that are gone, then the root itself, empty.
#[test]
fn w8b_a_legacy_preview_root_is_swept_when_the_browser_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("root");
    tree(&root);
    let cache = tmp.path().join("cache");
    let legacy = cache.join(".preview");
    // A window of an older sopen that is gone: its folder, a copy in it, and its lock, free.
    std::fs::create_dir_all(legacy.join("0a1b2c3d4e5f")).unwrap();
    std::fs::write(legacy.join("0a1b2c3d4e5f/0.png"), b"left behind").unwrap();
    std::fs::write(legacy.join("0a1b2c3d4e5f.lock"), b"").unwrap();
    let b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect: local_connect(),
        label: None,
        destination: None,
        abs: root.to_str().unwrap().to_string(),
        cache: cache.clone(),
        state_dir: tmp.path().join("state"),
        launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
        git: None,
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while legacy.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the legacy root stayed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(b);
}

/// 06 §7.3 for a local image (the memory line "applies to local images too"): one too large to
/// decode without asking shows the memory line with Hex view in the browser, and H shows the
/// file's bytes, read in place.
#[test]
fn w8b_a_local_image_too_large_to_decode_asks_and_offers_hex_view() {
    let mut fx = fixture();
    std::fs::write(
        format!("{}/huge.png", fx.root),
        png_claiming(100_000, 100_000),
    )
    .unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.key(Key::F9);
    fx.select("huge.png");
    fx.key(Key::Enter);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !shows(&fx, "Hex view (2)") {
        assert!(
            std::time::Instant::now() < deadline,
            "no memory line with Hex view ({:?})",
            fx.b.status.text
        );
        fx.drv.idle(&mut fx.b, 1);
    }
    assert!(shows(&fx, "decoding this image needs"), "the memory line");
    fx.key(Key::H);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while fx
        .drv
        .find_text("89 50 4E 47 0D 0A 1A 0A  00 00 00 0D 49 48 44 52")
        .is_none()
    {
        assert!(std::time::Instant::now() < deadline, "no hex rows");
        fx.drv.idle(&mut fx.b, 1);
    }
    assert!(fx.b.viewer.is_some());
}

/// W8b at the browser's Enter over real SFTP, by the fixture's count: a 2 GiB remote image
/// (sparse, a PNG header at its front) shows its window and its memory line having read its
/// header's block — nothing read whole (the budget is nothing), one session. H then shows its
/// hex rows on the same stream: no stream opened again, no connection, no byte read again.
#[test]
fn w8b_enter_on_a_2_gib_remote_image_reads_its_header_only() {
    let block = crate::stream::BLOCK;
    let Some((mut fx, meter, abs)) = metered(block, 0, "huge.png", Ok(&png_claiming(9, 9))) else {
        return;
    };
    std::fs::File::options()
        .write(true)
        .open(&abs)
        .unwrap()
        .set_len(2 << 30)
        .unwrap();
    fx.select("huge.png");
    fx.key(Key::Enter);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !shows(&fx, "Continue (Enter)") {
        assert!(
            std::time::Instant::now() < deadline,
            "no memory line ({:?})",
            fx.b.status.text
        );
        fx.drv.idle(&mut fx.b, 1);
    }
    let (_, _, whole_bytes, whole_reads) = meter.totals();
    assert_eq!((whole_bytes, whole_reads), (0, 0), "nothing read whole");
    assert_eq!(meter.reads_of(&abs), vec![(0, block as u32)], "its header");
    assert!(!meter.tripped());
    assert_eq!(meter.connects.load(Ordering::SeqCst), 1, "one session");
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
    // Hex view: the same stream, what it holds already.
    fx.key(Key::H);
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while fx
        .drv
        .find_text("89 50 4E 47 0D 0A 1A 0A  00 00 00 0D 49 48 44 52")
        .is_none()
    {
        assert!(std::time::Instant::now() < deadline, "no hex rows");
        fx.drv.idle(&mut fx.b, 1);
    }
    assert_eq!(
        meter.opened.lock().unwrap().len(),
        1,
        "no stream opened again"
    );
    assert_eq!(
        meter.reads_of(&abs),
        vec![(0, block as u32)],
        "nothing read again"
    );
    assert_eq!(meter.connects.load(Ordering::SeqCst), 1, "no connection");
    assert!(!meter.tripped());
    assert_eq!(files_in(&fx.cache), Vec::<PathBuf>::new());
}

/// 06 §12 V10 at open, in the browser: the open's own connection is refused as sshd refuses a
/// session. The status row says 06 §6's line — not ssh's words — nothing opens, and the list is
/// as it was; once sessions are free again the same Enter opens the file.
#[test]
fn w8a_v10_a_refused_session_at_open_is_said_on_the_status_row() {
    let refused = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let flag = refused.clone();
    let connect: Connect = Arc::new(move || {
        if flag.load(Ordering::SeqCst) {
            anyhow::bail!(
                "cannot start sftp on box: mux_client_request_session: session request failed: \
                 Session open refused by peer"
            );
        }
        Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)
    });
    let mut fx = fixture_with(Some("box"), connect, None);
    fx.key(Key::F9);
    fx.select("data.bin");
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.b, 10, |b| b.opening.is_none());
    assert!(fx.b.viewer.is_none(), "nothing opened");
    assert_eq!(fx.b.status.text, crate::stream::SESSION_REFUSED);
    assert_eq!(fx.current(), "data.bin", "the list is as it was");
    refused.store(false, Ordering::SeqCst);
    fx.key(Key::Enter);
    fx.drv.until(&mut fx.b, 10, |b| b.viewer.is_some());
    fx.settle();
    assert!(
        fx.drv
            .find_text("00 01 02 03 04 05 06 07  08 09 0A 0B 0C 0D 0E 0F")
            .is_some(),
        "the hex rows"
    );
}

/// A worker held at a known point until the test lets it go: `hold` (on the worker) says it
/// is there and waits; the test waits for `reached`, acts, then `release`s it.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<(bool, bool)>, std::sync::Condvar)>);

impl Gate {
    fn hold(&self) {
        let (lock, cv) = &*self.0;
        let mut st = lock.lock().unwrap();
        st.0 = true;
        cv.notify_all();
        while !st.1 {
            st = cv.wait(st).unwrap();
        }
    }

    /// Waits (at most 10 s) until a worker is held.
    fn reached(&self) {
        let (lock, cv) = &*self.0;
        let st = lock.lock().unwrap();
        let (st, timeout) = cv
            .wait_timeout_while(st, std::time::Duration::from_secs(10), |s| !s.0)
            .unwrap();
        assert!(
            !timeout.timed_out() && st.0,
            "the worker never got to the gate"
        );
    }

    fn release(&self) {
        let (lock, cv) = &*self.0;
        lock.lock().unwrap().1 = true;
        cv.notify_all();
    }

    /// Passes of the browser until a worker is held (at most 10 s): what it waits on may need
    /// the window's passes first (a save waits for its journal).
    fn reached_running(&self, fx: &mut Fx) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !self.0 .0.lock().unwrap().0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never got to the gate"
            );
            fx.drv.idle(&mut fx.b, 1);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// The local disk posing as a remote whose `read_to` (a fetch: after the first chunk),
/// `create_from` (a save's upload) or `read_head` (a sniff) stops at a gate.
struct Gated {
    fs: LocalFs,
    gate: Gate,
    at: &'static str,
}

impl RemoteFs for Gated {
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
        if self.at == "read_head" {
            self.gate.hold();
        }
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
        if self.at != "read_to" {
            return self.fs.read_to(p, out, progress);
        }
        let bytes = std::fs::read(p)?;
        let mut done = 0u64;
        for (i, chunk) in bytes.chunks(64 * 1024).enumerate() {
            out.write_all(chunk)?;
            done += chunk.len() as u64;
            progress(done);
            if i == 0 {
                self.gate.hold();
            }
        }
        Ok(done)
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        if self.at == "create_from" {
            self.gate.hold();
        }
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
    /// A stream reads on the local disk, ungated (06 §2).
    fn into_ranged(
        self: Box<Self>,
        path: &str,
    ) -> std::result::Result<Box<dyn crate::remote::Ranged>, crate::remote::Refused> {
        Box::new(self.fs).into_ranged(path)
    }
}

/// A `Connect` whose connections stop at `gate` in `at`.
fn gated(gate: &Gate, at: &'static str) -> Connect {
    let gate = gate.clone();
    Arc::new(move || {
        Ok(Box::new(Gated {
            fs: LocalFs::new(),
            gate: gate.clone(),
            at,
        }) as Box<dyn RemoteFs + Send>)
    })
}

/// Esc during a copy stops it: back to the list, nothing of it kept in the cache. (A local
/// image is what is still copied whole — into the cache, as the standalone route copies it;
/// every remote file opens as a stream, 06 §13.)
#[test]
fn esc_stops_a_fetch() {
    let gate = Gate::default();
    let mut fx = fixture_with(None, gated(&gate, "read_to"), None);
    std::fs::write(format!("{}/big.png", fx.root), vec![1u8; 2 << 20]).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.select("big.png");
    fx.key(Key::Enter);
    // The worker is held inside the copy, a chunk in.
    gate.reached();
    fx.drv.idle(&mut fx.b, 1);
    let shown = fx.b.status_line().text;
    assert_eq!(shown, "opening big.png…");
    fx.key(Key::Escape);
    gate.release();
    fx.drv.until(&mut fx.b, 10, |b| b.opening.is_none());
    assert!(fx.b.viewer.is_none());
    assert!(
        fx.b.status.text.starts_with("cancelled"),
        "{}",
        fx.b.status.text
    );
    let copy = crate::cache::cache_path(
        &fx.cache,
        crate::cache::LOCAL_HOST,
        &format!("{}/big.png", fx.root),
    );
    let left: Vec<String> = std::fs::read_dir(copy.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("big.png"))
        .collect();
    assert!(left.is_empty(), "nothing of the fetch is kept: {left:?}");
}

/// The `SOPEN_LIST` hook's text: the path, a row per entry (folders first), the footer.
#[test]
fn the_listing_hook_prints_what_the_panel_shows() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    tree(&root);
    let lines = listing(&mut LocalFs::new(), Some("box"), root.to_str().unwrap()).unwrap();
    assert_eq!(lines[0], format!("box:{}", root.display()));
    assert!(
        lines[1].starts_with("..") && lines[1].ends_with("<UP>"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].starts_with("alpha ") && lines[2].contains("<DIR>"),
        "{}",
        lines[2]
    );
    let main = lines.iter().find(|l| l.starts_with("main ")).unwrap();
    assert!(
        main.contains(" rs ") && main.contains(&format!(" {} ", RUST.len())),
        "{main}"
    );
    assert!(
        lines.last().unwrap().starts_with("8 Listed = "),
        "{lines:?}"
    );
}

/// A link to a file is opened as the file it points at (spec §5: the write target is the path
/// resolved when the file is opened): a byte typed in the hex view and saved lands in that
/// file, and the link stays a link.
#[test]
fn a_link_is_saved_through_to_its_file() {
    let mut fx = fixture();
    std::os::unix::fs::symlink("data.bin", format!("{}/link.bin", fx.root)).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("link.bin");
    // The hex view opens viewing (08 D35): Enter edits.
    fx.key(Key::Enter);
    fx.typed("41");
    fx.drv
        .key(&mut fx.b, Key::S, crate::gui::testkit::command(false));
    fx.settle();
    let data = std::fs::read(format!("{}/data.bin", fx.root)).unwrap();
    assert_eq!(
        data[0], 0x41,
        "the save landed in the file the link points at"
    );
    let link = std::fs::symlink_metadata(format!("{}/link.bin", fx.root)).unwrap();
    assert!(link.file_type().is_symlink(), "the link is still a link");
}

/// The wheel scrolls a long panel's rows without moving the bar, and the next key brings the
/// rows back to the bar.
#[test]
fn the_wheel_scrolls_the_rows_and_a_key_brings_back_the_bar() {
    let mut fx = fixture();
    let many = format!("{}/alpha", fx.root);
    for i in 0..200 {
        std::fs::write(format!("{many}/f{i:03}.txt"), "x").unwrap();
    }
    fx.b.list(Some(0), many, None, None);
    fx.settle();
    assert_eq!(fx.b.panels[0].top, 0);
    fx.drv.wheel(&mut fx.b, Pos2::new(250.0, 300.0), -10.0);
    let top = fx.b.panels[0].top;
    assert_eq!(top, 30, "ten wheel lines are thirty rows");
    fx.drv.idle(&mut fx.b, 2);
    assert_eq!(
        fx.b.panels[0].top, top,
        "the rows stay where the wheel left them"
    );
    assert_eq!(fx.b.panels[0].highlight, 0, "the bar did not move");
    fx.key(Key::ArrowDown);
    let p = &fx.b.panels[0];
    assert_eq!(
        (p.highlight, p.top),
        (1, 1),
        "a key scrolls back to the bar"
    );
}

/// In the image viewer Esc first clears a drawn rectangle (the image window's own Esc); with no
/// rectangle it leaves.
#[test]
fn esc_clears_a_rectangle_before_it_leaves_the_image() {
    let mut fx = fixture();
    fx.open("photo.png");
    let c = Pos2::new(500.0, 350.0);
    fx.drv.drag(
        &mut fx.b,
        PointerButton::Primary,
        c - Vec2::new(60.0, 40.0),
        c + Vec2::new(60.0, 40.0),
    );
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_some(), "the first Esc cleared the rectangle");
    assert!(fx.drv.find_text("Unsaved changes").is_none());
    fx.key(Key::Escape);
    assert!(
        fx.b.viewer.is_none(),
        "the second Esc left (nothing was edited)"
    );
}

/// Esc while a file is being made ready — before any byte is fetched, or after (a local file is
/// read in place, an image decoded): the viewer does not appear once it is ready.
#[test]
fn esc_while_opening_keeps_the_viewer_from_appearing() {
    let gate = Gate::default();
    let held = gate.clone();
    let connect: Connect = Arc::new(move || {
        held.hold();
        Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)
    });
    let mut fx = fixture_with(None, connect, None);
    fx.select("main.rs");
    fx.key(Key::Enter);
    gate.reached();
    assert!(fx.b.opening.is_some());
    // Esc while the worker is held before it has read anything; then it goes on to the end.
    fx.key(Key::Escape);
    gate.release();
    fx.drv.until(&mut fx.b, 10, |b| b.opening.is_none());
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.is_none(), "the viewer appeared after Esc");
    assert!(
        fx.b.status.text.starts_with("cancelled"),
        "{}",
        fx.b.status.text
    );
    assert_eq!(fx.current(), "main.rs");
}

/// The `SOPEN_STATE` `open=NAME` receipt hook: the entry opens once the folder is listed.
#[test]
fn the_open_state_opens_an_entry_once_listed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    tree(&root);
    let mut b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect: local_connect(),
        label: None,
        destination: None,
        abs: root.to_str().unwrap().to_string(),
        cache: tmp.path().join("cache"),
        state_dir: tmp.path().join("state"),
        launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
        git: None,
    });
    b.open_first = Some("main.rs".to_string());
    let mut drv = Driver::new(1000.0, 700.0);
    drv.until(&mut b, 20, |b| b.ready() && b.viewer.is_some());
    assert!(drv.find_text("fn main() {").is_some());
    assert_eq!(b.panels[0].current().unwrap().name, "main.rs");
}

/// With `open=NAME`, `SOPEN_STATE` `at=OFFSET` opens the text viewer with the row that byte
/// is in at the top.
#[test]
fn the_at_state_opens_a_text_file_at_that_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let body: String = (0..1000).map(|i| format!("row {i:04}\n")).collect();
    std::fs::write(root.join("rows.txt"), &body).unwrap();
    let mut b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect: local_connect(),
        label: None,
        destination: None,
        abs: root.to_str().unwrap().to_string(),
        cache: tmp.path().join("cache"),
        state_dir: tmp.path().join("state"),
        launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
        git: None,
    });
    b.open_first = Some("rows.txt".to_string());
    // Inside "row 0600".
    b.open_at = Some(600 * 9 + 5);
    let mut drv = Driver::new(1000.0, 700.0);
    drv.until(&mut b, 20, |b| b.ready() && b.viewer.is_some());
    let rows = drv.texts_starting_with("row ");
    let top = rows
        .iter()
        .min_by(|a, b| a.0.top().total_cmp(&b.0.top()))
        .map(|(_, t)| t.as_str());
    assert_eq!(top, Some("row 0600"), "{rows:?}");
}

/// `SOPEN_STATE` `loading=1` as the browser parsed it reaches the text viewer it opens (06
/// §11), with no environment variable read on the way; without the key the viewer is told no.
#[test]
fn the_loading_state_reaches_the_text_viewer_the_browser_opens() {
    assert!(
        std::env::var_os("SOPEN_STATE").is_none(),
        "the test reads no env"
    );
    for (text, want) in [("open=rows.txt;loading=1", true), ("open=rows.txt", false)] {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        std::fs::write(root.join("rows.txt"), "row 0\nrow 1\n").unwrap();
        let mut b = Browser::new(Start {
            fs: Box::new(LocalFs::new()),
            connect: local_connect(),
            label: None,
            destination: None,
            abs: root.to_str().unwrap().to_string(),
            cache: tmp.path().join("cache"),
            state_dir: tmp.path().join("state"),
            launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
            git: None,
        });
        let state = crate::gui::State::parse(text).unwrap();
        b.scripted(&state);
        assert_eq!(b.open_loading, want, "{text}");
        let mut drv = Driver::new(1000.0, 700.0);
        drv.until(&mut b, 20, |b| b.ready() && b.viewer.is_some());
        assert!(drv.find_text("row 1").is_some(), "{text}: the viewer is up");
        assert_eq!(
            crate::gui::text::embedded_loading(),
            Some(want),
            "{text}: what the viewer was handed"
        );
    }
}

/// Video in the browser: Enter shows the media window (its first frame decoded), Esc leaves it.
/// Needs ffmpeg (skips without it, unless `SOPEN_REQUIRE_FFMPEG` is set).
#[test]
fn a_video_opens_in_the_media_window_and_esc_leaves_it() {
    let tools = match crate::ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    let mut fx = fixture();
    // A bounded wait: a stuck ffmpeg fails the test instead of hanging it.
    let mut child = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=1",
        ])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(format!("{}/clip.mp4", fx.root))
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("ffmpeg did not make the fixture in 60 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(status.success(), "fixture");
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    // `open` waits for the viewer to be ready: the media window's first frame decoded.
    fx.open("clip.mp4");
    let info = fx.drv.text_starting_with("160×120 · 25.00 fps · ");
    assert!(
        info.as_deref().is_some_and(|i| i.ends_with("no audio")),
        "the media window's info line: {info:?}"
    );
    let frame = fx.drv.pictures();
    assert!(
        frame.iter().any(|r| r.width() > 50.0 && r.height() > 30.0),
        "the decoded frame is drawn: {frame:?}"
    );
    assert!(fx.drv.find_text("Exit   ").is_some(), "the viewer's F-keys");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "clip.mp4");
    assert!(!fx.drv.closed());
}

/// Enter on a remote `.mp4` that does not open: the status row says why about the file as its
/// owner knows it, `host:/abs`, named once — never the URL of the byte server ffprobe read it
/// through, nor a cache copy: one that is not a video is what ffprobe said of it. An empty one
/// opens on Hex, the reason its first row (08 D10: the byte server has no range of it to serve,
/// and its words are not shown). From the list (dual mode: no preview) and from the preview's
/// card, which says the same. Needs ffmpeg (skips without it, unless `SOPEN_REQUIRE_FFMPEG` is
/// set).
#[test]
fn a_remote_file_that_fails_to_open_is_named_as_host_abs_never_as_its_cache_copy() {
    if let Err(e) = crate::ffmpeg::resolve() {
        if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}");
        }
        eprintln!("skipping: {e}");
        return;
    }
    let files: [(&str, Vec<u8>); 2] =
        [("k_zero.mp4", Vec::new()), ("k_junk.mp4", vec![0x55; 4096])];
    for mode in [view::Mode::Dual, view::Mode::Preview] {
        for (name, bytes) in &files {
            let mut fx = fixture_with(Some("box"), local_connect(), None);
            fx.b.mode = mode;
            std::fs::write(format!("{}/{name}", fx.root), bytes).unwrap();
            let root = fx.root.clone();
            fx.b.list(None, root, None, None);
            fx.settle();
            fx.select(name);
            // The bar rests (past the 150 ms commit): in preview mode the card is on screen.
            fx.drv.idle(&mut fx.b, 12);
            fx.settle();
            if mode == view::Mode::Preview {
                let card = match bytes.is_empty() {
                    true => "the file is empty (0 bytes)",
                    false => "ffprobe cannot read",
                };
                assert!(shows(&fx, card), "{name}: the card");
                for never in ["Server returned", "127.0.0.1", "http://"] {
                    assert!(!shows(&fx, never), "{name}: the card shows {never}");
                }
            }
            fx.key(Key::Enter);
            fx.settle();
            if bytes.is_empty() {
                let on = fx.b.viewer.as_ref().and_then(|v| v.views.set.on_screen());
                assert_eq!(on, Some(crate::views::View::Hex), "{mode:?} {name}: on Hex");
                let why = "the file is empty (0 bytes); it is shown as bytes";
                assert!(shows(&fx, why), "{mode:?} {name}: the reason first");
                for never in ["Server returned", "127.0.0.1", "http://"] {
                    assert!(!shows(&fx, never), "{mode:?} {name}: {never} shown");
                }
                continue;
            }
            assert!(fx.b.viewer.is_none(), "{mode:?} {name}: nothing to show");
            let said = fx.b.status.text.clone();
            assert!(fx.b.status.error, "{mode:?}: {said}");
            let known = format!("box:{}/{name}", fx.root);
            assert!(
                said.starts_with(&format!("ffprobe cannot read {known}: ")),
                "{mode:?}: the file as its owner knows it: {said}"
            );
            for never in ["Server returned", "127.0.0.1", "http://"] {
                assert!(!said.contains(never), "{mode:?}: {said}");
            }
            let cache = std::fs::canonicalize(&fx.cache).unwrap_or_else(|_| fx.cache.clone());
            for copy in [fx.cache.as_path(), cache.as_path()] {
                assert!(
                    !said.contains(copy.to_str().unwrap()),
                    "{mode:?}: the cache copy is named: {said}"
                );
            }
            assert_eq!(said.matches(*name).count(), 1, "{mode:?}: {said}");
        }
    }
}

/// The preview of a remote file ffprobe reads but that holds no picture and no sound (a
/// Matroska file with only subtitles): the card says so about the file as its owner knows it,
/// `host:/abs` — never about the URL ffprobe read it through.
/// Needs ffmpeg (skips without it, unless `SOPEN_REQUIRE_FFMPEG` is set).
#[test]
fn a_remote_preview_without_a_stream_is_named_as_host_abs_never_as_its_preview_copy() {
    let tools = match crate::ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => return eprintln!("skipping: {e}"),
    };
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    let srt = fx._tmp.path().join("a.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();
    let made = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(&srt)
        .args(["-c:s", "srt"])
        .arg(format!("{}/subs.mkv", fx.root))
        .status()
        .unwrap();
    assert!(made.success(), "fixture");
    let root = fx.root.clone();
    fx.b.list(None, root, None, None);
    fx.settle();
    fx.select("subs.mkv");
    // The bar rests (past the 150 ms commit): the card is on screen.
    fx.drv.idle(&mut fx.b, 12);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    let said: Vec<String> = fx.drv.texts().into_iter().map(|t| t.2).collect();
    let known = format!("box:{}/subs.mkv has no audio or video stream", fx.root);
    let about: Vec<&String> = said.iter().filter(|t| t.contains("no audio")).collect();
    assert_eq!(about, [&known], "the card's reason");
    assert!(
        !said.iter().any(|t| t.contains("127.0.0.1")),
        "the byte server is named: {about:?}"
    );
}

/// Cmd/Ctrl+C, X and V reach a hosted image as in its own window (the browser takes none of
/// them): Copy and Cut put pictures on the clipboard, a paste lands, and leaving asks before
/// the edits go.
#[test]
fn clipboard_shortcuts_reach_a_hosted_image() {
    let mut fx = fixture();
    fx.open("photo.png");
    fx.drv.take_commands();
    fx.drv.copy(&mut fx.b);
    let copied: Vec<[usize; 2]> = fx
        .drv
        .take_commands()
        .into_iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyImage(image) => Some(image.size),
            _ => None,
        })
        .collect();
    assert_eq!(copied, [[64, 48]], "the whole image went to the clipboard");
    assert!(fx.b.viewer.is_some());
    // Copy changed nothing: leaving asks nothing.
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none(), "Copy is not an edit");

    // A paste alone is an edit. Pasted without a rectangle it selects what it covers: the
    // first Esc clears that, the second asks before the paste goes. Its journal is held between
    // its folder and its lock, where a slow disk can keep it a while: the image opened again
    // below takes that folder, finds nothing in it and removes it — no recovery row (07 §3),
    // none with a Discard under the next prompt's.
    let gap = crate::journal::gap::arm(&fx.cache);
    fx.open("photo.png");
    let red = egui::ColorImage::new([4, 4], vec![egui::Color32::RED; 16]);
    fx.drv.paste_image(&mut fx.b, red);
    fx.key(Key::Escape);
    assert!(fx.drv.find_text("Unsaved changes").is_none());
    assert!(fx.b.viewer.is_some());
    fx.key(Key::Escape);
    assert!(
        fx.drv.find_text("Unsaved changes").is_some(),
        "the paste was an edit"
    );
    fx.drv.click_text(&mut fx.b, "Discard");
    assert!(fx.b.viewer.is_none());

    // Cut alone: a drawn rectangle's pixels to the clipboard, the rectangle left in the
    // background colour — an edit too.
    gap.reached();
    fx.open("photo.png");
    gap.release();
    let said: Vec<String> = fx.drv.texts().into_iter().map(|t| t.2).collect();
    assert!(
        !said.iter().any(|t| t.starts_with("unsaved edits")),
        "the discarded session's folder is offered: {said:?}"
    );
    let c = Pos2::new(500.0, 350.0);
    fx.drv.drag(
        &mut fx.b,
        PointerButton::Primary,
        c - Vec2::new(60.0, 40.0),
        c + Vec2::new(60.0, 40.0),
    );
    fx.drv.take_commands();
    fx.drv.cut(&mut fx.b);
    let cut = fx
        .drv
        .take_commands()
        .into_iter()
        .filter(|c| matches!(c, egui::OutputCommand::CopyImage(_)))
        .count();
    assert_eq!(cut, 1, "Cut put the rectangle on the clipboard");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_some(), "the first Esc cleared the rectangle");
    fx.key(Key::Escape);
    assert!(
        fx.drv.find_text("Unsaved changes").is_some(),
        "the cut was an edit"
    );
    fx.drv.click_text_on_top(&mut fx.b, "Discard");
    assert!(
        fx.b.viewer.is_none(),
        "the prompt's Discard drops it at once; on screen: {:?}",
        fx.drv.texts().into_iter().map(|t| t.2).collect::<Vec<_>>()
    );
    assert!(!fx.drv.closed());
}

/// An entry whose name is not UTF-8 (it shows with U+FFFD) is never opened or handed to the
/// editor: the status says why.
#[test]
fn an_entry_named_in_bad_utf8_is_not_opened() {
    let mut fx = fixture();
    let mut entries = fx.b.panels[0].entries[1..].to_vec();
    entries.push(crate::remote::Entry {
        name: "q\u{FFFD}.txt".to_string(),
        lossy: true,
        kind: EntryKind::File,
        link: false,
        size: 3,
        mtime: std::time::SystemTime::UNIX_EPOCH,
    });
    let dir = fx.root.clone();
    fx.b.panels[0].fill(dir, entries, None, None);
    fx.select("q\u{FFFD}.txt");
    fx.key(Key::Enter);
    assert!(fx.b.opening.is_none() && fx.b.viewer.is_none());
    assert!(
        fx.b.status.text.contains("not valid UTF-8"),
        "{}",
        fx.b.status.text
    );
    fx.key(Key::F4);
    fx.drv.idle(&mut fx.b, 2);
    assert!(
        fx.launched.lock().unwrap().is_empty(),
        "F4 handed it to no editor"
    );
}

/// P1: a path of wide glyphs (Hangul) is fitted by its laid-out width: the text drawn in the
/// frame, "…" included, fits the room and keeps the path's end.
#[test]
fn a_wide_path_is_fitted_by_its_real_width() {
    let fx = fixture();
    let font = chrome::font();
    // The test kit has the skin's VGA font and egui's own after it, where Hangul (which a
    // system font draws wide) is as wide as "0"; the ideographic space U+3000 is wider there.
    let path = format!("/srv/{}/마지막폴더", "한\u{3000}글\u{3000}".repeat(12));
    let w = |t: &str| {
        fx.drv.ctx.fonts_mut(|f| {
            f.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE)
                .size()
                .x
        })
    };
    assert!(
        w("\u{3000}") > w("0") * 1.2,
        "the case needs a glyph wider than 0"
    );
    for room in [120.0, 260.0, 400.0] {
        let fitted = chrome::fit_left(&fx.drv.ctx, &path, &font, room);
        let width = w(&fitted);
        assert!(width <= room, "{fitted:?} is {width} wide in {room}");
        assert!(
            fitted.starts_with('…') && fitted.ends_with("마지막폴더"),
            "{fitted:?}"
        );
    }
    let short = "/a/b";
    assert_eq!(chrome::fit_left(&fx.drv.ctx, short, &font, 400.0), short);
}

/// A size too long for its column is said in the smallest unit that fits (powers of 1000,
/// rounded); one that fits, and what is not a file, as they are.
#[test]
fn a_size_too_long_for_its_column_takes_a_larger_unit() {
    let file = |size: u64| crate::remote::Entry {
        name: "f".to_string(),
        lossy: false,
        kind: EntryKind::File,
        link: false,
        size,
        mtime: std::time::SystemTime::UNIX_EPOCH,
    };
    assert_eq!(panel::size_in(&file(99_999_999_999), 14), "99,999,999,999");
    assert_eq!(panel::size_in(&file(1 << 40), 14), "1,099,512 MB");
    assert_eq!(panel::size_in(&file(u64::MAX), 14), "18,446,744 TB");
    assert_eq!(panel::size_in(&file(1_500), 4), "2 KB");
    for size in [0, 1, 999, 1 << 20, 1 << 40, u64::MAX] {
        assert!(
            panel::size_in(&file(size), 14).chars().count() <= 14,
            "{size}"
        );
    }
    let mut dir = file(0);
    dir.kind = EntryKind::Special;
    assert_eq!(
        panel::size_in(&dir, 4),
        "<SPECIAL>",
        "a kind is never turned into a unit"
    );
}

/// P2: a dot file keeps its whole name, however many dots (spec §3a): no extension.
#[test]
fn dot_files_keep_their_whole_name() {
    let file = |name: &str| crate::remote::Entry {
        name: name.to_string(),
        lossy: false,
        kind: EntryKind::File,
        link: false,
        size: 1,
        mtime: std::time::SystemTime::UNIX_EPOCH,
    };
    assert_eq!(panel::split_name(&file(".bashrc")), (".bashrc", ""));
    assert_eq!(
        panel::split_name(&file(".config.json")),
        (".config.json", "")
    );
    assert_eq!(panel::split_name(&file("main.rs")), ("main", "rs"));
    assert_eq!(panel::split_name(&file("a.tar.gz")), ("a.tar", "gz"));
}

/// P-nice: a time before 1970 with a fraction of a second floors (the second it is in), and
/// the footer adds sizes without overflow.
#[test]
fn early_times_floor_and_the_footer_never_overflows() {
    let mut e = crate::remote::Entry {
        name: "old".to_string(),
        lossy: false,
        kind: EntryKind::File,
        link: false,
        size: u64::MAX,
        mtime: std::time::SystemTime::UNIX_EPOCH - std::time::Duration::from_millis(500),
    };
    assert_eq!(panel::unix_secs(e.mtime), -1);
    assert_eq!(panel::unix_secs(std::time::SystemTime::UNIX_EPOCH), 0);
    let mut p = Panel::new("/x");
    e.name = "b".into();
    let mut f = e.clone();
    f.name = "c".into();
    p.fill("/x".into(), vec![e, f], None, None);
    assert!(
        p.footer()
            .starts_with("2 Listed = 36,893,488,147,419,103,230 bytes"),
        "{}",
        p.footer()
    );
}

/// B1: an F4 launch that finishes later (here: fails after a moment) wakes the window by
/// itself — no input comes — and its error shows on the next pass.
#[test]
fn a_finished_f4_launch_wakes_the_window() {
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let go = Arc::new(Mutex::new(go_rx));
    let launch: Launch = Arc::new(move |_: &str, _: bool| {
        go.lock().unwrap().recv().unwrap();
        anyhow::bail!("VS Code was not found and Zed was not found; install an editor")
    });
    let mut fx = fixture_with(None, local_connect(), Some(launch));
    fx.select("main.rs");
    fx.key(Key::F4);
    assert!(fx.b.status.text.starts_with("opening in the editor"));
    // From here no pass runs until the window asks for one: every repaint request is seen.
    let woken = Arc::new(Mutex::new(Vec::new()));
    let seen = woken.clone();
    fx.drv
        .ctx
        .set_repaint_observer(move |info| seen.lock().unwrap().push(info.delay));
    go_tx.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !woken.lock().unwrap().iter().any(|d| d.is_zero()) {
        assert!(
            std::time::Instant::now() < deadline,
            "the window was never woken"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    fx.drv.idle(&mut fx.b, 1);
    assert!(
        fx.b.status.error && fx.b.status.text.contains("Zed was not found"),
        "{}",
        fx.b.status.text
    );
}

/// B-nice: a listing that works after one that failed clears the failure from the status row.
#[test]
fn a_good_listing_clears_an_earlier_listing_error() {
    let mut fx = fixture();
    let gone = format!("{}/gone", fx.root);
    fx.b.list(Some(0), gone, None, None);
    fx.settle();
    let said = fx.b.status_line();
    assert!(
        said.error && said.text.starts_with("left panel: "),
        "{}",
        said.text
    );
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    let said = fx.b.status_line();
    assert!(!said.error && fx.b.status.text.is_empty(), "{}", said.text);
}

/// X1: leaving never cuts a running save off; Esc is 07 §10's Stop. The upload is held at a gate.
/// Esc during it stops the save up to its rename: the viewer stays and says so, the file is as
/// before the save and its backup holds the original. A close asked for during a held upload
/// waits — the window says it closes when the save is done — and the window closes once the
/// save has landed, the file holding the edit.
#[test]
fn a_running_save_is_not_cut_off() {
    let walk = |dir: &std::path::Path| -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    todo.push(p);
                } else {
                    out.push(p);
                }
            }
        }
        out
    };
    // Esc: stopped, the viewer stays.
    let gate = Gate::default();
    let mut fx = fixture_with(None, gated(&gate, "create_from"), None);
    let photo = format!("{}/photo.png", fx.root);
    let original = std::fs::read(&photo).unwrap();
    fx.open("photo.png");
    edit_image(&mut fx);
    fx.drv
        .key(&mut fx.b, Key::S, crate::gui::testkit::command(false));
    gate.reached_running(&mut fx);
    fx.key(Key::Escape);
    gate.release();
    let stopped = "stopped: nothing was written; the original is untouched";
    for _ in 0..200 {
        let said = fx
            .drv
            .texts()
            .iter()
            .any(|(_, _, t)| t.starts_with(stopped));
        if said {
            break;
        }
        fx.drv.idle(&mut fx.b, 1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        fx.drv
            .texts()
            .iter()
            .any(|(_, _, t)| t.starts_with(stopped)),
        "it says it was stopped"
    );
    assert!(fx.b.viewer.is_some(), "Esc during the save left the image");
    assert!(!fx.drv.closed());
    assert_eq!(
        std::fs::read(&photo).unwrap(),
        original,
        "the file as before"
    );
    let backups = walk(&fx.cache.join(".backup"));
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    let dir = std::path::Path::new(&photo).parent().unwrap();
    let temps: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains(".sopen-"))
        .collect();
    assert!(temps.is_empty(), "the temp removed");
    // The picture is still the edited one, and unsaved: its size line is the crop's, and the
    // status bar says where its edits are kept.
    let texts: Vec<String> = fx.drv.texts().into_iter().map(|(_, _, t)| t).collect();
    let size = texts
        .iter()
        .find(|t| t.contains(" PNG · "))
        .unwrap_or_else(|| panic!("the size line: {texts:?}"));
    assert!(
        !size.starts_with("64×48 "),
        "the edit is still there: {size}"
    );
    assert!(
        texts.iter().any(|t| t.starts_with("unsaved")),
        "still unsaved: {texts:?}"
    );

    // F3 (leaving the viewer) during the save: it waits for it; once it has landed, the viewer
    // goes back to the list, the window open.
    let gate = Gate::default();
    let mut fx = fixture_with(None, gated(&gate, "create_from"), None);
    fx.open("photo.png");
    edit_image(&mut fx);
    fx.drv
        .key(&mut fx.b, Key::S, crate::gui::testkit::command(false));
    gate.reached_running(&mut fx);
    fx.key(Key::F3);
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.is_some(), "F3 during the save waits for it");
    gate.release();
    fx.drv.until(&mut fx.b, 10, |b| b.viewer.is_none());
    assert!(!fx.drv.closed(), "back to the list, the window open");
    let (w, h) = image::image_dimensions(format!("{}/photo.png", fx.root)).unwrap();
    assert!((w, h) != (64, 48), "the crop was saved: {w}×{h}");

    // A close during the save: it waits, then closes once the save has landed.
    let gate = Gate::default();
    let mut fx = fixture_with(None, gated(&gate, "create_from"), None);
    fx.open("photo.png");
    edit_image(&mut fx);
    fx.drv
        .key(&mut fx.b, Key::S, crate::gui::testkit::command(false));
    gate.reached_running(&mut fx);
    assert!(
        !Window::close_requested(&mut fx.b),
        "closing the window waits"
    );
    fx.drv.idle(&mut fx.b, 2);
    assert!(
        fx.drv
            .find_text("the window closes when the save is done")
            .is_some(),
        "it says why it stays"
    );
    assert!(fx.b.viewer.is_some() && !fx.drv.closed());
    gate.release();
    fx.drv.until(&mut fx.b, 10, |b| b.viewer.is_none());
    assert!(fx.drv.closed(), "the window closes once the save is done");
    let (w, h) = image::image_dimensions(format!("{}/photo.png", fx.root)).unwrap();
    assert!(
        (w, h) != (64, 48) && w > 0 && h > 0,
        "the crop was saved: {w}×{h}"
    );
    assert!(
        fx.cache.join(".backup").exists(),
        "the original was backed up first"
    );
}

/// A text file read in place is followed at its end (the viewer asks to look again each
/// second); a copy fetched from a host is not. Counted at the viewer's own follow request, not
/// by repaint delays: with the title row's clock in a minute's last second it asks for a repaint
/// a second away too (the preview build's failure at :59).
#[test]
fn only_a_local_text_file_is_followed() {
    for (label, followed) in [(None, true), (Some("box"), false)] {
        let mut fx = fixture_with(label, local_connect(), None);
        crate::gui::chrome::set_test_now(Some(crate::gui::testkit::START_OF_A_MINUTE + 59));
        fx.open("notes.txt");
        let before = crate::gui::text::follows();
        fx.drv.idle(&mut fx.b, 1);
        let asked = crate::gui::text::follows() > before;
        assert_eq!(asked, followed, "{label:?}");
    }
}

/// Audio in the browser (message 13): the media viewer's F5 Save joins the row; its edits are
/// its own keys there (Cmd/Ctrl+X cuts in the viewer, the browser takes none of it); leaving
/// with an edit asks first, and Discard leaves without writing anything.
#[test]
fn a_hosted_media_viewer_lists_f5_and_leaving_with_an_edit_asks() {
    let tools = match crate::ffmpeg::resolve() {
        Ok(t) => t,
        Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
            panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
        }
        Err(e) => {
            eprintln!("skipping: {e}");
            return;
        }
    };
    let mut fx = fixture();
    let wav = format!("{}/tone.wav", fx.root);
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .arg(&wav)
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture");
    let original = std::fs::read(&wav).unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("tone.wav");
    for label in ["Exit   ", "Edit   ", "Save   "] {
        assert!(fx.drv.find_text(label).is_some(), "{label:?} in the row");
    }
    // Select the first half with I/O at the playhead and cut it.
    fx.key(Key::ArrowRight);
    fx.key(Key::O);
    fx.drv.cut(&mut fx.b);
    assert!(
        fx.drv.text_starting_with("unsaved: ").is_some(),
        "the cut landed in the viewer"
    );
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_some(), "still in the viewer");
    assert!(fx.drv.find_text("Unsaved changes").is_some(), "the prompt");
    fx.drv.click_text(&mut fx.b, "Discard");
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.viewer.is_none(), "Discard leaves");
    assert_eq!(std::fs::read(&wav).unwrap(), original, "nothing written");
}

/// `at=OFFSET` without `open=`: the first text file opened by hand takes it; the next one opens
/// at its top.
#[test]
fn the_at_state_goes_to_the_first_text_file_opened_by_hand_only() {
    let mut fx = fixture();
    let body: String = (0..1000).map(|i| format!("row {i:04}\n")).collect();
    std::fs::write(format!("{}/rows.txt", fx.root), &body).unwrap();
    std::fs::write(format!("{}/again.txt", fx.root), &body).unwrap();
    fx.b.open_at = Some(600 * 9 + 5);
    // Listed again so the new files are there.
    fx.key(Key::Backspace);
    fx.settle();
    fx.key(Key::Enter);
    fx.settle();
    let top = |fx: &Fx| {
        let rows = fx.drv.texts_starting_with("row ");
        rows.iter()
            .min_by(|a, b| a.0.top().total_cmp(&b.0.top()))
            .map(|(_, t)| t.clone())
    };
    fx.open("rows.txt");
    assert_eq!(top(&fx).as_deref(), Some("row 0600"));
    fx.key(Key::Escape);
    fx.open("again.txt");
    assert_eq!(top(&fx).as_deref(), Some("row 0000"), "taken by the first");
}
