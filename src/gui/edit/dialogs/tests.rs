//! ED-dialogs (12 §16): the editor's dialogs in cells, through its view — the close prompt at
//! each exit of §6.4 (ES-5), the conflict dialog (§9.2, ES-2's dialog), Ctrl/⌘+R's question and
//! the encoding question (§7.2), each on Cancel under E6; F10's menus; Go to and Find / Replace
//! on the status row; one prompt over a Hangul name, the same cells in a terminal.

use super::*;
use crate::edit::arm::Press;
use crate::edit::buffer::Original;
use crate::edit::file::{read_local, Session};
use crate::edit::ops::{Doubt, Editor};
use crate::encoding::Encoding;
use crate::gui::browser::shown_wrapped;
use crate::gui::cells;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::Driver;
use crate::journal::{self, Keep};
use crate::remote::LocalFs;
use eframe::egui::{Color32, Event, Key, Modifiers};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn view_on(text: &str, name: &str, backend: Backend, doubt: Doubt) -> EditView {
    let ed = Editor::open(
        Original::new(text.as_bytes().to_vec(), Encoding::Utf8),
        doubt,
    );
    EditView::new(ed, name, backend, doubt.guessed)
}

fn view(text: &str) -> EditView {
    view_on(text, "notes.txt", Backend::Window, Doubt::default())
}

fn doc(v: &EditView) -> Vec<u8> {
    let mut out = Vec::new();
    v.editor().buffer().write_to(&mut out).unwrap();
    out
}

fn ctrl(key: Key) -> Press {
    Press::key(key, Modifiers::COMMAND)
}

fn file(running: bool) -> FileState {
    FileState {
        shown: "notes.txt".to_string(),
        can_replace: true,
        journal: None,
        running,
    }
}

/// Whether the pass's cells show `text` on one row.
fn shows(drv: &Driver, text: &str) -> bool {
    !cells::find(&cells::layers(&drv.ctx), text).is_empty()
}

/// The colours of the first cell that shows `text` (its last place: a box is over the rest).
fn pen_of(drv: &Driver, text: &str) -> Option<(Option<Color32>, Option<Color32>)> {
    let layers = cells::layers(&drv.ctx);
    let hit = *cells::find(&layers, text).last()?;
    let l = &layers[hit.layer];
    let row = &l.seen()[usize::from(hit.cells.y - l.buf.area.y)];
    let first = &row[usize::from(hit.cells.x - l.buf.area.x)];
    Some((first.fg, first.bg))
}

/// Whether `label` is in the bar's colours: the control with the keyboard.
fn focused(drv: &Driver, label: &str) -> bool {
    pen_of(drv, label) == Some((Some(PALETTE.bar_text), Some(PALETTE.bar)))
}

/// Where cell (`x`, `y`) of layer `l` is on the screen's grid (a box is laid on a grid of its
/// own).
fn on_screen(l: &cells::Layer, x: u16, y: u16) -> (u16, u16) {
    let cell = |p: f32, size: f32| (p / size).round() as u16;
    let o = l.origin;
    (x + cell(o.x, CELL.x), y + cell(o.y, CELL.y))
}

/// The status row's text (the last row of the editor's grid).
fn status_row(drv: &Driver) -> String {
    let l = (cells::layers(&drv.ctx).into_iter())
        .max_by_key(|l| l.buf.area.height)
        .expect("the editor's cells");
    cells::row_text(&l.buf, l.buf.area.height - 1).0
}

const LETTERS: [Key; 26] = [
    Key::A,
    Key::B,
    Key::C,
    Key::D,
    Key::E,
    Key::F,
    Key::G,
    Key::H,
    Key::I,
    Key::J,
    Key::K,
    Key::L,
    Key::M,
    Key::N,
    Key::O,
    Key::P,
    Key::Q,
    Key::R,
    Key::S,
    Key::T,
    Key::U,
    Key::V,
    Key::W,
    Key::X,
    Key::Y,
    Key::Z,
];

/// Every letter as egui-winit sends it (the key with its physical key, then its text), and
/// 2-set jamo as a terminal sends them (text alone).
fn every_letter(drv: &mut Driver, v: &mut EditView) {
    for key in LETTERS {
        drv.press_typed(v, key, &key.name().to_lowercase());
    }
    for jamo in ["ㄴ", "ㅊ", "ㅛ", "ㅜ", "ㅁ", "ㄹ"] {
        drv.frame(v, vec![Event::Text(jamo.into())], Modifiers::NONE);
    }
}

/// Keeps until the journal says kept (its original is written on a worker).
fn kept(s: &mut Session, ed: &Editor) {
    let t0 = Instant::now();
    loop {
        match s.keep(ed) {
            Keep::Kept => return,
            Keep::Keeping if t0.elapsed() < Duration::from_secs(30) => {
                std::thread::sleep(Duration::from_millis(2))
            }
            other => panic!("{other:?}"),
        }
    }
}

/// `notes.txt` holding `body` in a folder of its own, a cache beside it, read for the editor:
/// its session and the view on it.
struct Rig {
    _tmp: tempfile::TempDir,
    target: std::path::PathBuf,
    session: Session,
    view: EditView,
}

