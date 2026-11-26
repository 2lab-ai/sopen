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
            drv.key(w, Key::W, Modifiers::COMMAND);
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

/// Once the session has ended (the input's end, the window kept), the window is one started
/// directly: no `[TUI]` on its title row, and F12 takes nothing back — there is no one to
/// return to; its close still asks.
#[test]
fn the_button_goes_with_the_session() {
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
            assert!(drv.find_text("[TUI]").is_some(), "the session's button");
            edit_bytes(drv, w);
            // Where the title row drew it, over the viewer's views too.
            let drawn = |drv: &Driver| chrome::mode_button_rect(&drv.ctx).is_some();
            assert!(drawn(drv), "the button over the viewer");
            input.end();
            drv.idle(w, 3);
            assert_eq!(said.text(), format!("{KEPT_CUE}\n"));
            assert!(!drawn(drv), "no button once kept");
            key(drv, w, Key::F12);
            drv.idle(w, 3);
            assert_eq!(drv.refused_closes, 0, "F12 asked nothing");
            assert!(drv.find_text("Unsaved changes").is_none(), "no prompt");
            assert!(!drv.window_closed);
            drv.key(w, Key::W, Modifiers::COMMAND);
            drv.idle(w, 2);
            drv.click_text_on_top(w, "Discard");
            drv.idle(w, 3);
            assert!(drv.window_closed, "closed there");
        },
    );
    ran.unwrap();
    assert_eq!(said.text(), format!("{KEPT_CUE}\n"));
}

/// The window in the wrappers `run_window` puts it in — the cells receipt's `Receipted` (no
/// receipt asked) and the `Shell` eframe runs — so a pass reaches it through each of their hops.
type Shelled = crate::gui::Shell<crate::gui::Receipted<Switched>>;

fn shelled(w: Switched) -> Shelled {
    crate::gui::Shell {
        inner: crate::gui::Receipted {
            inner: w,
            dump: None,
        },
        scripted: false,
        shot: None,
        frames: 0,
        started: std::time::Instant::now(),
        focus_asked: None,
        raise: crate::gui::Raise::Done,
        press: crate::gui::Press::Done,
        fkeys: None,
        closed: Arc::default(),
        web: None,
    }
}

/// One pass of a hidden window as eframe runs it (`update_logic_only`: the shell's `logic` alone,
/// in `run_logic`); whether it asked to close.
fn hidden_pass(drv: &Driver, shell: &mut Shelled) -> bool {
    let mut frame = eframe::Frame::_new_kittest();
    let out = drv.ctx.run_logic(&egui::RawInput::default(), |ctx| {
        eframe::App::logic(shell, ctx, &mut frame)
    });
    out.viewport_commands
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|c| c.contains(&egui::ViewportCommand::Close))
}

/// A run of HOST's half on `root` at `state` whose window is in the shell's wrappers ([`shelled`]),
/// driven by `drive`: what the run returned.
fn host_shelled(
    root: &str,
    state: &SwitchState,
    input: &Input,
    said: &Said,
    drive: impl FnOnce(&mut Driver, &mut Shelled),
) -> Result<Outcome> {
    let tmp = tempfile::tempdir().unwrap();
    run_on(
        start(tmp.path(), root, None),
        &Hooks::default(),
        sent(state),
        input.clone(),
        said.cues(),
        |_, w| {
            let mut shell = shelled(w);
            let mut drv = driver();
            drv.as_shell();
            drive(&mut drv, &mut shell);
            Ok(())
        },
    )
}

/// A hidden window is not drawn — eframe runs only the shell's `logic` then — yet it hears its
/// input's end there, through the shell and the receipt's wrapper: with nothing to lose it asks to
/// close, with unsaved edits it says [`KEPT_CUE`] and stays.
#[test]
fn a_hidden_window_hears_the_input_end() {
    let (_tmp, root) = root();
    let input = Input::default();
    let said = Said::default();
    let ran = host_shelled(
        &root,
        &at(&root, "notes.txt"),
        &input,
        &said,
        |drv, shell| {
            settle(drv, &mut shell.inner.inner);
            input.end();
            assert!(
                hidden_pass(drv, shell),
                "nothing to lose: it asks to close, undrawn"
            );
        },
    );
    ran.unwrap();
    assert_eq!(said.text(), "");
    assert!(said.gone.get(), "the session is let go");
    let input = Input::default();
    let said = Said::default();
    let ran = host_shelled(
        &root,
        &at(&root, "data.bin"),
        &input,
        &said,
        |drv, shell| {
            let w = &mut shell.inner.inner;
            settle(drv, w);
            edit_bytes(drv, w);
            input.end();
            assert!(!hidden_pass(drv, shell), "kept");
            assert_eq!(said.text(), format!("{KEPT_CUE}\n"), "said undrawn");
            assert!(said.gone.get(), "the session is let go");
        },
    );
    ran.unwrap();
    assert_eq!(said.text(), format!("{KEPT_CUE}\n"));
}

