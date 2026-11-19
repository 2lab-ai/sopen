//! The file browser driven by real egui key and pointer events through the test kit: the list
//! and its keys, a viewer in the same window and every way out of it through the viewer's own
//! `close_requested`, F4 through the real editor launch, a fetch stopped by Esc.

use super::*;
use crate::gui::testkit::Driver;
use crate::remote::{LocalFs, Stat};
use eframe::egui::{Event, PointerButton};
use std::io::Write;
use std::path::Path;

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
    #[allow(dead_code)]
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
        abs: root.clone(),
        cache: cache.clone(),
        launch,
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

    /// `path` as a panel's frame shows it (a 1000-point window: two 496-point panels): whole,
    /// or its end after "…".
    fn framed(&self, path: &str) -> String {
        let font = egui::FontId::monospace(13.0);
        chrome::fit_left(&self.drv.ctx, path, &font, 496.0 - 24.0)
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
    assert!(footer.starts_with("8 listed = "), "{footer}");
    assert!(footer.contains("bytes free"), "{footer}");
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
    // Closing the window with an edit, then Discard: the window closes.
    fx.key(Key::Enter);
    fx.settle();
    edit_image(&mut fx);
    assert!(!Window::close_requested(&mut fx.b));
    fx.drv.idle(&mut fx.b, 1);
    fx.drv.click_text(&mut fx.b, "Discard");
    assert!(fx.b.viewer.is_none());
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
    fx.key(Key::Tab);
    fx.typed("n");
    fx.key(Key::ArrowDown);
    fx.key(Key::Escape);
    // The byte typed is an edit: the hex window asks.
    assert!(fx.drv.find_text("Unsaved changes").is_some());
    fx.drv.click_text(&mut fx.b, "Discard");
    assert!(fx.b.viewer.is_none());
    assert!(!fx.drv.closed());
    assert_eq!((fx.b.active, fx.current().as_str()), (0, "data.bin"));
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

/// A folder "on host box": files are fetched into the cache as the standalone routes fetch
/// them, and the copy is released with the viewer — opening it again uses the same copy, not a
/// session copy of its own.
#[test]
fn a_viewer_holds_its_cached_copy_only_while_it_shows_it() {
    let mut fx = fixture_with(Some("box"), local_connect(), None);
    fx.open("notes.txt");
    let copy = crate::cache::cache_path(&fx.cache, "box", &format!("{}/notes.txt", fx.root));
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "plain notes\n");
    assert!(fx.drv.find_text("plain notes").is_some());
    fx.key(Key::Escape);
    fx.open("notes.txt");
    assert!(
        !fx.cache.join(".sessions").exists(),
        "the second open got a session copy: the first one's claim was still held"
    );
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
}

/// The local disk posing as a slow remote: a fetch takes a while.
struct Slow(LocalFs);

impl RemoteFs for Slow {
    fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.0.stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.0.lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.0.realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.0.read_head(p, max)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.0.list_dir(p)
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        let bytes = std::fs::read(p)?;
        let mut done = 0u64;
        for chunk in bytes.chunks(64 * 1024) {
            std::thread::sleep(std::time::Duration::from_millis(30));
            out.write_all(chunk)?;
            done += chunk.len() as u64;
            progress(done);
        }
        Ok(done)
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<crate::remote::Like>,
    ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
        self.0.create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.0.set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.0.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.0.link_new(from, to)
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.0.remove(p)
    }
}

/// Esc during a fetch stops it: back to the list, nothing of it kept in the cache; the progress
/// was on the status row meanwhile.
#[test]
fn esc_stops_a_fetch() {
    let slow: Connect = Arc::new(|| Ok(Box::new(Slow(LocalFs::new())) as Box<dyn RemoteFs + Send>));
    let mut fx = fixture_with(Some("box"), slow, None);
    std::fs::write(format!("{}/big.bin", fx.root), vec![1u8; 2 << 20]).unwrap();
    // Listed again (Enter on `..`, then back) so the new file shows.
    fx.key(Key::Enter);
    fx.settle();
    fx.select("root");
    fx.key(Key::Enter);
    fx.settle();
    fx.select("big.bin");
    fx.key(Key::Enter);
    fx.drv.until(
        &mut fx.b,
        10,
        |b| matches!(&b.task, Some(Task::Opening { done, .. }) if done.load(Ordering::Relaxed) > 0),
    );
    let shown = fx.b.status_line().text;
    assert!(
        shown.starts_with("fetching big.bin ") && shown.ends_with("Esc cancels"),
        "{shown}"
    );
    fx.key(Key::Escape);
    fx.drv.until(&mut fx.b, 10, |b| b.task.is_none());
    assert!(fx.b.viewer.is_none());
    assert!(
        fx.b.status.text.starts_with("cancelled"),
        "{}",
        fx.b.status.text
    );
    let copy = crate::cache::cache_path(&fx.cache, "box", &format!("{}/big.bin", fx.root));
    let left: Vec<String> = std::fs::read_dir(copy.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("big.bin"))
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
        lines.last().unwrap().starts_with("8 listed = "),
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
    let slow: Connect = Arc::new(|| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)
    });
    let mut fx = fixture_with(None, slow, None);
    fx.select("main.rs");
    fx.key(Key::Enter);
    assert!(matches!(fx.b.task, Some(Task::Opening { .. })));
    fx.key(Key::Escape);
    fx.drv.until(&mut fx.b, 10, |b| b.task.is_none());
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
        abs: root.to_str().unwrap().to_string(),
        cache: tmp.path().join("cache"),
        launch: Arc::new(|_: &str, _: bool| Ok(String::new())),
    });
    b.open_first = Some("main.rs".to_string());
    let mut drv = Driver::new(1000.0, 700.0);
    drv.until(&mut b, 20, |b| b.ready() && b.viewer.is_some());
    assert!(drv.find_text("fn main() {").is_some());
    assert_eq!(b.panels[0].current().unwrap().name, "main.rs");
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
    let ok = std::process::Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=25:duration=1",
        ])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(format!("{}/clip.mp4", fx.root))
        .status()
        .unwrap()
        .success();
    assert!(ok, "fixture");
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.open("clip.mp4");
    assert!(fx.drv.find_text("Exit   ").is_some(), "the viewer's F-keys");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "clip.mp4");
    assert!(!fx.drv.closed());
}

/// Cmd/Ctrl+C, X and V reach a hosted image as in its own window (the browser takes none of
/// them): Copy puts the image on the clipboard, and Paste then Esc asks before the edit goes.
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
    // A picture pasted without a rectangle selects what it covers (the first Esc clears that)
    // and is an edit: the second Esc asks before it goes.
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
    assert!(!fx.drv.closed());
}