fn rig(body: &[u8]) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    let (files, cache) = (dir.join("files"), dir.join("cache"));
    std::fs::create_dir_all(&files).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    let target = files.join("notes.txt");
    std::fs::write(&target, body).unwrap();
    let never = AtomicBool::new(false);
    let rd = read_local(
        target.to_str().unwrap(),
        Encoding::Utf8,
        &mut |_, _| {},
        &never,
    )
    .unwrap();
    let session = Session::new(&rd, None, &cache, journal::disk());
    let ed = Editor::open(rd.original.clone(), Doubt::of(&rd.original, false));
    let view = EditView::new(ed, "notes.txt", Backend::Window, false);
    Rig {
        _tmp: tmp,
        target,
        session,
        view,
    }
}

impl Rig {
    /// What the binding tells the view of the session.
    fn told(&mut self, running: bool) {
        let s = &self.session;
        self.view.set_file(FileState {
            shown: s.show(s.target()),
            can_replace: true,
            journal: s.journal_dir().map(Path::to_path_buf),
            running,
        });
    }
}

// ---------------------------------------------------------------------------------------------
// ES-5: the close prompt.

/// ES-5, E6: at each exit of §6.4 a clean editor goes at once; with unsaved edits the close
/// prompt asks 07 §10's four choices — Ctrl/⌘+R's worded for reading again — with the keyboard
/// on Cancel, and Enter presses it: still editing, the edit there, nothing chosen.
#[test]
fn es5_the_close_prompt_at_each_exit_opens_on_cancel() {
    let exits = [
        Exit::Leave,
        Exit::View,
        Exit::External,
        Exit::Views(2),
        Exit::Open("other.txt".into()),
        Exit::Reread,
        Exit::Close,
        Exit::Switch,
    ];
    for exit in exits {
        let mut v = view("abc\n");
        let mut drv = Driver::new(640.0, 400.0);
        assert!(
            v.leave(exit.clone()),
            "{exit:?}: a clean editor goes at once"
        );
        assert!(!v.asking(), "{exit:?}");
        v.press(&Press::Text("x".into()));
        assert!(!v.leave(exit.clone()), "{exit:?}: the edit holds it");
        assert!(v.asking(), "{exit:?}");
        drv.idle(&mut v, 1);
        let words = match exit {
            Exit::Reread => "before reading it again?",
            _ => "before closing?",
        };
        let question = format!("Save the changes to notes.txt {words}");
        assert!(shown_wrapped(&drv, &question), "{exit:?}: {question}");
        assert!(shows(&drv, " Unsaved changes "), "{exit:?}: its title");
        let buttons = "[ Save ]  [ Save As… ]  [ Discard ]  [ Cancel ]";
        assert!(shows(&drv, buttons), "{exit:?}");
        assert!(
            focused(&drv, "[ Cancel ]"),
            "{exit:?}: the keyboard on Cancel"
        );
        drv.key(&mut v, Key::Enter, Modifiers::NONE);
        assert!(!v.asking(), "{exit:?}: Enter pressed Cancel");
        assert_eq!(v.take_chosen(), vec![], "{exit:?}");
        assert_eq!(v.take_commands(), vec![], "{exit:?}");
        assert_eq!(doc(&v), b"xabc\n", "{exit:?}: still editing");
    }
}

/// ES-5, E6: the close prompt, Ctrl/⌘+R's question, the conflict dialog and the encoding
/// question each open on Cancel, and no letter answers them — by key, typed text or jamo, nor
/// through the editor's keys; Enter then presses Cancel.
#[test]
fn es5_letters_answer_nothing_in_the_four_prompts() {
    let conflict = Saved::Conflict {
        line: "notes.txt changed since it was read (size 4 → 6)".into(),
        overwrite: true,
        staged: PathBuf::from("/nowhere/notes.txt"),
        notes: vec![],
    };
    let guessed = Doubt {
        guessed: true,
        undecodable: 0,
    };
    for prompt in ["close", "Ctrl+R", "conflict", "encoding"] {
        let mut v = view_on("abc\n", "notes.txt", Backend::Window, guessed);
        let mut drv = Driver::new(640.0, 400.0);
        match prompt {
            "close" => {
                v.press(&Press::Text("x".into()));
                assert!(!v.leave(Exit::Close));
            }
            "Ctrl+R" => {
                v.press(&Press::Text("x".into()));
                assert_eq!(v.press(&ctrl(Key::R)), Command::Reread);
                assert!(!v.leave(Exit::Reread));
            }
            "conflict" => {
                v.press(&Press::Text("x".into()));
                v.landed(&conflict);
            }
            _ => {
                v.press(&Press::Text("한".into()));
            }
        }
        let before = doc(&v);
        drv.idle(&mut v, 1);
        assert!(v.asking(), "{prompt}");
        assert!(focused(&drv, "[ Cancel ]"), "{prompt}: on Cancel");
        every_letter(&mut drv, &mut v);
        assert!(v.asking(), "{prompt}: a letter answered it");
        assert!(focused(&drv, "[ Cancel ]"), "{prompt}: still on Cancel");
        for press in [
            Press::Text("s".into()),
            Press::Text("ㄴ".into()),
            Press::key(Key::F2, Modifiers::NONE),
            Press::key(Key::Enter, Modifiers::NONE),
        ] {
            assert_eq!(v.press(&press), Command::Nothing, "{prompt}: {press:?}");
        }
        assert_eq!(v.take_chosen(), vec![], "{prompt}");
        assert_eq!(v.take_commands(), vec![], "{prompt}");
        assert_eq!(doc(&v), before, "{prompt}");
        drv.key(&mut v, Key::Enter, Modifiers::NONE);
        assert!(!v.asking(), "{prompt}: Enter pressed Cancel");
        assert_eq!(v.take_chosen(), vec![], "{prompt}");
        assert_eq!(v.take_commands(), vec![], "{prompt}");
        assert_eq!(
            doc(&v),
            before,
            "{prompt}: nothing typed, saved or discarded"
        );
    }
}

