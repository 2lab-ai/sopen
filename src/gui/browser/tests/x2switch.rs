//! X2-switch's question about the git panel before the switch to another machine (10 §7.4,
//! Tensions "§7.4's 'an open dialog with its typed text' vs the git panel's dialogs"): the state
//! carries nothing of a git dialog or the planner's plan yet (X4 does), so with one open the
//! switch asks first — what it drops, Switch · Cancel.

use super::s2::{small, Gx};
use super::*;
use crate::git::history::fixture::{self, Repo};

/// Whether a text the last pass drew holds `text` (a dialog's rows are wrapped).
fn has(g: &Gx, text: &str) -> bool {
    g.fx.drv.texts().iter().any(|(_, _, t)| t.contains(text))
}

/// The commit dialog open with a message typed: the switch asks first, naming what it drops.
/// Cancel stays — the dialog and its text as they were; Switch lets it go, and only then.
#[test]
fn a_git_dialog_open_asks_before_the_switch() {
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
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    g.idle(2);
    assert!(has(&g, "is not carried to box"), "{:#?}", g.fx.drv.texts());
    assert!(has(&g, "a commit message"), "names what is dropped");
    assert!(g.fx.drv.find_text("[ Switch ]").is_some());
    g.fx.drv.click_text_on_top(&mut g.fx.b, "[ Cancel ]");
    g.idle(2);
    assert!(!has(&g, "is not carried to box"), "Cancel closes it");
    assert!(!g.fx.b.switch_due(), "and stays");
    assert!(has(&g, "half a message"), "the dialog as it was");
    // Typed text goes to the dialog again, not to a question.
    g.fx.typed(", and more");
    g.idle(2);
    assert!(has(&g, "half a message, and more"));
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "asked again");
    g.idle(2);
    // Under the question, typing reaches nothing behind it.
    g.fx.typed("x");
    g.idle(2);
    assert!(!has(&g, "and morex"), "the dialog behind takes no keys");
    g.fx.drv.click_text_on_top(&mut g.fx.b, "[ Switch ]");
    g.idle(1);
    assert!(g.fx.b.switch_due(), "Switch lets it go");
    assert_eq!(g.fx.b.before_switch("box"), Before::Go);
}

/// The planner open: its plan is what the switch drops; Cancel keeps it.
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
        has(&g, "The planner's unsaved plan is not carried to box"),
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
