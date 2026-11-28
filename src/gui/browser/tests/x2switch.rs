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
