//! X2-switch's question about the git panel before the switch to another machine (10 §7.4,
//! Tensions "§7.4's 'an open dialog with its typed text' vs the git panel's dialogs"), as v2
//! (X4-state) leaves it: a git dialog the state carries asks nothing; the planner's plan goes
//! with the switch and asks first, as unsaved edits do — Switch · Cancel.

use super::s2::{small, Gx};
use super::*;
use crate::git::history::fixture::{self, Repo};

/// Whether a text the last pass drew holds `text` (a dialog's rows are wrapped).
fn has(g: &Gx, text: &str) -> bool {
    g.fx.drv.texts().iter().any(|(_, _, t)| t.contains(text))
}

/// The commit dialog open with a message typed: the state carries it (v2), so the switch asks
/// nothing and goes.
#[test]
fn a_git_dialog_open_asks_nothing_before_the_switch() {
    let Some(git) = fixture::git() else {
        return;
    };
    let repo = small(&git);
    repo.run(&["add", "a.txt"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.f(Key::F7);
    g.fx.typed("half a message");
    g.idle(2);
    assert!(has(&g, "half a message"), "{:#?}", g.fx.drv.texts());
    assert_eq!(g.fx.b.before_switch("box"), Before::Go);
}

/// The planner open: its plan goes with the switch, and the switch asks first; Cancel keeps it.
#[test]
fn the_planners_plan_asks_before_the_switch() {
    let Some(git) = fixture::git() else {
        return;
    };
    let repo = Repo::new(&git);
    for (i, name) in ["base", "A", "B"].into_iter().enumerate() {
        repo.write("c.txt", format!("{i}\n"));
        repo.commit(name);
    }
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("base");
    g.cmd(Key::E);
    g.idle(2);
    g.until_quiet("the planner's range");
    assert!(
        g.panel().view.planner_model().is_some(),
        "the planner is open"
    );
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    g.idle(2);
    assert!(
        has(
            &g,
            "The planner's unsaved plan goes with the switch to box."
        ),
        "{:#?}",
        g.fx.drv.texts()
    );
    g.fx.drv.click_text_on_top(&mut g.fx.b, "[ Cancel ]");
    g.idle(2);
    assert!(
        g.panel().view.planner_model().is_some(),
        "the plan as it was"
    );
    assert!(!g.fx.b.switch_due());
}

// ---------------------------------------------------------------------------------------------
// F-git-covered: the switch's question is another dialog to the git panel's.

/// What [`Bursts`] notes of the browser when it sends a burst: the planner open, the switch's
/// question up.
type Noted = (bool, bool);

/// A terminal that sends its bursts of events back to back — each waiting while the next is
/// unread, so the backend prints only the last pass of a burst — one burst once the backend has
/// asked three times since the last (its passes are done and printed), then ends the run
/// (`tests_x4_dialogs.rs`' stand-in), noting the browser as each burst is sent.
struct Bursts {
    bursts: std::collections::VecDeque<Vec<crossterm::event::Event>>,
    now: std::collections::VecDeque<crossterm::event::Event>,
    asked: usize,
    noted: std::rc::Rc<std::cell::Cell<Noted>>,
    sent_at: std::rc::Rc<std::cell::RefCell<Vec<Noted>>>,
}

impl crate::gui::term::Tty for Bursts {
    fn raw(&mut self, _on: bool) -> std::io::Result<()> {
        Ok(())
    }
    fn size(&mut self) -> std::io::Result<(u16, u16)> {
        Ok((100, 30))
    }
    fn probe(&mut self) -> crate::gui::term::Probed {
        crate::gui::term::Probed::default()
    }
    fn drop_input(&mut self) {}
    fn next(&mut self, _wait: Duration) -> std::io::Result<Option<crossterm::event::Event>> {
        if let Some(ev) = self.now.pop_front() {
            return Ok(Some(ev));
        }
        self.asked += 1;
        assert!(self.asked < 20_000, "the script ran out without an end");
        if self.asked >= 3 {
            if let Some(burst) = self.bursts.pop_front() {
                self.sent_at.borrow_mut().push(self.noted.get());
                self.asked = 0;
                self.now.extend(burst);
                return Ok(self.now.pop_front());
            }
        }
        std::thread::sleep(Duration::from_millis(1));
        Ok(None)
    }
    fn pending(&mut self) -> std::io::Result<bool> {
        Ok(!self.now.is_empty())
    }
    fn stopped(&self) -> Option<i32> {
        (self.bursts.is_empty() && self.now.is_empty() && self.asked >= 3).then_some(libc::SIGTERM)
    }
}

/// The browser in the terminal, noting after each pass whether `top`'s planner is open and the
/// switch's question up.
struct InTerminal<'a> {
    b: &'a mut Browser,
    top: String,
    noted: std::rc::Rc<std::cell::Cell<Noted>>,
}

impl crate::gui::Window for InTerminal<'_> {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.b.ui(ui);
        let git = &self.b.git;
        let open =
            (git.panel(&self.top)).is_some_and(|i| git.panels[i].view.planner_model().is_some());
        self.noted.set((open, self.b.switch_asking()));
    }

    fn ready(&self) -> bool {
        true
    }
}

