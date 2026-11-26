//! HOST's half of the switch (10 §7.2–§7.3, X2-host): a state line opens the window at it — the
//! same cells as the browser it was taken from — and every way back (F12, `[TUI]`, the close
//! button, Cmd/Ctrl+Q and +W) hands the state back as the cue; unsaved edits ask first and Cancel
//! keeps the window; a newer `v` and another host's state are refused before any window; the
//! input's end closes a window with nothing to lose and keeps one with unsaved edits, after
//! [`KEPT_CUE`] and nothing more. The window is driven as the shell drives one ([`Driver`]).

use super::*;
use crate::gui::browser::switch_state::{Mode, PanelState, ViewerState};
use crate::gui::browser::Connect;
use crate::gui::chrome::{self, CELL};
use crate::gui::testkit::Driver;
use crate::gui::{cells, term};
use crate::remote::{CreateError, Entry, Like, LocalFs, RemoteFs, Stat, Uploaded};
use eframe::egui::{Event, Key, Modifiers, Pos2};
use std::cell::Cell;
use std::path::Path;

/// The receipts' grid: wide enough that no title row cuts a path (10 §15).
const COLS: u16 = 150;
const ROWS: u16 = 30;

/// This machine's files with no free bytes in the panels' foot: other writers move them between
/// two listings, and they are no part of the state.
struct NoFree(LocalFs);

impl RemoteFs for NoFree {
    fn stat_opt(&mut self, path: &str) -> anyhow::Result<Option<Stat>> {
        self.0.stat_opt(path)
    }
    fn lstat_opt(&mut self, path: &str) -> anyhow::Result<Option<Stat>> {
        self.0.lstat_opt(path)
    }
    fn realpath(&mut self, path: &str) -> anyhow::Result<String> {
        self.0.realpath(path)
    }
    fn read_head(&mut self, path: &str, max: usize) -> anyhow::Result<Vec<u8>> {
        self.0.read_head(path, max)
    }
    fn list_dir(&mut self, path: &str) -> anyhow::Result<Vec<String>> {
        self.0.list_dir(path)
    }
    fn list_entries(&mut self, path: &str) -> anyhow::Result<Vec<Entry>> {
        self.0.list_entries(path)
    }
    fn read_to(
        &mut self,
        path: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> anyhow::Result<u64> {
        self.0.read_to(path, out, progress)
    }
    fn create_from(
        &mut self,
        local: &Path,
        path: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.0.create_from(local, path, like)
    }
    fn set_mode(&mut self, path: &str, mode: u32) -> anyhow::Result<()> {
        self.0.set_mode(path, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.0.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        self.0.link_new(from, to)
    }
    fn remove(&mut self, path: &str) -> anyhow::Result<()> {
        self.0.remove(path)
    }
}

/// A browser's start on `root` over [`NoFree`], named `label` (`None`: this machine), with its
/// cache and state folders in `tmp`.
fn start(tmp: &Path, root: &str, label: Option<&str>) -> Start {
    let connect: Connect = Arc::new(|| Ok(Box::new(NoFree(LocalFs::new())) as Box<_>));
    Start {
        fs: Box::new(NoFree(LocalFs::new())),
        connect,
        label: label.map(str::to_string),
        destination: None,
        abs: root.to_string(),
        cache: tmp.join("cache"),
        state_dir: tmp.join("state"),
        launch: Arc::new(|abs: &str, _| Ok(format!("opened {abs}"))),
        git: None,
    }
}

/// The folder the tests open: folders, a long one, text, and bytes for the hex viewer.
fn root() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("root");
    std::fs::create_dir_all(root.join("alpha")).unwrap();
    std::fs::write(root.join("alpha/inner.txt"), "inside\n").unwrap();
    std::fs::create_dir(root.join("beta")).unwrap();
    std::fs::create_dir(root.join("many")).unwrap();
    for n in 0..60 {
        std::fs::write(root.join(format!("many/f{n:02}.txt")), "x\n".repeat(n)).unwrap();
    }
    std::fs::write(root.join("notes.txt"), "plain notes\n").unwrap();
    std::fs::write(root.join("data.bin"), (0..=255u8).collect::<Vec<u8>>()).unwrap();
    (tmp, root.to_str().unwrap().to_string())
}

/// A window of the receipts' grid.
fn driver() -> Driver {
    Driver::new(f32::from(COLS) * CELL.x, f32::from(ROWS) * CELL.y)
}

/// Passes until the window is ready, then two more.
fn settle<W: Window>(drv: &mut Driver, w: &mut W) {
    drv.until(w, 60, |w| w.ready());
    drv.idle(w, 2);
    drv.until(w, 60, |w| w.ready());
}

/// The `SOPEN_CELLS` receipt of one more pass, as the window writes it.
fn receipt<W: Window>(drv: &mut Driver, w: &mut W) -> String {
    let size = egui::vec2(f32::from(COLS) * CELL.x, f32::from(ROWS) * CELL.y);
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(10_000.0),
        max_texture_side: Some(8192),
        focused: true,
        ..Default::default()
    };
    let mut pass = None;
    let mut out = drv.ctx.run_ui(raw, |ui| {
        cells::clear(ui.ctx());
        w.ui(ui);
        pass = Some(term::keep_window_pass(ui.ctx()));
    });
    out.textures_delta.clear();
    term::window_receipt(&drv.ctx, &pass.expect("the pass ran"))
}