/// E6: ← → and Tab / Shift+Tab move the keyboard between the buttons (the bar's colours on
/// the one that has it), Enter presses that one, and Esc is Cancel.
#[test]
fn the_arrows_and_tab_move_the_focus_and_esc_cancels() {
    let mut v = view("abc\n");
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&Press::Text("x".into()));
    assert!(!v.leave(Exit::Close));
    drv.idle(&mut v, 1);
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    assert!(!v.asking(), "Esc is Cancel");
    assert_eq!(v.take_chosen(), vec![]);
    assert!(!v.leave(Exit::Close));
    drv.idle(&mut v, 1);
    let steps = [
        (Key::ArrowLeft, Modifiers::NONE, "[ Discard ]"),
        (Key::ArrowLeft, Modifiers::NONE, "[ Save As… ]"),
        (Key::ArrowRight, Modifiers::NONE, "[ Discard ]"),
        (Key::Tab, Modifiers::NONE, "[ Cancel ]"),
        (Key::Tab, Modifiers::SHIFT, "[ Discard ]"),
    ];
    for (key, mods, on) in steps {
        drv.key(&mut v, key, mods);
        assert!(focused(&drv, on), "{key:?} {mods:?}: {on}");
    }
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(v.take_chosen(), vec![Chosen::Discard(Exit::Close)]);
    assert!(!v.asking());
}

/// ES-5: the prompt's Save is the binding's Save; once it lands, the exit goes. Discard names
/// the kept edits it deletes, and the journal goes with it.
#[test]
fn es5_a_save_that_lands_closes_and_discard_deletes_the_journal() {
    let mut r = rig(b"line one\n");
    let mut drv = Driver::new(640.0, 400.0);
    r.view.press(&Press::Text("x".into()));
    kept(&mut r.session, r.view.editor());
    r.told(false);
    let journal = r.session.journal_dir().unwrap().to_path_buf();
    assert!(!r.view.leave(Exit::Close));
    drv.idle(&mut r.view, 1);
    let deletes = format!(
        "Discard deletes the kept edits at {}",
        crate::gui::binary::tilde(&journal)
    );
    assert!(shown_wrapped(&drv, &deletes), "{deletes}");
    for _ in 0..3 {
        drv.key(&mut r.view, Key::Tab, Modifiers::SHIFT);
    }
    assert!(focused(&drv, "[ Save ]"));
    drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
    assert_eq!(r.view.take_commands(), vec![Command::Save]);
    assert_eq!(r.view.take_chosen(), vec![], "not before it lands");
    let saved = (r.session)
        .save(r.view.editor_mut(), &mut LocalFs::new())
        .unwrap();
    r.view.landed(&saved);
    assert_eq!(r.view.take_chosen(), vec![Chosen::Go(Exit::Close)]);
    assert_eq!(std::fs::read(&r.target).unwrap(), b"xline one\n");
    // Discard: the journal of the edits since goes.
    r.view.press(&Press::Text("y".into()));
    kept(&mut r.session, r.view.editor());
    r.told(false);
    let journal = r.session.journal_dir().unwrap().to_path_buf();
    assert!(journal.exists());
    assert!(!r.view.leave(Exit::Leave));
    drv.idle(&mut r.view, 1);
    drv.key(&mut r.view, Key::Tab, Modifiers::SHIFT);
    assert!(focused(&drv, "[ Discard ]"));
    drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
    assert_eq!(r.view.take_chosen(), vec![Chosen::Discard(Exit::Leave)]);
    r.session.discard();
    assert!(!journal.exists(), "the kept edits are gone");
    assert_eq!(std::fs::read(&r.target).unwrap(), b"xline one\n");
}

/// ES-5 (07 §10): an exit asked while a Save runs waits for it — no prompt meanwhile — and goes
/// once it lands; one that fails leaves the edit, and the next exit asks.
#[test]
fn es5_a_running_save_makes_the_exit_wait() {
    let mut v = view("abc\n");
    let mut drv = Driver::new(1000.0, 400.0);
    v.press(&Press::Text("x".into()));
    v.set_file(file(true));
    assert!(!v.leave(Exit::Leave));
    assert!(!v.asking(), "no prompt while it saves");
    drv.idle(&mut v, 1);
    assert!(status_row(&drv).contains(WAITS), "{}", status_row(&drv));
    assert_eq!(v.take_chosen(), vec![]);
    v.editor_mut().mark_saved();
    v.landed(&Saved::Written {
        file: "notes.txt".into(),
        bytes: 5,
        encoding: Encoding::Utf8,
        backup: None,
        kept: None,
        unguarded: None,
        notes: vec![],
    });
    assert_eq!(v.take_chosen(), vec![Chosen::Go(Exit::Leave)]);
    // A Save that fails: the exit is dropped, the edit stays, and the next exit asks.
    v.press(&Press::Text("y".into()));
    v.set_file(file(true));
    assert!(!v.leave(Exit::Close));
    v.not_saved("cannot write notes.txt".into());
    assert_eq!(v.take_chosen(), vec![]);
    v.set_file(file(false));
    assert!(!v.leave(Exit::Close));
    assert!(v.asking());
}