/// F-git-covered, the fix-check's sequence in `sopen --tui`: the planner's Esc question up (the
/// plan changed), then F12's question over it; `[Esc, Enter]` in one read — the Esc cancels the
/// switch in a pass the terminal does not print, the Enter reaches the planner's question, whose
/// last printed frame stood under the switch's — closes nothing; the next Enter, once the
/// question was printed with the keyboard, closes the planner.
#[test]
fn f_git_covered_the_planners_question_under_the_switchs_stays() {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
    let Some(git) = fixture::git() else {
        return;
    };
    let repo = Repo::new(&git);
    for (i, name) in ["base", "A", "B"].into_iter().enumerate() {
        repo.write("c.txt", format!("{i}\n"));
        repo.commit(name);
    }
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("base");
    g.cmd(Key::E);
    g.idle(2);
    g.until_quiet("the planner's range");
    g.fx.typed("d");
    g.idle(2);
    g.key(Key::Escape);
    g.idle(2);
    assert!(
        has(&g, "Close the planner and lose its changes?"),
        "Esc's question: {:#?}",
        g.fx.drv.texts()
    );
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    let press = |code| CEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let settle = || vec![CEvent::FocusGained];
    let bursts = vec![
        settle(),
        settle(),
        vec![press(KeyCode::Esc), press(KeyCode::Enter)],
        settle(),
        vec![press(KeyCode::Enter)],
    ];
    let noted = std::rc::Rc::new(std::cell::Cell::new((true, true)));
    let sent_at = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let tty = Bursts {
        bursts: bursts.into(),
        now: Default::default(),
        asked: 0,
        noted: noted.clone(),
        sent_at: sent_at.clone(),
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut backend = crate::gui::term::Backend::new(tty, std::io::sink(), ctx, true);
    let top = g.top();
    let mut win = InTerminal {
        b: &mut g.fx.b,
        top,
        noted: noted.clone(),
    };
    let mut no_switch =
        |_: &mut crate::gui::term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = backend.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, crate::gui::term::End::Signal(libc::SIGTERM));
    assert_eq!(
        *sent_at.borrow(),
        [
            (true, true),
            (true, true),
            (true, true),
            (true, false),
            (true, false)
        ],
        "the switch's question up until the Esc, the planner open until the last Enter"
    );
    assert_eq!(
        noted.get(),
        (false, false),
        "the last Enter, the question printed with the keyboard, closed it"
    );
}

/// F-git-covered review 1: a press on the git panel while F12's question is up moves no keyboard
/// there, as under the list's question: the planner open, the keyboard back in the list, the
/// question up over both; a click on the panel's rows, outside the question, cancels it (a click
/// outside is Cancel) and leaves the keyboard in the list.
#[test]
fn f_git_covered_a_press_under_the_switchs_question_moves_no_keyboard() {
    let Some(git) = fixture::git() else {
        return;
    };
    let repo = Repo::new(&git);
    for (i, name) in ["base", "A", "B"].into_iter().enumerate() {
        repo.write("c.txt", format!("{i}\n"));
        repo.commit(name);
    }
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("base");
    g.cmd(Key::E);
    g.idle(2);
    g.until_quiet("the planner's range");
    g.key(Key::Tab);
    g.idle(2);
    assert_eq!(g.b().git_focused(), None, "the keyboard in the list");
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    g.idle(2);
    assert!(g.fx.b.switch_asking());
    // On the panel's side, right of the question's box (50 cells, centred).
    g.fx.drv.click(&mut g.fx.b, egui::pos2(960.0, 120.0));
    g.idle(2);
    assert!(!g.fx.b.switch_asking(), "the click outside cancelled it");
    assert_eq!(g.b().git_focused(), None, "and moved no keyboard");
}

/// A viewer closed while its save or export still writes (its window in the browser's
/// `Leftover` until it settles) holds the switch as a running job does (10 §7.4: every running
/// job that writes): `waiting for the save or export of <file>…`, then the switch once it ends.
#[test]
fn a_write_left_by_a_closed_viewer_holds_the_switch() {
    let mut fx = fixture();
    let done = Arc::new(AtomicBool::new(false));
    let ended = done.clone();
    fx.b.leftover.borrow_mut().closing.push(Closing {
        settle: Box::new(move |_| ended.load(Ordering::SeqCst).then(Outcome::default)),
        hold: None,
        file: "box:/srv/clip.mp4".to_string(),
    });
    assert_eq!(fx.b.before_switch("box"), Before::Held, "it writes");
    fx.drv.idle(&mut fx.b, 2);
    assert_eq!(
        fx.b.status_line().text,
        "waiting for the save or export of box:/srv/clip.mp4…"
    );
    assert!(!fx.b.switch_due(), "not while it writes");
    done.store(true, Ordering::SeqCst);
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.switch_due(), "asked for again once it has ended");
    assert_eq!(fx.b.before_switch("box"), Before::Go);
}