fn same_screen(taken: &str, given: &str) {
    let rows = |r: &str| {
        r.lines()
            .take(usize::from(ROWS) + 1)
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        taken == given,
        "the same cells\n--- taken\n{}\n--- given\n{}",
        rows(taken),
        rows(given)
    );
}

fn key<W: Window>(drv: &mut Driver, w: &mut W, key: Key) {
    drv.key(w, key, Modifiers::NONE);
}

fn typed<W: Window>(drv: &mut Driver, w: &mut W, text: &str) {
    drv.frame(w, vec![Event::Text(text.into())], Modifiers::NONE);
    drv.idle(w, 1);
}

/// What the cues said, and whether the session was let go.
#[derive(Clone, Default)]
struct Said {
    out: Rc<RefCell<Vec<u8>>>,
    gone: Rc<Cell<bool>>,
}

struct Out(Rc<RefCell<Vec<u8>>>);

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Said {
    fn cues(&self) -> Cues {
        let gone = self.gone.clone();
        Cues::new(
            Box::new(Out(self.out.clone())),
            Box::new(move || gone.set(true)),
        )
    }

    fn text(&self) -> String {
        String::from_utf8(self.out.borrow().clone()).unwrap()
    }

    /// The state the cue handed back: it is the one line said.
    fn handed(&self) -> SwitchState {
        let text = self.text();
        let line = text.strip_suffix('\n').expect("one line");
        assert!(!line.contains('\n'), "one line: {text}");
        let json = line
            .strip_prefix(&format!("{STATE_CUE} "))
            .unwrap_or_else(|| panic!("the state's cue: {text}"));
        switch_state::read(json, "HOST").expect("the cue's state reads")
    }
}

/// The state through its line, as the sender writes it on the input.
fn sent(state: &SwitchState) -> Sent {
    let line = format!("{}\n", state.line());
    read_state(&mut line.as_bytes()).unwrap_or_else(|e| panic!("{e}"))
}

/// Both sides on `root`, the bar on `bar` in the left one, which is active, in dual mode (no
/// preview: what each side shows is its list).
fn at(root: &str, bar: &str) -> SwitchState {
    let panel = |folder: &str, bar: Option<&str>| PanelState {
        folder: folder.to_string(),
        bar: bar.map(str::to_string),
        selected: Vec::new(),
        top: 0,
        tree: false,
        pin: None,
    };
    SwitchState {
        v: switch_state::VERSION,
        host: None,
        mode: Mode::Dual,
        active: 0,
        panels: [
            panel(root, Some(bar)),
            panel(&format!("{root}/alpha"), Some("inner.txt")),
        ],
        git_focus: false,
        dialog: None,
        viewer: None,
    }
}

/// A run of HOST's half on `root` (on the host `label`) at `state`, its input `input`, its cues
/// on `said`, its window driven by `drive` as the shell drives one: what the run returned.
fn host(
    root: &str,
    label: Option<&str>,
    state: &SwitchState,
    input: &Input,
    said: &Said,
    drive: impl FnOnce(&mut Driver, &mut Switched),
) -> Result<Outcome> {
    let tmp = tempfile::tempdir().unwrap();
    let start = start(tmp.path(), root, label);
    run_on(
        start,
        &Hooks::default(),
        sent(state),
        input.clone(),
        said.cues(),
        |_, mut w| {
            let mut drv = driver();
            drv.as_shell();
            drive(&mut drv, &mut w);
            Ok(())
        },
    )
}