/// §9.6: where the server cannot replace a file in one step the prompt's first button is
/// Save copy; once the copy lands the exit goes (the edits are in it).
#[test]
fn the_close_prompt_offers_save_copy_where_save_is_off() {
    let mut v = view("abc\n");
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&Press::Text("x".into()));
    v.set_file(FileState {
        can_replace: false,
        ..file(false)
    });
    assert!(!v.leave(Exit::Close));
    drv.idle(&mut v, 1);
    let buttons = "[ Save copy ]  [ Save As… ]  [ Discard ]  [ Cancel ]";
    assert!(shows(&drv, buttons), "{buttons}");
    assert!(!shows(&drv, "[ Save ]"));
    for _ in 0..3 {
        drv.key(&mut v, Key::Tab, Modifiers::SHIFT);
    }
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(v.take_commands(), vec![Command::SaveCopy]);
    assert_eq!(v.take_chosen(), vec![]);
    v.elsewhere("saved a copy at notes.edited.txt".into());
    assert_eq!(v.take_chosen(), vec![Chosen::Go(Exit::Close)]);
}

/// An exit's Save that is given up (its Save As modal cancelled) drops the exit: a later Save
/// that lands closes nothing; a new exit asked replaces the one a Save waited for.
#[test]
fn a_given_up_save_or_a_new_exit_drops_the_waiting_one() {
    let written = Saved::Written {
        file: "notes.txt".into(),
        bytes: 5,
        encoding: Encoding::Utf8,
        backup: None,
        kept: None,
        unguarded: None,
        notes: vec![],
    };
    let mut v = view("abc\n");
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&Press::Text("x".into()));
    assert!(!v.leave(Exit::Close));
    drv.idle(&mut v, 1);
    drv.key(&mut v, Key::ArrowLeft, Modifiers::NONE);
    drv.key(&mut v, Key::ArrowLeft, Modifiers::NONE);
    assert!(focused(&drv, "[ Save As… ]"));
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(v.take_commands(), vec![Command::SaveAs]);
    v.gave_up();
    v.editor_mut().mark_saved();
    v.landed(&written);
    assert_eq!(v.take_chosen(), vec![], "the F2 after it closes nothing");
    // A prompt's Save waits; then another exit is asked and cancelled: the Save that lands
    // takes neither.
    v.press(&Press::Text("y".into()));
    assert!(!v.leave(Exit::Close));
    drv.idle(&mut v, 1);
    for _ in 0..3 {
        drv.key(&mut v, Key::Tab, Modifiers::SHIFT);
    }
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(v.take_commands(), vec![Command::Save]);
    assert!(!v.leave(Exit::View));
    drv.idle(&mut v, 1);
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    v.editor_mut().mark_saved();
    v.landed(&written);
    assert_eq!(v.take_chosen(), vec![]);
}

/// The binding as ED-layout will run it: each pass's keys and text given to the editor's
/// keys first, its exit commands to [`EditView::leave`], then the view's pass — and the events
/// the pass leaves for whatever comes after it.
struct Bound(EditView, Vec<Event>);

impl crate::gui::Window for Bound {
    fn ui(&mut self, ui: &mut egui::Ui) {
        for e in ui.ctx().input(|i| i.events.clone()) {
            let press = match e {
                Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } => Press::Key {
                    key,
                    physical: physical_key,
                    modifiers,
                },
                Event::Text(t) => Press::Text(t),
                _ => continue,
            };
            let exit = match self.0.press(&press) {
                Command::Leave => Some(Exit::Leave),
                Command::Close => Some(Exit::Close),
                Command::Reread => Some(Exit::Reread),
                Command::View => Some(Exit::View),
                _ => None,
            };
            if let Some(exit) = exit {
                self.0.leave(exit);
            }
        }
        crate::gui::Window::ui(&mut self.0, ui);
        self.1 = ui.ctx().input(|i| i.events.clone());
    }
    fn ready(&self) -> bool {
        true
    }
}

/// The key that opens a dialog never answers it in the same pass, its keys given to the
/// editor before the dialogs' pass: F10 leaves the bar up, then F S saves; the Esc that leads
/// to an exit leaves the prompt up, on Cancel. While anything is up no key or text is left in
/// the pass for what comes after the view.
#[test]
fn the_opening_key_does_not_answer_its_dialog() {
    let mut b = Bound(view("abc\n"), vec![]);
    let mut drv = Driver::new(640.0, 400.0);
    drv.key(&mut b, Key::F10, Modifiers::NONE);
    assert!(b.0.asking(), "F10 left its bar up");
    drv.press_typed(&mut b, Key::F, "f");
    drv.press_typed(&mut b, Key::S, "s");
    assert_eq!(b.0.take_commands(), vec![Command::Save]);
    assert!(!b.0.asking());
    drv.press_typed(&mut b, Key::X, "x");
    assert_eq!(doc(&b.0), b"xabc\n", "typed through the binding");
    drv.key(&mut b, Key::Escape, Modifiers::NONE);
    assert!(
        b.0.asking(),
        "the Esc that asked to leave left the prompt up"
    );
    assert!(focused(&drv, "[ Cancel ]"));
    let event = |key| Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    drv.frame(
        &mut b,
        vec![event(Key::S), Event::Text("s".into())],
        Modifiers::NONE,
    );
    assert!(b.0.asking());
    let left = |b: &Bound| {
        (b.1.iter())
            .filter(|e| matches!(e, Event::Key { .. } | Event::Text(_)))
            .count()
    };
    assert_eq!(left(&b), 0, "no key left after the view's pass");
    drv.key(&mut b, Key::Enter, Modifiers::NONE);
    assert!(!b.0.asking());
    assert_eq!(doc(&b.0), b"xabc\n");
    assert_eq!(b.0.take_chosen(), vec![]);
}