/// Set in a child of the cue tests below: which part of HOST's sopen it plays.
const CUE_CHILD: &str = "SOPEN_TEST_CUE_CHILD";

/// The state cue's line the "state" child says.
fn a_state_cue() -> String {
    format!("{STATE_CUE} {{\"v\":1}}")
}

/// The child's part, through the real relays ([`relay_stdio`]), cues ([`Cues::stdio`]), let go
/// ([`let_go_of_the_session`]) and end ([`end_of_run`]): "kept" says the kept cue and lets the
/// session go, as `keep()` does; "state" says a state cue and ends, as `run`'s caller does.
fn play_host(part: &str) {
    relay_stdio().expect("fds 1 and 2 relayed");
    let mut cues = Cues::stdio();
    match part {
        "kept" => {
            cues.say(KEPT_CUE);
            cues.let_go();
        }
        _ => cues.say(&a_state_cue()),
    }
    end_of_run();
}

/// This test binary's `test` run as a child playing HOST's `part`, its relays holding each write
/// `delay_ms` as a slow peer would; the other side reads its output to the end all along: what it
/// read, and how the child ended.
fn cue_child(test: &str, part: &str, delay_ms: u64) -> (String, std::process::ExitStatus) {
    use std::io::Read as _;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(CUE_CHILD, part)
        .env("SOPEN_TEST_RELAY_DELAY_MS", delay_ms.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::BufReader::new(stdout).read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let read = rx.recv_timeout(std::time::Duration::from_secs(120));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the child did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    (read.expect("its output ends"), status)
}

/// The kept cue reaches a session that is still read (the other side's input closed, its output
/// still read: 10 §7.3's "stays open: it has unsaved edits"), though the relay holds it a while
/// when the session is let go: the relay lets go only after passing on what it was given.
#[test]
fn the_kept_cue_reaches_the_session_before_it_is_let_go() {
    if let Ok(part) = std::env::var(CUE_CHILD) {
        return play_host(&part);
    }
    let (read, status) = cue_child(
        "gui::switch_host::tests::the_kept_cue_reaches_the_session_before_it_is_let_go",
        "kept",
        300,
    );
    assert!(status.success(), "the child ended {status:?}");
    // The test harness's `test … ... ` stands before it on its line.
    assert!(
        read.lines().any(|l| l.ends_with(KEPT_CUE)),
        "read: {read:?}"
    );
}

/// The state cue reaches a peer that takes 3 s to take it (a slow link): `end_of_run` waits for
/// the relays to pass on all they were given — up to [`CUE_WAIT`] — not 2 s. An `end_of_run` that
/// waits for nothing loses it too.
#[test]
fn the_state_cue_waits_for_a_slow_peer() {
    if let Ok(part) = std::env::var(CUE_CHILD) {
        return play_host(&part);
    }
    let (read, status) = cue_child(
        "gui::switch_host::tests::the_state_cue_waits_for_a_slow_peer",
        "state",
        3000,
    );
    assert!(status.success(), "the child ended {status:?}");
    let cue = a_state_cue();
    assert!(read.lines().any(|l| l.ends_with(&cue)), "read: {read:?}");
}

/// A relay (the session's output, `--state-stdin`) whose session has gone drains: what is
/// written into it — far more than a pipe holds — always goes through, what it is asked is
/// answered at once, and it ends at its input's end.
#[test]
fn a_relay_drains_once_its_session_is_gone() {
    let (gone, session) = std::io::pipe().unwrap();
    drop(gone);
    let (from, mut into) = std::io::pipe().unwrap();
    let (ended, relay_ended) = std::sync::mpsc::channel();
    let mut line = start_relay(1, from, session, ended).unwrap();
    let (wrote, written) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let all = (0..256).try_for_each(|_| into.write_all(&[b'x'; 8192]));
        let _ = wrote.send(all.map_err(|e| e.to_string()));
    });
    let all = written
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the writes never stall");
    assert_eq!(all, Ok(()), "every write went through");
    let answer = line.ask(Ask::Flush);
    let answered = answer.recv_timeout(std::time::Duration::from_secs(10));
    assert!(
        !matches!(answered, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "answered at once"
    );
    relay_ended
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the relay ends at its input's end");
}