/// The hex viewer on `data.bin` (the bar is on it), a byte typed over: an unsaved edit.
fn edit_bytes(drv: &mut Driver, w: &mut Switched) {
    key(drv, w, Key::Enter);
    settle(drv, w);
    // F8 puts the typing in the ASCII column, where Z is a byte once Enter has started editing
    // (08 D35).
    key(drv, w, Key::F8);
    key(drv, w, Key::Enter);
    typed(drv, w, "Z");
    assert!(
        drv.text_starting_with("5A 01 02").is_some(),
        "the byte is typed over"
    );
}

/// The sender's browser, moved by the owner's keys: dual mode, the bar on a folder in the left
/// side, the right side in a long folder, scrolled, the bar deep in it. Its state through its
/// line opens HOST's window at the same cells, `[TUI]` on its title row (the receipt leaves the
/// mode button out, 10 §15); F12 there hands it back unchanged, as the cue.
#[test]
fn a_state_line_opens_the_window_at_it_and_f12_hands_it_back() {
    let (_tmp, root) = root();
    let tmp = tempfile::tempdir().unwrap();
    let mut a = Browser::new(start(tmp.path(), &root, None));
    let mut drv = driver();
    settle(&mut drv, &mut a);
    for k in [Key::F9, Key::ArrowDown, Key::ArrowDown, Key::Tab] {
        key(&mut drv, &mut a, k);
    }
    for _ in 0..3 {
        key(&mut drv, &mut a, Key::ArrowDown);
    }
    key(&mut drv, &mut a, Key::Enter);
    settle(&mut drv, &mut a);
    for _ in 0..40 {
        key(&mut drv, &mut a, Key::ArrowDown);
    }
    settle(&mut drv, &mut a);
    let state = a.take_state(&drv.ctx);
    assert_eq!(state.panels[1].folder, format!("{root}/many"), "{state:?}");
    assert!(state.panels[1].top > 0, "scrolled: {state:?}");
    let want = receipt(&mut drv, &mut a);
    let said = Said::default();
    let ran = host(&root, None, &state, &Input::default(), &said, |drv, w| {
        settle(drv, w);
        assert!(
            drv.find_text("[TUI]").is_some(),
            "the switched window's button"
        );
        same_screen(&want, &receipt(drv, w));
        assert_eq!(said.text(), "", "nothing handed back while it is open");
        key(drv, w, Key::F12);
        drv.idle(w, 2);
        assert!(drv.window_closed, "F12 returns");
    });
    ran.unwrap();
    assert_eq!(said.text(), format!("{STATE_CUE} {}\n", state.line()));
    assert!(!said.gone.get(), "the session is the other side's to end");
}

/// A way back from the window, as the owner takes it.
type Back = fn(&mut Driver, &mut Switched);

/// Every way back hands the state back: the title row's `[TUI]`, the close button, Cmd+Q and
/// Cmd+W (the shell's close: `closing()`), F12 from a text viewer — the viewer is in the state,
/// at its file.
#[test]
fn every_way_back_hands_the_state_back() {
    let (_tmp, root) = root();
    let state = at(&root, "notes.txt");
    let routes: [(&str, Back); 4] = [
        ("[TUI]", |drv, w| {
            let at = chrome::mode_button_rect(&drv.ctx).expect("the button is drawn");
            drv.click(w, at.center());
        }),
        ("the close button", |drv, w| {
            drv.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            drv.idle(w, 1);
        }),
        ("Cmd+Q", |drv, w| drv.key(w, Key::Q, Modifiers::COMMAND)),
        ("Cmd+W", |drv, w| drv.key(w, Key::W, Modifiers::COMMAND)),
    ];
    for (route, back) in routes {
        let said = Said::default();
        let ran = host(&root, None, &state, &Input::default(), &said, |drv, w| {
            settle(drv, w);
            back(drv, w);
            drv.idle(w, 2);
            assert!(drv.window_closed, "{route} returns");
        });
        ran.unwrap();
        assert_eq!(
            said.text(),
            format!("{STATE_CUE} {}\n", state.line()),
            "{route}"
        );
    }
    let mut viewing = at(&root, "notes.txt");
    viewing.viewer = Some(ViewerState {
        file: format!("{root}/notes.txt"),
        view: 1,
        place: None,
        encoding: None,
        wrap: None,
        revision: false,
    });
    let said = Said::default();
    let ran = host(&root, None, &viewing, &Input::default(), &said, |drv, w| {
        settle(drv, w);
        assert!(drv.find_text("plain notes").is_some(), "the viewer is open");
        key(drv, w, Key::F12);
        drv.idle(w, 2);
        assert!(drv.window_closed, "F12 in the viewer returns");
    });
    ran.unwrap();
    let back = said.handed();
    let viewer = back.viewer.expect("the viewer is in the state");
    assert_eq!(viewer.file, format!("{root}/notes.txt"));
}