/// E31: under a prompt a terminal's focus report still reaches the keys, which forget Shift
/// known — so Ctrl+S after it arms `^S` again rather than saving.
#[test]
fn a_focus_report_under_a_prompt_forgets_shift_known() {
    let mut v = view_on("abc\n", "notes.txt", Backend::Terminal, Doubt::default());
    v.press(&Press::key(Key::Z, Modifiers::COMMAND | Modifiers::SHIFT));
    assert!(v.keys.shift_known());
    v.press(&Press::Text("x".into()));
    assert!(!v.leave(Exit::Close));
    v.press(&Press::Focus);
    assert!(!v.keys.shift_known());
}

// ---------------------------------------------------------------------------------------------
// ES-2's dialog: a file changed since it was read.

/// ES-2 (§9.2): a real conflict — the size changed, or the file was deleted — opens the dialog
/// with its line, on Cancel; Overwrite only while the file is still a regular file; letters
/// answer nothing. Reload asks first (on Cancel too), then hands Reload back; Overwrite and
/// Save As… go to the binding.
#[test]
fn es2_the_conflict_dialog_names_the_change_on_cancel() {
    type Change = fn(&Path);
    let changes: [(&str, Change, bool); 2] = [
        ("its size", |t| std::fs::write(t, b"short\n").unwrap(), true),
        ("deleted", |t| std::fs::remove_file(t).unwrap(), false),
    ];
    for (what, change, overwrite) in changes {
        let mut r = rig(b"line one\n");
        let mut drv = Driver::new(640.0, 400.0);
        r.view.press(&Press::Text("x".into()));
        change(&r.target);
        let saved = (r.session)
            .save(r.view.editor_mut(), &mut LocalFs::new())
            .unwrap();
        let Saved::Conflict { line, staged, .. } = &saved else {
            panic!("{what}: {saved:?}")
        };
        r.view.landed(&saved);
        drv.idle(&mut r.view, 1);
        assert!(shown_wrapped(&drv, line), "{what}: {line}");
        let kept = format!("your edit is kept at {}", crate::gui::binary::tilde(staged));
        assert!(shown_wrapped(&drv, &kept), "{what}: the staged copy named");
        assert_eq!(shows(&drv, "[ Overwrite ]"), overwrite, "{what}");
        assert!(shows(&drv, "[ Reload ]") && shows(&drv, "[ Save As… ]"));
        assert!(focused(&drv, "[ Cancel ]"), "{what}");
        every_letter(&mut drv, &mut r.view);
        assert!(r.view.asking(), "{what}");
        drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
        assert!(!r.view.asking(), "{what}");
        assert_eq!(r.view.take_chosen(), vec![], "{what}");
        assert_eq!(r.view.take_commands(), vec![], "{what}");
        assert!(
            !r.view.editor().buffer().is_saved(),
            "{what}: the edit stays"
        );
        // Reload asks first, on Cancel.
        r.view.landed(&saved);
        drv.idle(&mut r.view, 1);
        let lefts = if overwrite { 3 } else { 2 };
        for _ in 0..lefts {
            drv.key(&mut r.view, Key::ArrowLeft, Modifiers::NONE);
        }
        assert!(focused(&drv, "[ Reload ]"), "{what}");
        drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
        let ask = "Discard your changes and read notes.txt again?";
        assert!(shown_wrapped(&drv, ask), "{what}");
        assert!(
            focused(&drv, "[ Cancel ]"),
            "{what}: the question on Cancel"
        );
        drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
        assert!(!r.view.asking());
        assert_eq!(r.view.take_chosen(), vec![]);
        r.view.landed(&saved);
        drv.idle(&mut r.view, 1);
        for _ in 0..lefts {
            drv.key(&mut r.view, Key::ArrowLeft, Modifiers::NONE);
        }
        drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
        drv.key(&mut r.view, Key::ArrowLeft, Modifiers::NONE);
        assert!(focused(&drv, "[ Reload ]"), "{what}");
        drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
        assert_eq!(r.view.take_chosen(), vec![Chosen::Reload], "{what}");
        // Save As…, and Overwrite where offered.
        r.view.landed(&saved);
        drv.idle(&mut r.view, 1);
        drv.key(&mut r.view, Key::ArrowLeft, Modifiers::NONE);
        drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
        assert_eq!(r.view.take_commands(), vec![Command::SaveAs], "{what}");
        if overwrite {
            r.view.landed(&saved);
            drv.idle(&mut r.view, 1);
            drv.key(&mut r.view, Key::ArrowLeft, Modifiers::NONE);
            drv.key(&mut r.view, Key::ArrowLeft, Modifiers::NONE);
            drv.key(&mut r.view, Key::Enter, Modifiers::NONE);
            assert_eq!(r.view.take_chosen(), vec![Chosen::Overwrite]);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// §7.2: the encoding question.

/// E37 in cells: the first non-ASCII character typed into a guessed encoding asks in a box on
/// Cancel; Enter types nothing; Type types it, and the question is not asked again.
#[test]
fn the_encoding_question_is_a_box_on_cancel() {
    let guessed = Doubt {
        guessed: true,
        undecodable: 0,
    };
    let mut v = view_on("abc\n", "notes.txt", Backend::Window, guessed);
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&Press::Text("a".into()));
    assert!(!v.asking(), "ASCII: no question");
    v.press(&Press::Text("한".into()));
    drv.idle(&mut v, 1);
    let ask = "Type 한 as UTF-8? The encoding was guessed";
    assert!(shown_wrapped(&drv, ask), "{ask}");
    assert!(shows(&drv, "[ Type ]  [ Cancel ]"));
    assert!(focused(&drv, "[ Cancel ]"));
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(doc(&v), b"aabc\n", "Cancel types nothing");
    assert!(!v.asking());
    v.press(&Press::Text("한".into()));
    drv.idle(&mut v, 1);
    drv.key(&mut v, Key::ArrowLeft, Modifiers::NONE);
    assert!(focused(&drv, "[ Type ]"));
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(doc(&v), "a한abc\n".as_bytes());
    v.press(&Press::Text("글".into()));
    assert!(!v.asking(), "not asked again for this file");
    assert_eq!(doc(&v), "a한글abc\n".as_bytes());
}

// ---------------------------------------------------------------------------------------------
// §4: F10's menus.

/// The stand-in window that keeps the IME output of its last pass.
struct Ime(EditView, Option<egui::output::IMEOutput>);

impl crate::gui::Window for Ime {
    fn ui(&mut self, ui: &mut egui::Ui) {
        crate::gui::Window::ui(&mut self.0, ui);
        self.1 = ui.ctx().output(|o| o.ime);
    }
    fn ready(&self) -> bool {
        true
    }
}

/// §4, §5.5: F10 puts the bar on File in the title row, the status row naming the menus'
/// letters; a menu's letter drops its items under its name, the status row naming theirs; an
/// item's letter runs it — F10 F S saves (the binding's Save), F10 E R redoes — by the physical
/// key, or a 2-set jamo as a terminal sends it; the input method is off meanwhile.
#[test]
fn f10_the_menus_and_their_letters() {
    let mut w = Ime(view("abc\n"), None);
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut w, 1);
    assert!(w.1.is_some(), "the input method at the caret");
    w.0.press(&Press::key(Key::F10, Modifiers::NONE));
    drv.idle(&mut w, 1);
    assert!(w.0.asking());
    assert_eq!(
        w.1, None,
        "the input method off while the menus wait for a letter"
    );
    assert!(focused(&drv, "File"), "the bar on File");
    assert_eq!(
        status_row(&drv).trim(),
        "←→ choose · Enter opens · Esc closes · a letter: F E S B G O"
    );
    drv.press_typed(&mut w, Key::F, "f");
    assert!(focused(&drv, "[ Save  "), "the first item has the keyboard");
    let layers = cells::layers(&drv.ctx);
    let hit = *cells::find(&layers, "[ Save  ").last().unwrap();
    assert_eq!(
        on_screen(&layers[hit.layer], hit.cells.x, hit.cells.y),
        (9, 3),
        "under its name: its box's corner two cells left of it, a row down"
    );
    assert_eq!(
        status_row(&drv).trim(),
        "File  ↑↓ choose · Enter runs · Esc closes · a letter: S A C R V X"
    );
    assert_eq!(w.1, None);
    drv.press_typed(&mut w, Key::S, "s");
    assert!(!w.0.asking());
    assert_eq!(w.0.take_commands(), vec![Command::Save], "F10 F S saves");
    // F10 E R redoes.
    w.0.press(&Press::Text("y".into()));
    w.0.press(&ctrl(Key::Z));
    assert_eq!(doc(&w.0), b"abc\n");
    w.0.press(&Press::key(Key::F10, Modifiers::NONE));
    drv.press_typed(&mut w, Key::E, "e");
    drv.press_typed(&mut w, Key::R, "r");
    assert_eq!(doc(&w.0), b"yabc\n", "F10 E R redoes");
    assert!(!w.0.asking());
    // Under the Korean input source: the physical key with its jamo, and jamo alone.
    w.0.press(&Press::key(Key::F10, Modifiers::NONE));
    drv.press_typed(&mut w, Key::F, "ㄹ");
    drv.frame(&mut w, vec![Event::Text("ㄴ".into())], Modifiers::NONE);
    assert_eq!(w.0.take_commands(), vec![Command::Save], "F10 ㄹ ㄴ saves");
    assert!(!w.0.asking());
}

/// ← → move the bar, Enter or ↓ drops a menu, ↑ ↓ move its keyboard, a click on a name opens
/// it, Esc closes the innermost first; an item with no command yet takes no letter.
#[test]
fn the_menus_keys_and_esc_closes_the_innermost() {
    let mut v = view("abc\n");
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&Press::key(Key::F10, Modifiers::NONE));
    drv.key(&mut v, Key::ArrowRight, Modifiers::NONE);
    assert!(focused(&drv, "Edit"));
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert!(focused(&drv, "[ Undo "));
    drv.key(&mut v, Key::ArrowDown, Modifiers::NONE);
    assert!(focused(&drv, "[ Redo "));
    drv.key(&mut v, Key::ArrowRight, Modifiers::NONE);
    assert!(focused(&drv, "[ Find… "), "the next menu");
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    assert!(v.asking() && !shows(&drv, "[ Find… "), "back to the bar");
    assert!(focused(&drv, "Search"));
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    assert!(!v.asking(), "closed");
    // A click on a name opens its menu.
    let at = cells::find(&cells::layers(&drv.ctx), "Block")[0];
    let l = &cells::layers(&drv.ctx)[at.layer];
    drv.click(&mut v, l.rect(at.cells).center());
    assert!(focused(&drv, "[ Begin "));
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    // The Git menu's items are ED-git's: off, and their letters take nothing.
    v.press(&Press::key(Key::F10, Modifiers::NONE));
    drv.press_typed(&mut v, Key::G, "g");
    assert!(shows(&drv, "[ Stage this file "));
    drv.press_typed(&mut v, Key::S, "s");
    assert!(v.asking(), "an item off takes no letter");
    assert_eq!(v.take_commands(), vec![]);
}