/// A relay asked to let go passes on what it was given first — in order, all of it — then writes
/// nothing more and closes its stream to the session (its reader sees the end); one asked only
/// to flush goes on.
#[test]
fn a_relay_lets_go_only_after_passing_on_what_it_was_given() {
    use std::io::Read as _;
    let (mut peer, session) = std::io::pipe().unwrap();
    let (from, mut into) = std::io::pipe().unwrap();
    let (ended, _relay_ended) = std::sync::mpsc::channel();
    let mut line = start_relay(1, from, session, ended).unwrap();
    into.write_all(b"one\n").unwrap();
    line.ask(Ask::Flush)
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("flushed");
    into.write_all(b"two\n").unwrap();
    line.ask(Ask::LetGo)
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("let go");
    into.write_all(b"three\n").unwrap();
    let mut read = String::new();
    // The end comes from the relay's close: `into` is still open here.
    peer.read_to_string(&mut read).unwrap();
    assert_eq!(read, "one\ntwo\n");
}

/// Set in the child of [`a_dead_session_never_stops_what_still_writes`]: where its job lands.
const RELAY_CHILD: &str = "SOPEN_TEST_RELAY_CHILD";

/// The child's cue: the window has returned.
const RETURNED: &str = "sopen-test-returned";

/// Once the window has returned and its cue is out, the session can die while something still
/// writes — a file operation's report after the window (`Leftover::finish`), a viewer's
/// `waiting…`, a completion line in a pass. With fds 1 and 2 relayed ([`relay_stdio`]) none of
/// those writes fails: the job's own write lands and the run ends 0 ([`end_of_run`]). A child of
/// this test binary plays HOST's sopen with its own fds 1 and 2; the test is the other side, which
/// drops its ends of the session after the child's cue.
#[test]
fn a_dead_session_never_stops_what_still_writes() {
    use std::io::BufRead as _;
    if let Some(landed) = std::env::var_os(RELAY_CHILD) {
        return relay_child(Path::new(&landed));
    }
    let tmp = tempfile::tempdir().unwrap();
    let landed = tmp.path().join("landed");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "gui::switch_host::tests::a_dead_session_never_stops_what_still_writes",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(RELAY_CHILD, &landed)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (seen, cue) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = std::io::BufReader::new(stdout.unwrap()).lines();
        // The test harness's `test … ... ` stands before it on its line.
        let saw = lines.any(|l| l.is_ok_and(|l| l.ends_with(RETURNED)));
        // The other side's end of the output goes before it says it saw the cue.
        drop(lines);
        let _ = seen.send(saw);
    });
    let saw = cue.recv_timeout(std::time::Duration::from_secs(120));
    assert_eq!(saw, Ok(true), "the child's cue");
    drop(stderr);
    // The input ends: the child's job goes on writing into the dead session.
    drop(stdin);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the child did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(status.success(), "the child ended {status:?}");
    assert_eq!(
        std::fs::read_to_string(&landed).ok().as_deref(),
        Some("the job's write landed")
    );
}

/// The child's half: fds 1 and 2 relayed, the cue, then — once the other side is gone — a job
/// that says far more than a pipe holds on both, then lands its write.
fn relay_child(landed: &Path) {
    relay_stdio().expect("fds 1 and 2 relayed");
    println!("{RETURNED}");
    let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
    for n in 0..2000 {
        println!("a completion, line {n}");
        eprintln!("sopen: the file operation ended after the window closed: line {n}");
    }
    std::fs::write(landed, "the job's write landed").unwrap();
    end_of_run();
}

/// Set in the child of [`a_debug_line_on_a_dead_stderr_is_dropped`]: where it lands its write.
const DEBUG_CHILD: &str = "SOPEN_TEST_DEBUG_CHILD";

/// `SOPEN_LOG=debug`'s lines (`crate::debug!`) never fail on a standard error whose reader is
/// gone — the F-key watchdog's, which inherits the relay's pipe and outlives sopen (its retry
/// line, `fnkeys::watch`): the line is dropped and what comes after it runs. A child of this test
/// binary writes them once its standard error's reader is gone; it lands a file and ends 0.
#[test]
fn a_debug_line_on_a_dead_stderr_is_dropped() {
    if let Some(landed) = std::env::var_os(DEBUG_CHILD) {
        let _ = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
        for n in 0..2000 {
            crate::debug!("F-key watchdog: a retry, line {n}");
        }
        std::fs::write(landed, "after the debug lines").unwrap();
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let landed = tmp.path().join("landed");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "gui::switch_host::tests::a_debug_line_on_a_dead_stderr_is_dropped",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DEBUG_CHILD, &landed)
        .env("SOPEN_LOG", "debug")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Its standard error's reader goes first; then its input ends and it writes.
    drop(child.stderr.take());
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the child did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(status.success(), "the child ended {status:?}");
    assert_eq!(
        std::fs::read_to_string(&landed).ok().as_deref(),
        Some("after the debug lines")
    );
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