/// Unsaved edits in the viewer and the planner's plan: two questions in a row — Discard, then
/// Switch — and the switch goes, Discard's answer with it; neither asks again. The planner's panel
/// is not on screen then (the list's bar on `0.bin`, its viewer open): the state does not carry
/// it, and the question says the plan is dropped (X4-state's review).
#[test]
fn discard_then_switch_goes_after_both_questions() {
    let Some(git) = fixture::git() else {
        return;
    };
    let repo = Repo::new(&git);
    for (i, name) in ["base", "A", "B"].into_iter().enumerate() {
        repo.write("c.txt", format!("{i}\n"));
        repo.commit(name);
    }
    std::fs::write(repo.top.join("0.bin"), (0..=255u8).collect::<Vec<u8>>()).unwrap();
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("base");
    g.cmd(Key::E);
    g.idle(2);
    g.until_quiet("the planner's range");
    assert!(
        g.panel().view.planner_model().is_some(),
        "the planner is open"
    );
    // Back to the list, the hex viewer on `0.bin`, a byte typed over.
    g.key(Key::Tab);
    g.idle(2);
    g.fx.select("0.bin");
    g.key(Key::Enter);
    g.fx.settle();
    g.idle(2);
    g.key(Key::F8);
    g.key(Key::Enter);
    g.fx.typed("Z");
    g.idle(2);
    assert!(g.fx.b.would_lose(), "an unsaved edit");
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    g.idle(2);
    assert!(has(&g, "Save the changes to"), "{:#?}", g.fx.drv.texts());
    g.fx.drv.click_text_on_top(&mut g.fx.b, "[ Discard ]");
    g.idle(1);
    assert!(g.fx.b.switch_due());
    assert_eq!(
        g.fx.b.before_switch("box"),
        Before::Held,
        "the plan asks next"
    );
    g.idle(2);
    let texts = g.fx.drv.texts();
    let said: Vec<&str> = texts.iter().map(|(_, _, t)| t.as_str()).collect();
    let said = said.join(" ");
    let dropped = "The unsaved plan of a planner not on screen is not carried to box: switching \
                   drops it.";
    assert!(said.contains(dropped), "{said}");
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    assert!(
        state.git.iter().all(|p| p.plan.is_none()),
        "the state carries no plan"
    );
    g.fx.drv.click_text_on_top(&mut g.fx.b, "[ Switch ]");
    g.idle(1);
    assert!(g.fx.b.switch_due());
    assert_eq!(
        g.fx.b.before_switch("box"),
        Before::Go,
        "both answered: it goes"
    );
    assert!(g.fx.b.take_discard(), "with Discard's answer");
}

/// Discard answered, then a write starts before the switch is decided again: it waits, and once
/// the write has ended it is decided anew from nothing — the edits made meanwhile (still unsaved)
/// ask again; no answer outlives a wait.
#[test]
fn a_wait_after_an_answer_forgets_it() {
    let mut fx = fixture();
    std::fs::write(
        Path::new(&fx.root).join("0.bin"),
        (0..=255u8).collect::<Vec<u8>>(),
    )
    .unwrap();
    let root = fx.root.clone();
    fx.b.list(Some(0), root, None, None);
    fx.settle();
    fx.select("0.bin");
    fx.key(Key::Enter);
    fx.settle();
    fx.drv.idle(&mut fx.b, 2);
    fx.key(Key::F8);
    fx.key(Key::Enter);
    fx.typed("Z");
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.would_lose(), "an unsaved edit");
    assert_eq!(fx.b.before_switch("box"), Before::Held);
    fx.drv.idle(&mut fx.b, 2);
    fx.drv.click_text_on_top(&mut fx.b, "[ Discard ]");
    fx.drv.idle(&mut fx.b, 1);
    assert!(fx.b.switch_due(), "Discard lets it go");
    // A write starts before the switch is decided again.
    let done = Arc::new(AtomicBool::new(false));
    let ended = done.clone();
    fx.b.leftover.borrow_mut().closing.push(Closing {
        settle: Box::new(move |_| ended.load(Ordering::SeqCst).then(Outcome::default)),
        hold: None,
        file: "/elsewhere".to_string(),
    });
    assert_eq!(fx.b.before_switch("box"), Before::Held, "it waits");
    done.store(true, Ordering::SeqCst);
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.switch_due());
    assert_eq!(
        fx.b.before_switch("box"),
        Before::Held,
        "decided anew: the edit asks"
    );
    assert!(fx.b.switch_asking());
    assert!(!fx.b.take_discard());
}