// ---------------------------------------------------------------------------------------------
// The status row: Go to, Find, Replace's question.

/// Types `text` into the field with the keyboard, a key and its text at a time.
fn type_in(drv: &mut Driver, v: &mut EditView, text: &str) {
    for c in text.chars() {
        let name = c.to_ascii_uppercase().to_string();
        let key = Key::from_name(&name).unwrap_or(Key::Space);
        drv.press_typed(v, key, &c.to_string());
    }
}

/// Ctrl/⌘+G: Go to line's field on the status row; Enter goes to the line's start, a word that
/// is no line number is refused with its line, Esc moves nothing.
#[test]
fn go_to_line_is_a_field_on_the_status_row() {
    let text: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    let mut v = view(&text);
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&ctrl(Key::G));
    drv.idle(&mut v, 1);
    assert!(v.asking());
    assert!(
        status_row(&drv).starts_with(" Go to line: "),
        "{}",
        status_row(&drv)
    );
    type_in(&mut drv, &mut v, "12");
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert!(!v.asking());
    assert_eq!(
        v.editor().caret(),
        v.editor().buffer().line_start(11).unwrap()
    );
    v.press(&ctrl(Key::G));
    type_in(&mut drv, &mut v, "x");
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert!(status_row(&drv).contains("x is not a line number"));
    assert_eq!(
        v.editor().caret(),
        v.editor().buffer().line_start(11).unwrap()
    );
    v.press(&ctrl(Key::G));
    type_in(&mut drv, &mut v, "3");
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    assert!(!v.asking());
    assert_eq!(
        v.editor().caret(),
        v.editor().buffer().line_start(11).unwrap()
    );
}