/// A played F12 (`SOPEN_KEYS`, which the browser plays after the shell's hook took the
/// keyboard's) is the switch, as the owner's own; one with a modifier is not, and both are
/// taken out of the pass.
#[test]
fn a_played_f12_is_the_switch() {
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let pass = |modifiers: Modifiers| {
        let mut asked = None;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.ctx().input_mut(|i| {
                for pressed in [true, false] {
                    i.events.push(Event::Key {
                        key: Key::F12,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
            });
            asked = Some(played_switch(ui.ctx()));
            let left = ui.ctx().input(|i| i.events.len());
            assert_eq!(left, 0, "taken out of the pass");
        });
        out.textures_delta.clear();
        asked.expect("the pass ran")
    };
    assert!(pass(Modifiers::NONE));
    assert!(!pass(Modifiers::SHIFT));
}

/// Unsaved edits in the window: F12 asks the viewer's close prompt first, and its Cancel keeps
/// the window, nothing said; the next F12's Discard returns, with the state.
#[test]
fn unsaved_edits_ask_first_and_cancel_keeps_the_window() {
    let (_tmp, root) = root();
    let state = at(&root, "data.bin");
    let said = Said::default();
    let ran = host(&root, None, &state, &Input::default(), &said, |drv, w| {
        settle(drv, w);
        edit_bytes(drv, w);
        key(drv, w, Key::F12);
        drv.idle(w, 2);
        assert!(
            drv.find_text("Unsaved changes").is_some(),
            "the prompt first"
        );
        assert!(!drv.window_closed);
        drv.click_text_on_top(w, "Cancel");
        drv.idle(w, 2);
        assert!(drv.find_text("Unsaved changes").is_none());
        assert!(!drv.window_closed, "Cancel keeps the window");
        assert_eq!(said.text(), "", "nothing said");
        key(drv, w, Key::F12);
        drv.idle(w, 2);
        drv.click_text_on_top(w, "Discard");
        drv.idle(w, 3);
        assert!(drv.window_closed, "Discard returns");
    });
    ran.unwrap();
    let back = said.handed();
    assert_eq!(back.panels[0].bar.as_deref(), Some("data.bin"), "{back:?}");
    assert_eq!(std::fs::read(format!("{root}/data.bin")).unwrap()[0], 0);
}

/// Refused before any window (10 §7.3: exit 2): a newer `v` with §7.4's line, no line at all, a
/// line that does not read; and a state of another host than the window's, while one of its own
/// host opens (the folder `box:/…` the other side sends for one on its own machine).
#[test]
fn a_state_it_cannot_take_is_refused_before_any_window() {
    let (_tmp, root) = root();
    let refused = |line: &str| read_state(&mut line.as_bytes()).err();
    assert_eq!(
        refused(&format!(
            "{{\"v\":2,\"panels\":[{{\"folder\":\"{root}\"}},{{\"folder\":\"{root}\"}}]}}\n"
        ))
        .as_deref(),
        Some("the sender sent switch state v2; this sopen reads up to v1 — update one of them")
    );
    assert_eq!(
        refused("").as_deref(),
        Some("the input ended before the switch's state")
    );
    let garbled = refused("{\"v\":1,\n").expect("refused");
    assert!(
        garbled.starts_with("the sender sent a switch state that does not read: "),
        "{garbled}"
    );
    // Another host's: the window never shows.
    let mut boxed = at(&root, "notes.txt");
    boxed.host = Some("box".to_string());
    let tmp = tempfile::tempdir().unwrap();
    let ran = run_on(
        start(tmp.path(), &root, None),
        &Hooks::default(),
        sent(&boxed),
        Input::default(),
        Said::default().cues(),
        |_, _| panic!("no window for another host's state"),
    );
    let e = ran.expect_err("refused");
    let not = e.downcast_ref::<NotGiven>().expect("a usage error");
    assert_eq!(
        not.0,
        "the switch's state is of box, this window of this machine"
    );
    // Of the window's host: it opens, and hands back a state of that host.
    let said = Said::default();
    let ran = host(
        &root,
        Some("box"),
        &boxed,
        &Input::default(),
        &said,
        |drv, w| {
            settle(drv, w);
            key(drv, w, Key::F12);
            drv.idle(w, 2);
            assert!(drv.window_closed);
        },
    );
    ran.unwrap();
    assert_eq!(said.handed(), boxed);
    // A state of this machine for a window of `box`: refused.
    let tmp = tempfile::tempdir().unwrap();
    let ran = run_on(
        start(tmp.path(), &root, Some("box")),
        &Hooks::default(),
        sent(&at(&root, "notes.txt")),
        Input::default(),
        Said::default().cues(),
        |_, _| panic!("no window for another host's state"),
    );
    assert!(ran.unwrap_err().downcast_ref::<NotGiven>().is_some());
}

/// The input's end (the other side's sopen gone) with nothing to lose: the window closes, the
/// session let go, no cue.
#[test]
fn the_input_ending_with_nothing_to_lose_closes_the_window() {
    let (_tmp, root) = root();
    let input = Input::default();
    let said = Said::default();
    let ran = host(
        &root,
        None,
        &at(&root, "notes.txt"),
        &input,
        &said,
        |drv, w| {
            settle(drv, w);
            input.end();
            drv.idle(w, 3);
            assert!(drv.window_closed, "nothing to lose: closed");
        },
    );
    ran.unwrap();
    assert_eq!(said.text(), "");
    assert!(said.gone.get(), "the session is let go");
}

/// The input's end with unsaved edits: [`KEPT_CUE`], the session let go, the window kept — its
/// own close prompt still asks (Cancel keeps it) — and nothing said after, not even when it is
/// closed there.
#[test]
fn the_input_ending_with_unsaved_edits_keeps_the_window() {
    let (_tmp, root) = root();
    let input = Input::default();
    let said = Said::default();
    let ran = host(
        &root,
        None,
        &at(&root, "data.bin"),
        &input,
        &said,
        |drv, w| {
            settle(drv, w);
            edit_bytes(drv, w);
            input.end();
            drv.idle(w, 3);
            assert!(!drv.window_closed, "kept");
            assert_eq!(said.text(), format!("{KEPT_CUE}\n"));
            assert!(said.gone.get(), "the session is let go");
            // Closed there: its prompt; Cancel keeps it; Discard closes it.
            drv.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            drv.idle(w, 4);
            assert_eq!(drv.refused_closes, 1, "asked");
            assert!(
                drv.find_text("Unsaved changes").is_some(),
                "the prompt: {:?}",
                drv.texts().into_iter().map(|t| t.2).collect::<Vec<_>>()
            );
            drv.click_text_on_top(w, "Cancel");
            drv.idle(w, 2);
            assert!(!drv.window_closed, "Cancel keeps it");
            key(drv, w, Key::F12);
            drv.idle(w, 2);
            drv.click_text_on_top(w, "Discard");
            drv.idle(w, 3);
            assert!(drv.window_closed, "closed there");
        },
    );
    ran.unwrap();
    assert_eq!(
        said.text(),
        format!("{KEPT_CUE}\n"),
        "nothing after the cue"
    );
    assert!(said.gone.get(), "the session is let go");
}

/// A run a hook drives (`SOPEN_CELLS`): the shell lets its close through unasked (`closing()`
/// asks a scripted window nothing), so it hands back the state of its last pass; the input's end
/// is its hook's to end — the window neither closes nor is kept for it.
#[test]
fn a_hook_driven_run_hands_back_its_last_pass() {
    let (_tmp, root) = root();
    let tmp = tempfile::tempdir().unwrap();
    let hooks = Hooks {
        cells: Some(tmp.path().join("cells.txt")),
        ..Hooks::default()
    };
    let (said, input) = (Said::default(), Input::default());
    let ran = run_on(
        start(tmp.path(), &root, None),
        &hooks,
        sent(&at(&root, "notes.txt")),
        input.clone(),
        said.cues(),
        |_, mut w| {
            // Not as the shell: a scripted window is never asked about a close.
            let mut drv = driver();
            settle(&mut drv, &mut w);
            key(&mut drv, &mut w, Key::ArrowUp);
            input.end();
            drv.idle(&mut w, 3);
            assert!(!drv.closed(), "the hook ends the run, not the input");
            assert_eq!(said.text(), "", "nothing kept");
            Ok(())
        },
    );
    ran.unwrap();
    let back = said.handed();
    assert_eq!(back.panels[0].bar.as_deref(), Some("data.bin"), "{back:?}");
    assert!(!said.gone.get());
}