/// Ctrl/⌘+F on the status row (§4, §5.1): `Find:`, Enter to `Options:` — letters by the
/// physical key or a jamo — Enter finds; Ctrl+L finds again. Tab moves to `Replace:`; each match
/// asks on the status row (Y replaces it, N skips it, other letters nothing); N replaces all at
/// once.
#[test]
fn find_and_replace_on_the_status_row() {
    let mut v = view("one two one TWO one\n");
    let mut drv = Driver::new(640.0, 400.0);
    v.press(&ctrl(Key::F));
    drv.idle(&mut v, 1);
    assert!(
        status_row(&drv).starts_with(" Find: "),
        "{}",
        status_row(&drv)
    );
    type_in(&mut drv, &mut v, "two");
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert!(
        status_row(&drv).contains(" Options: "),
        "{}",
        status_row(&drv)
    );
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert!(!v.asking());
    assert_eq!(v.editor().shown(), Some(4..7));
    // Ignore case (U) typed as ㅕ on its key, then Ctrl+L finds again.
    v.editor_mut().place(0, false);
    v.press(&ctrl(Key::F));
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    drv.press_typed(&mut v, Key::U, "ㅕ");
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(v.editor().shown(), Some(4..7));
    v.press(&ctrl(Key::L));
    assert_eq!(
        v.editor().shown(),
        Some(12..15),
        "Ctrl+L: the next, any case"
    );
    // Replace, asking at each match.
    v.editor_mut().place(0, false);
    v.press(&ctrl(Key::F));
    type_in(&mut drv, &mut v, "one");
    drv.key(&mut v, Key::Tab, Modifiers::NONE);
    assert!(
        status_row(&drv).contains(" Replace: "),
        "{}",
        status_row(&drv)
    );
    type_in(&mut drv, &mut v, "1");
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    drv.press_typed(&mut v, Key::Backspace, "");
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert_eq!(
        status_row(&drv).trim(),
        "Replace this one? Y replaces it · N skips it · Esc stops"
    );
    assert_eq!(v.editor().shown(), Some(0..3));
    drv.press_typed(&mut v, Key::K, "k");
    assert_eq!(v.editor().shown(), Some(0..3), "another letter: nothing");
    drv.press_typed(&mut v, Key::Y, "y");
    assert_eq!(v.editor().shown(), Some(6..9), "the next");
    drv.press_typed(&mut v, Key::N, "ㅜ");
    drv.press_typed(&mut v, Key::Y, "y");
    assert!(!v.asking());
    assert_eq!(doc(&v), b"1 two one TWO 1\n");
    assert!(
        status_row(&drv).contains("replaced 2"),
        "{}",
        status_row(&drv)
    );
    // N: every match at once, one undo step.
    v.editor_mut().place(0, false);
    v.press(&ctrl(Key::F));
    type_in(&mut drv, &mut v, "two");
    drv.key(&mut v, Key::Tab, Modifiers::NONE);
    type_in(&mut drv, &mut v, "2");
    drv.key(&mut v, Key::Tab, Modifiers::NONE);
    type_in(&mut drv, &mut v, "ung");
    drv.key(&mut v, Key::Enter, Modifiers::NONE);
    assert!(!v.asking());
    assert_eq!(doc(&v), b"1 2 one 2 1\n");
    v.press(&ctrl(Key::Z));
    assert_eq!(doc(&v), b"1 two one TWO 1\n");
}

// ---------------------------------------------------------------------------------------------
// One prompt in both backends.

/// The close prompt over a Hangul name at 80×25: what a terminal shows (`term::compose` of the
/// pass's layers) is the window's cells — the prompt's box and shadow over the editor's rows,
/// the cells under it blanked first, no hand-off box.
fn hangul_prompt() -> (Driver, EditView) {
    let line = "한글 줄: 가나다라마바사아자차카타파하 가나다라마바사아자차카타파하 끝\n";
    let body = line.repeat(30);
    let mut v = view_on(&body, "한글 메모.txt", Backend::Window, Doubt::default());
    let mut drv = Driver::new(640.0, 400.0);
    v.set_file(FileState {
        shown: "~/notes/한글 메모.txt".into(),
        ..file(false)
    });
    v.press(&Press::Text("x".into()));
    assert!(!v.leave(Exit::Close));
    drv.idle(&mut v, 2);
    (drv, v)
}

#[test]
fn a_prompt_over_a_hangul_name_is_the_same_cells_in_a_terminal() {
    let (drv, _v) = hangul_prompt();
    let layers = cells::layers(&drv.ctx);
    let screen = crate::gui::term::compose(&drv.ctx, &layers, 80, 25);
    assert_eq!(screen.hand_offs, vec![], "every cell is the window's");
    let main = (layers.iter())
        .max_by_key(|l| l.buf.area.height)
        .expect("the editor's cells");
    let boxed = (layers.iter())
        .find(|l| {
            cells::row_text(&l.buf, l.buf.area.y)
                .0
                .contains("Unsaved changes")
        })
        .expect("the prompt's cells");
    assert_eq!(screen.caret, None, "no caret under a prompt");
    // The box's cells are on its own grid, between the title and status rows.
    let (dx, dy) = on_screen(boxed, 0, 0);
    for y in 0..main.buf.area.height {
        for x in 0..80u16 {
            let at = (x, y);
            let mine = (x.wrapping_sub(dx), y.wrapping_sub(dy));
            let over = boxed.buf.area.contains(Position::new(mine.0, mine.1))
                && !matches!(boxed.buf[mine].bg, ratatui_core::style::Color::Reset);
            let want = if over {
                &boxed.buf[mine]
            } else {
                &main.buf[at]
            };
            let got = &screen.buf[at];
            assert_eq!(
                (want.symbol(), want.fg, want.bg),
                (got.symbol(), got.fg, got.bg),
                "cell {x},{y}"
            );
        }
    }
    let question = "Save the changes to ~/notes/한글 메모.txt before closing?";
    let rows: Vec<String> = (0..25).map(|y| cells::row_text(&screen.buf, y).0).collect();
    assert!(rows.iter().any(|r| r.contains(question)), "{rows:#?}");
}

/// The File menu over the Hangul body: its box laid on a grid of its own under its name is
/// the same cells in a terminal as in the window.
#[test]
fn a_menu_over_a_hangul_body_is_the_same_cells_in_a_terminal() {
    let (mut drv, mut v) = hangul_prompt();
    drv.key(&mut v, Key::Escape, Modifiers::NONE);
    assert!(!v.asking());
    v.press(&Press::key(Key::F10, Modifiers::NONE));
    drv.press_typed(&mut v, Key::F, "f");
    let layers = cells::layers(&drv.ctx);
    let screen = crate::gui::term::compose(&drv.ctx, &layers, 80, 25);
    assert_eq!(screen.hand_offs, vec![]);
    let main = (layers.iter())
        .max_by_key(|l| l.buf.area.height)
        .expect("the editor's cells");
    let boxed = (layers.iter())
        .find(|l| {
            (0..l.buf.area.height).any(|y| {
                cells::row_text(&l.buf, l.buf.area.y + y)
                    .0
                    .contains("[ Save  ")
            })
        })
        .expect("the menu's cells");
    let (dx, dy) = on_screen(boxed, 0, 0);
    for y in 0..main.buf.area.height {
        for x in 0..80u16 {
            let mine = (x.wrapping_sub(dx), y.wrapping_sub(dy));
            let over = boxed.buf.area.contains(Position::new(mine.0, mine.1))
                && !matches!(boxed.buf[mine].bg, ratatui_core::style::Color::Reset);
            let want = if over {
                &boxed.buf[mine]
            } else {
                &main.buf[(x, y)]
            };
            let got = &screen.buf[(x, y)];
            assert_eq!(
                (want.symbol(), want.fg, want.bg),
                (got.symbol(), got.fg, got.bg),
                "cell {x},{y}"
            );
        }
    }
    let row = cells::row_text(&screen.buf, 3).0;
    assert!(
        row[row.find('[').unwrap()..].starts_with("[ Save "),
        "{row}"
    );
}

/// The prompt's cells at 80×25, the window's layers and the terminal's screen, for a look
/// (`cargo test --lib -- --ignored --exact gui::edit::dialogs::tests::dialog_cells_dump_80_by_25
/// --nocapture`).
#[test]
#[ignore]
fn dialog_cells_dump_80_by_25() {
    let (drv, _v) = hangul_prompt();
    let layers = cells::layers(&drv.ctx);
    for l in &layers {
        let a = l.buf.area;
        println!(
            "-- window layer at {},{} ({}x{})",
            a.x, a.y, a.width, a.height
        );
        for y in a.top()..a.bottom() {
            println!("{}", cells::row_text(&l.buf, y).0);
        }
    }
    let screen = crate::gui::term::compose(&drv.ctx, &layers, 80, 25);
    println!("-- terminal");
    print!("{}", crate::gui::term::cells_text(&screen, None));
}
