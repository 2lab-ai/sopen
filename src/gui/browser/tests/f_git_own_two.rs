//! F-git-own-two's tests (`.prd/xtools/loop.md`): a read the git panel asked for that opens a
//! dialog when it lands — a quick edit's range, an action's prefill, a restore's entries, the
//! Backups list — waits in its channel while the panel's own dialog is up, and opens once the
//! owner closes it, with the keyboard (read again if a refresh came meanwhile); the pass that
//! hears it drops its keys (F-git-listq). A quick `split`'s load is heard, its Split's
//! confirmation waits; a run's question asked again waits, in order and bounded; the status row
//! follows the front dialog; nothing held polls. `GitView::open_edit` refuses to open over a
//! dialog. Each read is made slow by a channel of the test's own in its place, sent to only once
//! the owner's dialog is up.

use super::s2::{small, Gx};
use super::s3::{a_run_that_ended, other_repo, pressed, two_panels};
use super::*;
use crate::git::actions::{Action, ResetMode};
use crate::git::history::fixture::{self, Repo};
use crate::git::model::Oid;
use crate::git::rebase::{Edit, EditId, Quick};
use crate::git::runner::Handle;
use git_host::RunEnd;
use git_panel::Front;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

/// The fixture's git, or the test skips (fails under `SOPEN_REQUIRE_GIT=1`).
macro_rules! git_or_skip {
    () => {
        match fixture::git() {
            Some(git) => git,
            None => return,
        }
    };
}

/// `base`, `A`, `B`, `C` on `main`, each changing `c.txt`; a clean work tree.
fn linear(git: &std::path::Path) -> Repo {
    let repo = Repo::new(git);
    for (i, name) in ["base", "A", "B", "C"].into_iter().enumerate() {
        repo.write("c.txt", format!("{i}\n"));
        repo.commit(name);
    }
    repo
}

/// [`small`] with `a.txt` staged (Commit is offered on the uncommitted row), the window ready
/// with the keyboard in git and the bar on the uncommitted row.
fn staged(git: &std::path::Path) -> Gx {
    let repo = small(git);
    repo.run(&["add", "a.txt"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    let first = g.subjects()[0].clone();
    assert!(first.starts_with("Uncommitted changes"), "{first}");
    g.bar_to(&first);
    g
}

/// The read waiting in `slot`, taken from its worker once that has answered: what it read, and
/// the sender of a channel of the test's own put in its place — the panel hears the read only
/// once the test sends it. No pass is drawn meanwhile.
fn held<T>(slot: &mut Option<git_host::Pending<T>>) -> (T, mpsc::Sender<T>) {
    let real = slot.take().expect("a read asked for");
    let (tx, rx) = mpsc::channel();
    *slot = Some(git_host::Pending {
        rx,
        handle: Handle::new(),
    });
    let read = (real.rx)
        .recv_timeout(Duration::from_secs(20))
        .expect("the read answered within 20 s");
    (read, tx)
}

/// Whether the panel's dialog slot `kind` is up (`GitView::fronts`).
fn up(g: &Gx, kind: Front) -> bool {
    g.view().fronts()[kind as usize].0
}

/// The quick edit's draft while one is up: its commit and message, for a Reword.
fn reword(g: &Gx) -> Option<(Oid, String)> {
    match g.view().edit_draft()? {
        Edit::Quick(Quick::Reword { commit, message }) => Some((commit.clone(), message.clone())),
        _ => None,
    }
}

/// The action dialog's draft while one is up.
fn draft(g: &Gx) -> Option<Action> {
    g.view().draft().cloned()
}

/// A pass whose keys would act on what is heard in it: a letter (the graph's Find, a field's
/// text) and an Enter. (An Esc there would stop the read first: `git_stop_on_esc` runs before
/// anything is heard, 03 §3.4.)
fn letter_and_enter() -> Vec<Event> {
    let mut events = vec![Event::Text("x".into())];
    events.extend(pressed(Key::Enter));
    events
}

/// The defect: F2 on a commit asks for its range; Cmd/Ctrl+K opens the filters before it lands.
/// The range waits in its channel — no Reword over the filters, whose hint the status row keeps —
/// and once Esc closes them it opens on its commit with its message, the keyboard in it. The pass
/// that hears it drops its keys (F-git-listq): a letter and an Enter reach neither the graph nor
/// the Reword, which never runs on them; what is typed after it is in its message.
#[test]
fn f_git_own_two_a_reword_waits_for_the_owners_filters() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("B");
    let b = Oid::parse(&g.repo.rev("HEAD~1")).unwrap();
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F2), Modifiers::NONE);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].edit_open);
    g.cmd(Key::K);
    g.idle(2);
    assert!(up(&g, Front::Filters), "the owner's filters");
    tx.send(read).unwrap();
    g.idle(3);
    assert!(!up(&g, Front::Quick), "no Reword over the filters");
    assert!(up(&g, Front::Filters), "the filters stay");
    assert!(
        g.b().git.panels[i].edit_open.is_some(),
        "the range waits in its channel"
    );
    assert!(g.status().contains("Enter applies"), "{}", g.status());
    assert!(
        g.b().ready(),
        "a receipt's next key waits for no read held for the owner's dialog"
    );
    // Esc closes the filters; the range is heard from the next pass.
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert!(!up(&g, Front::Filters), "Esc closed the filters");
    assert!(!up(&g, Front::Quick), "heard from the next pass");
    g.fx.drv
        .frame(&mut g.fx.b, letter_and_enter(), Modifiers::NONE);
    assert!(up(&g, Front::Quick), "the Reword opened in that pass");
    g.idle(2);
    assert!(up(&g, Front::Quick), "and stays");
    assert_eq!(
        reword(&g).map(|(c, m)| (c, m.trim_end().to_string())),
        Some((b.clone(), "B".to_string())),
        "on its commit, its message read"
    );
    assert!(g.b().git.panels[i].edit_open.is_none(), "heard");
    assert_eq!(g.view().find_text(), None, "the letter started no Find");
    assert!(
        g.b().git.panels[i].running.is_empty(),
        "that Enter ran nothing"
    );
    assert_eq!(g.b().git_focused(), Some(i), "the keyboard in the panel");
    assert!(!g.status().contains("Enter applies"), "{}", g.status());
    g.fx.typed("y");
    g.idle(2);
    let message = reword(&g).map(|(_, m)| m).unwrap_or_default();
    assert!(
        message.contains('y') && message.contains('B') && !message.contains('x'),
        "typed into its message: {message:?}"
    );
    g.esc();
    assert!(!g.view().dialog_up(), "Esc closes it");
}

/// An action's prefill (F7's Commit on the uncommitted row) that lands while the owner's menu is
/// open waits in its channel; once Esc closes the menu its dialog opens with the keyboard, the
/// pass that hears it dropping a letter and an Enter, and what is typed after it is its message.
#[test]
fn f_git_own_two_a_prefill_waits_for_the_owners_menu() {
    let git = git_or_skip!();
    let mut g = staged(&git);
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F7), Modifiers::NONE);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].prefill);
    g.key(Key::F10);
    g.idle(2);
    assert!(up(&g, Front::Menu), "the owner's menu");
    tx.send(read).unwrap();
    g.idle(3);
    assert!(!up(&g, Front::Dialog), "no Commit dialog behind the menu");
    assert!(up(&g, Front::Menu), "the menu stays");
    assert!(
        g.b().git.panels[i].prefill.is_some(),
        "the prefill waits in its channel"
    );
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert!(!up(&g, Front::Menu), "Esc closed the menu");
    assert!(!up(&g, Front::Dialog), "heard from the next pass");
    g.fx.drv
        .frame(&mut g.fx.b, letter_and_enter(), Modifiers::NONE);
    assert!(
        up(&g, Front::Dialog),
        "the Commit dialog opened in that pass"
    );
    g.idle(2);
    assert!(
        matches!(draft(&g), Some(Action::Commit { message, .. }) if message.is_empty()),
        "the Commit dialog, as its prefill read it: {:?}",
        draft(&g)
    );
    assert!(g.b().git.panels[i].prefill.is_none(), "heard");
    assert!(
        g.b().git.panels[i].running.is_empty(),
        "that Enter ran nothing"
    );
    assert_eq!(g.b().git_focused(), Some(i), "the keyboard in the panel");
    g.fx.typed("msg");
    g.idle(2);
    assert!(
        matches!(draft(&g), Some(Action::Commit { message, .. }) if message == "msg"),
        "typed into its message: {:?}",
        draft(&g)
    );
    g.esc();
    assert!(!g.view().dialog_up(), "Esc closes it");
}

/// A prefill that lands while the owner types in another action dialog (Tag… on a commit) takes
/// neither its place nor its draft: it waits in its channel, and opens once that dialog closes.
#[test]
fn f_git_own_two_a_prefill_leaves_the_owners_dialog_in_its_place() {
    let git = git_or_skip!();
    let mut g = staged(&git);
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F7), Modifiers::NONE);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].prefill);
    g.bar_to("third");
    g.key(Key::F10);
    g.idle(2);
    g.key(Key::Enter);
    g.idle(2);
    g.fx.typed("v9");
    g.idle(2);
    let tag = |g: &Gx| matches!(draft(g), Some(Action::Tag { name, .. }) if name == "v9");
    assert!(tag(&g), "the owner's Tag dialog: {:?}", draft(&g));
    tx.send(read).unwrap();
    g.idle(3);
    assert!(tag(&g), "still the owner's, as typed: {:?}", draft(&g));
    assert!(
        g.b().git.panels[i].prefill.is_some(),
        "the prefill waits in its channel"
    );
    g.esc();
    g.idle(2);
    assert!(
        matches!(draft(&g), Some(Action::Commit { .. })),
        "the Commit dialog once the Tag's closed: {:?}",
        draft(&g)
    );
    assert_eq!(g.b().git_focused(), Some(i), "the keyboard in the panel");
    g.esc();
    assert!(!g.view().dialog_up());
}

/// A restore's entries and the Backups list read while the owner's filters are up wait in their
/// channels; each opens once Esc closes the filters.
#[test]
fn f_git_own_two_a_restore_and_the_backups_list_wait_for_the_owners_filters() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.cmd(Key::K);
    g.idle(2);
    assert!(up(&g, Front::Filters), "the owner's filters");
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(("20260101T000000Z".to_string(), Vec::new(), None)))
        .unwrap();
    g.fx.b.git.panels[i].restore_read = Some(git_host::Pending {
        rx,
        handle: Handle::new(),
    });
    g.idle(3);
    assert!(!up(&g, Front::Dialog), "no restore over the filters");
    assert!(g.b().git.panels[i].restore_read.is_some(), "in its channel");
    g.esc();
    g.idle(2);
    assert!(
        matches!(draft(&g), Some(Action::Restore { .. })),
        "the restore's dialog: {:?}",
        draft(&g)
    );
    g.esc();
    assert!(!g.view().dialog_up());
    // The Backups list, the same way.
    g.cmd(Key::K);
    g.idle(2);
    assert!(up(&g, Front::Filters), "the owner's filters again");
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(Vec::new())).unwrap();
    g.fx.b.git.panels[i].backups_read = Some(git_host::Pending {
        rx,
        handle: Handle::new(),
    });
    g.idle(3);
    assert!(
        g.view().backups().is_none(),
        "no Backups list under the filters"
    );
    assert!(g.b().git.panels[i].backups_read.is_some(), "in its channel");
    g.esc();
    g.idle(2);
    assert!(g.view().backups().is_some(), "the Backups list");
    assert!(g.b().git.panels[i].backups_read.is_none(), "heard");
}

/// The load after a quick `split` is heard while the owner's filters are up — the snapshot is
/// never held for the panel's own dialog — and only its Split's confirmation waits for them
/// (`split_due`), the window ready meanwhile; once they close it is asked for.
#[test]
fn f_git_own_two_a_splits_confirmation_waits_for_the_owners_filters() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.cmd(Key::K);
    g.idle(2);
    assert!(up(&g, Front::Filters), "the owner's filters");
    // As a quick `split`'s end leaves it: the Split's confirmation next, a refresh asked.
    g.fx.b.git.panels[i].split_next = true;
    let ctx = g.fx.drv.ctx.clone();
    g.fx.b.start_read(&ctx, i, true, 0);
    let real = g.fx.b.git.panels[i].read.take().expect("the load asked");
    let (tx, rx) = mpsc::channel();
    g.fx.b.git.panels[i].read = Some(git_host::SnapshotRead {
        soft: real.soft,
        pages: real.pages,
        pending: git_host::Pending {
            rx,
            handle: Handle::new(),
        },
    });
    let snapshot = (real.pending.rx)
        .recv_timeout(Duration::from_secs(20))
        .expect("the load answered");
    drop(real);
    tx.send(snapshot).unwrap();
    g.idle(3);
    let p = &g.b().git.panels[i];
    assert!(p.read.is_none() && !p.split_next, "the load heard at once");
    assert!(p.split_due, "its Split's confirmation waits");
    assert!(g.b().ready(), "a receipt's next key waits not for it");
    g.esc();
    g.idle(2);
    let p = &g.b().git.panels[i];
    assert!(!p.split_due, "asked for once the filters closed");
    // No edit stop here: the Split says so instead of opening.
    assert_eq!(
        g.status(),
        "Split: the edit stop is gone; nothing was split",
        "said at its release"
    );
}

/// The backstop: `GitView::open_edit` opens no quick edit over a dialog of the panel's — here
/// the owner's filters — and says so on the status row once they close.
#[test]
fn f_git_own_two_open_edit_refuses_over_a_dialog() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("B");
    let b = Oid::parse(&g.repo.rev("HEAD~1")).unwrap();
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F2), Modifiers::NONE);
    let (read, _tx) = held(&mut g.fx.b.git.panels[i].edit_open);
    let model = read.model.expect("the range read");
    g.cmd(Key::K);
    g.idle(2);
    assert!(up(&g, Front::Filters), "the owner's filters");
    let asked = g.panel_mut().view.open_edit(EditId::Reword, &b, &model);
    assert!(asked.is_empty(), "{asked:?}");
    assert!(!up(&g, Front::Quick), "no Reword over the filters");
    g.esc();
    g.idle(2);
    assert!(!g.view().dialog_up());
    assert!(
        g.status().starts_with("Reword: not opened"),
        "{}",
        g.status()
    );
}

// ---------------------------------------------------------------------------------------------
// A run's question asked again waits for the owner's dialog; the status row follows the front;
// nothing held polls.

/// The owner's Tag… on the commit `subject`, "v9" typed into it.
fn the_owners_tag(g: &mut Gx, subject: &str) {
    g.bar_to(subject);
    g.key(Key::F10);
    g.idle(2);
    g.key(Key::Enter);
    g.idle(2);
    g.fx.typed("v9");
    g.idle(2);
    assert!(tagging(g), "the owner's Tag dialog: {:?}", draft(g));
}

/// The owner's Tag dialog is up, "v9" as typed.
fn tagging(g: &Gx) -> bool {
    matches!(draft(g), Some(Action::Tag { name, .. }) if name == "v9")
}

/// Two runs' ends that ask for their confirmations again (`RunEnd::Confirm`: their shown plans
/// were lost) come while the owner types in Tag…: the ends are heard, the Tag stays as typed and
/// the status row says the first question waits. The Esc that closes the Tag lets it go (the
/// next pass is asked for) and the next single pass opens the first confirmation, the status row
/// naming the second; the Esc that closes the first lets the second go the same way, in order.
#[test]
fn f_git_own_two_a_runs_confirmation_waits_for_the_owners_dialog() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    the_owners_tag(&mut g, "third");
    let reset = |g: &Gx, rev: &str| Action::Reset {
        to: Oid::parse(&g.repo.rev(rev)).unwrap(),
        mode: ResetMode::Hard,
    };
    let (light, first) = (reset(&g, "light"), reset(&g, ":/^first"));
    let ends = [("Reset", &light), ("Reset to first", &first)].map(|(title, action)| {
        let end = RunEnd::Confirm {
            title: title.into(),
            action: Box::new(action.clone()),
        };
        a_run_that_ended(&mut g, i, end)
    });
    g.idle(3);
    assert!(g.b().git.panels[i].running.is_empty(), "their ends heard");
    assert!(tagging(&g), "the owner's Tag, as typed: {:?}", draft(&g));
    assert_eq!(
        g.status(),
        "Reset asks again: close the dialog to see it",
        "said while it waits"
    );
    // The Tag closes: the first question is let go, and opens in the next pass.
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert!(!g.view().dialog_up(), "Esc closed the Tag");
    assert!(
        g.b().git_busy(),
        "the next pass is asked for (the timed one)"
    );
    g.fx.drv.frame(&mut g.fx.b, Vec::new(), Modifiers::NONE);
    assert_eq!(
        draft(&g),
        Some(light),
        "the first confirmation, in that pass"
    );
    assert_eq!(g.b().git_focused(), Some(i), "the keyboard in the panel");
    g.idle(4);
    assert_eq!(
        g.status(),
        "Reset to first asks again: close the dialog to see it",
        "the second waits behind it"
    );
    // The first closes: the second, the same way.
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert!(!g.view().dialog_up(), "Esc closed the first");
    assert!(
        g.b().git_busy(),
        "the next pass is asked for (the timed one)"
    );
    g.fx.drv.frame(&mut g.fx.b, Vec::new(), Modifiers::NONE);
    assert_eq!(draft(&g), Some(first), "the second confirmation");
    g.idle(2);
    assert!(
        g.status().contains("Reset to first: not run"),
        "{}",
        g.status()
    );
    g.idle(2);
    g.esc();
    assert!(!g.view().dialog_up());
    drop(ends);
}

/// A run's question waiting behind the owner's filters is said on the status row before the
/// filters' own keys (it shows nowhere else); once Esc closes them it opens.
#[test]
fn f_git_own_two_a_question_behind_the_filters_is_said_first() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.cmd(Key::K);
    g.idle(2);
    assert!(up(&g, Front::Filters), "the owner's filters");
    let light = Action::Reset {
        to: Oid::parse(&g.repo.rev("light")).unwrap(),
        mode: ResetMode::Hard,
    };
    let end = RunEnd::Confirm {
        title: "Reset".into(),
        action: Box::new(light.clone()),
    };
    let tx = a_run_that_ended(&mut g, i, end);
    g.idle(3);
    assert!(up(&g, Front::Filters) && !up(&g, Front::Dialog), "it waits");
    assert_eq!(g.status(), "Reset asks again: close the dialog to see it");
    g.esc();
    g.idle(2);
    assert!(!up(&g, Front::Filters));
    assert_eq!(draft(&g), Some(light), "the confirmation once they closed");
    g.esc();
    drop(tx);
}

/// A history edit's confirmation asked again (`RunEnd::ConfirmEdit`) waits the same way: no
/// quick edit over the owner's Tag, which stays as typed; it opens once the Tag closes.
#[test]
fn f_git_own_two_a_history_edits_confirmation_waits_for_the_owners_dialog() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    the_owners_tag(&mut g, "C");
    let edit = Edit::Quick(Quick::Reword {
        commit: Oid::parse(&g.repo.rev("HEAD~1")).unwrap(),
        message: "B, again".into(),
    });
    let end = RunEnd::ConfirmEdit {
        title: "Reword".into(),
        edit: Box::new(edit.clone()),
    };
    let tx = a_run_that_ended(&mut g, i, end);
    g.idle(3);
    assert!(g.b().git.panels[i].running.is_empty(), "its end heard");
    assert!(!up(&g, Front::Quick), "no quick edit over the Tag");
    assert!(tagging(&g), "the owner's Tag, as typed: {:?}", draft(&g));
    assert_eq!(g.status(), "Reword asks again: close the dialog to see it");
    g.esc();
    g.idle(2);
    assert_eq!(g.view().edit_draft(), Some(&edit), "its confirmation");
    g.esc();
    assert!(!g.view().dialog_up());
    drop(tx);
}

/// "Delete anyway?" (03 §7.2, `GitView::confirm_again`) asked while the owner types in Tag…:
/// the Tag stays as typed, and the question opens once it closes.
#[test]
fn f_git_own_two_delete_anyway_waits_for_the_owners_dialog() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    the_owners_tag(&mut g, "third");
    let forced = Action::DeleteBranch {
        branch: "feature".into(),
        force: true,
        remotes: Vec::new(),
    };
    let asked = g.panel_mut().view.confirm_again(forced.clone());
    assert!(asked.is_empty(), "{asked:?}");
    g.idle(3);
    assert!(tagging(&g), "the owner's Tag, as typed: {:?}", draft(&g));
    assert!(
        g.status()
            .ends_with("asks again: close the dialog to see it"),
        "{}",
        g.status()
    );
    g.esc();
    g.idle(2);
    assert_eq!(
        draft(&g),
        Some(forced),
        "delete anyway?, once the Tag closed"
    );
    g.esc();
    assert!(!g.view().dialog_up());
}

/// The status row follows the front dialog: a run's result over the owner's filters shows the
/// run's words, not the filters' keys; once the result closes, the filters' keys again.
#[test]
fn f_git_own_two_the_status_row_follows_the_front_dialog() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.cmd(Key::K);
    g.idle(2);
    assert!(g.status().contains("Enter applies"), "{}", g.status());
    let end = RunEnd::NotRun {
        title: "Push".into(),
        why: "the remote refused it".into(),
        line: None,
    };
    let tx = a_run_that_ended(&mut g, i, end);
    g.idle(3);
    assert!(
        up(&g, Front::Result) && up(&g, Front::Filters),
        "a result over the filters"
    );
    assert_eq!(g.status(), "Push: not run", "the run's words");
    g.esc();
    g.idle(2);
    assert!(!up(&g, Front::Result) && up(&g, Front::Filters));
    assert!(g.status().contains("Enter applies"), "{}", g.status());
    drop(tx);
}

/// Every repaint the window asks for, by its delay ([`egui::Context::set_repaint_observer`]).
fn repaints(g: &Gx) -> Arc<Mutex<Vec<Duration>>> {
    let asked: Arc<Mutex<Vec<Duration>>> = Arc::default();
    let record = asked.clone();
    (g.fx.drv.ctx).set_repaint_observer(move |info| {
        record.lock().unwrap().push(info.delay);
    });
    asked
}

/// The timed repaints asked for since the last call (a delay, 100 ms or less: what polls). A
/// zero delay is no evidence here: egui asks for one after every pass with an event, and the
/// kit sends one each frame (`ModifiersChanged`).
fn taken(asked: &Arc<Mutex<Vec<Duration>>>) -> usize {
    let all = std::mem::take(&mut *asked.lock().unwrap());
    (all.iter())
        .filter(|d| **d > Duration::ZERO && **d <= Duration::from_millis(100))
        .count()
}

/// A prefill held behind the owner's menu asks for no timed repaint over 5 s of passes — no
/// more than the same menu with nothing held — and the pass whose Esc closes the menu, the read
/// held no more and so on its way, asks for the timed one (`Browser::git_busy`), where the
/// control's close asks for none; the pass after the close hears it: its dialog opens.
#[test]
fn f_git_own_two_a_held_read_polls_for_nothing_and_its_release_asks_for_a_pass() {
    let git = git_or_skip!();
    let mut g = staged(&git);
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let asked = repaints(&g);
    // The control: the menu, nothing held.
    g.key(Key::F10);
    g.idle(2);
    assert!(up(&g, Front::Menu));
    taken(&asked);
    g.idle(300);
    let control = taken(&asked);
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert_eq!(
        taken(&asked),
        0,
        "the control's close asks for no timed pass"
    );
    g.idle(2);
    assert!(!g.view().dialog_up());
    // F7's prefill held behind the menu.
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F7), Modifiers::NONE);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].prefill);
    g.key(Key::F10);
    g.idle(2);
    tx.send(read).unwrap();
    g.idle(2);
    assert!(g.b().git.panels[i].prefill.is_some(), "held");
    taken(&asked);
    g.idle(300);
    assert!(
        g.b().git.panels[i].prefill.is_some(),
        "still held after 5 s"
    );
    let holding = taken(&asked);
    assert_eq!(holding, control, "no timed repaint while it is held");
    // The close asks for the next pass; that pass hears it.
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    let close = taken(&asked);
    assert!(!up(&g, Front::Menu) && !up(&g, Front::Dialog));
    assert!(close >= 1, "the close asks for the timed pass: {close}");
    g.fx.drv.frame(&mut g.fx.b, Vec::new(), Modifiers::NONE);
    assert!(up(&g, Front::Dialog), "the pass after the close hears it");
    assert!(g.b().git.panels[i].prefill.is_none());
}

/// A read held for another panel's dialog (`git_waits`, X4-menu) polls for nothing either: a's
/// tag details wait while b's menu is open, with no timed repaint over 5 s of passes, and the
/// pass whose Esc closes b's menu asks for the timed one, which opens a's result.
#[test]
fn f_git_own_two_a_read_held_for_another_panel_polls_for_nothing() {
    let git = git_or_skip!();
    let b = other_repo(&git);
    let mut g = Gx::new(small(&git));
    let (ia, ib) = two_panels(&mut g, &b);
    let asked = repaints(&g);
    // The control: b's menu opened and closed, nothing held.
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu");
    taken(&asked);
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert!(!g.b().git.panels[ib].view.dialog_up());
    assert_eq!(
        taken(&asked),
        0,
        "the control's close asks for no timed pass"
    );
    g.idle(2);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu");
    let (tx, rx) = mpsc::channel();
    tx.send(("Tag v1.0".to_string(), Ok("tagged by Ann".to_string())))
        .unwrap();
    g.fx.b.git.panels[ia].text_read = Some(git_host::Pending {
        rx,
        handle: Handle::new(),
    });
    g.idle(2);
    taken(&asked);
    g.idle(300);
    assert!(g.b().git.panels[ia].text_read.is_some(), "held after 5 s");
    let holding = taken(&asked);
    assert_eq!(holding, 0, "no timed repaint while it is held");
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    let close = taken(&asked);
    assert!(!g.b().git.panels[ib].view.dialog_up(), "b's menu closed");
    assert!(close >= 1, "the close asks for the timed pass: {close}");
    g.fx.drv.frame(&mut g.fx.b, Vec::new(), Modifiers::NONE);
    assert!(g.b().git.panels[ia].view.dialog_up(), "a's result");
}

// ---------------------------------------------------------------------------------------------
// Round 1's review: the load is always heard (only the split's confirmation waits); a held read
// that turns stale is asked again; questions keep their order and their bound; the switch names
// a question that waits.

/// The Running of panel `i`'s first run, its end taken from its worker once that has answered,
/// and a channel of the test's own in its place: what it sent, and the sender.
fn held_end(g: &mut Gx, i: usize) -> (RunEnd, mpsc::Sender<RunEnd>) {
    let (tx, rx) = mpsc::channel();
    let run = (g.fx.b.git.panels[i].running.first_mut()).expect("a run started");
    let real = std::mem::replace(&mut run.rx, rx);
    let end = real
        .recv_timeout(Duration::from_secs(20))
        .expect("the run ended within 20 s");
    (end, tx)
}

/// A quick `split` whose run ends while the owner types in Tag…: the end is heard and the load
/// after it too — the snapshot, its banner and graph show the edit stop at once — while the
/// Split's confirmation waits for the Tag; once Esc closes it, the confirmation opens.
#[test]
fn f_git_own_two_a_splits_load_is_heard_under_the_owners_dialog() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("a.txt", "a\n");
    repo.commit("base");
    repo.write("a.txt", "a, changed\n");
    repo.write("new.txt", "new\n");
    repo.commit("both");
    repo.write("z.txt", "z\n");
    repo.commit("top");
    let mut g = Gx::new(repo);
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("both");
    g.menu("Split…");
    g.idle(2);
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Enter), Modifiers::NONE);
    let (end, tx) = held_end(&mut g, i);
    assert!(!g.shows("stopped to edit"), "not refreshed yet");
    the_owners_tag(&mut g, "top");
    tx.send(end).unwrap();
    g.until("the run and its load", move |b| {
        let p = &b.git.panels[i];
        p.running.is_empty() && p.read.is_none()
    });
    g.idle(2);
    assert!(
        g.shows("stopped to edit"),
        "the stop at once: {:#?}",
        g.texts()
    );
    assert!(tagging(&g), "the owner's Tag, as typed: {:?}", draft(&g));
    assert!(!up(&g, Front::Quick), "the Split's confirmation waits");
    g.esc();
    g.until_quiet("the Split's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::SplitStop { .. })),
        "the Split's confirmation once the Tag closed: {:?} {}",
        g.view().edit_draft(),
        g.status()
    );
}

/// The repository read meanwhile, and the snapshot of panel `i` with it.
fn refreshed(g: &mut Gx, i: usize) {
    let before = g.view().repo().map(|(_, n)| n);
    let ctx = g.fx.drv.ctx.clone();
    g.fx.b.start_read(&ctx, i, true, 0);
    g.until("the refresh", move |b| b.git.panels[i].read.is_none());
    assert_ne!(g.view().repo().map(|(_, n)| n), before, "a new snapshot");
}

/// A Reword's range held behind the owner's filters turns stale — a refresh came meanwhile —
/// and is asked again at its release, not refused: once Esc closes the filters, the Reword
/// opens on its commit.
#[test]
fn f_git_own_two_a_held_range_made_stale_is_asked_again() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("B");
    let b = Oid::parse(&g.repo.rev("HEAD~1")).unwrap();
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F2), Modifiers::NONE);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].edit_open);
    g.cmd(Key::K);
    g.idle(2);
    tx.send(read).unwrap();
    g.idle(2);
    refreshed(&mut g, i);
    assert!(g.b().git.panels[i].edit_open.is_some(), "still held");
    g.esc();
    g.until_quiet("the range again");
    assert_eq!(
        reword(&g).map(|(c, _)| c),
        Some(b),
        "the Reword, its range read again: {}",
        g.status()
    );
    g.esc();
}

/// The same, with the commit gone meanwhile (the history rewritten under it): the range read
/// again no longer holds it, and the status row says so.
#[test]
fn f_git_own_two_a_held_range_whose_commit_went_says_so() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("B");
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F2), Modifiers::NONE);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].edit_open);
    g.cmd(Key::K);
    g.idle(2);
    tx.send(read).unwrap();
    g.idle(2);
    g.repo.run(&["reset", "-q", "--hard", "HEAD~2"]);
    g.repo.write("c.txt", "d\n");
    g.repo.commit("D");
    refreshed(&mut g, i);
    g.esc();
    g.until_quiet("the range again");
    assert!(!up(&g, Front::Quick), "no Reword");
    assert!(
        g.status().contains("is not in the range read"),
        "{}",
        g.status()
    );
}

/// User…'s prefill (the repository's own name and email) held behind the owner's menu turns
/// stale — the name changed and the repository was read meanwhile — and is read again at its
/// release: the dialog opens on the name as it is now, never the one read before.
#[test]
fn f_git_own_two_a_held_prefill_made_stale_is_read_again() {
    let git = git_or_skip!();
    let mut g = staged(&git);
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.repo.run(&["config", "user.name", "Ann"]);
    g.repo.run(&["config", "user.email", "ann@example.com"]);
    let asked = (g.panel_mut().view).open_by_id(crate::gui::DialogId::Action(
        crate::git::actions::ActionId::User,
    ));
    let ctx = g.fx.drv.ctx.clone();
    g.fx.b.git_actions(&ctx, i, asked);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].prefill);
    g.key(Key::F10);
    g.idle(2);
    assert!(up(&g, Front::Menu), "the owner's menu");
    tx.send(read).unwrap();
    g.idle(2);
    g.repo.run(&["config", "user.name", "Bea"]);
    refreshed(&mut g, i);
    g.esc();
    g.until_quiet("the prefill again");
    assert!(
        matches!(draft(&g), Some(Action::User { name, .. }) if name == "Bea"),
        "the name as it is now: {:?}",
        draft(&g)
    );
    g.esc();
}

/// A question that waited opens before a fresh one: A waits behind the owner's Tag; B's run ends
/// in the very poll that lets A go, as the Tag closes. A opens first, B waits behind it.
#[test]
fn f_git_own_two_a_fresh_question_waits_behind_an_older_one() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    the_owners_tag(&mut g, "third");
    let reset = |g: &Gx, rev: &str| Action::Reset {
        to: Oid::parse(&g.repo.rev(rev)).unwrap(),
        mode: ResetMode::Hard,
    };
    let (a, b) = (reset(&g, "light"), reset(&g, ":/^first"));
    let end = |title: &str, action: &Action| RunEnd::Confirm {
        title: title.into(),
        action: Box::new(action.clone()),
    };
    let tx_a = a_run_that_ended(&mut g, i, end("A", &a));
    g.idle(3);
    assert_eq!(g.status(), "A asks again: close the dialog to see it");
    let (_, tx_b) = super::s3::a_run_that_runs(&mut g, i);
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert!(!g.view().dialog_up(), "Esc closed the Tag");
    tx_b.send(end("B", &b)).unwrap();
    g.fx.drv.frame(&mut g.fx.b, Vec::new(), Modifiers::NONE);
    assert_eq!(draft(&g), Some(a), "A first");
    assert_eq!(g.status(), "B asks again: close the dialog to see it");
    drop((tx_a, tx_b));
}

/// The questions waiting are bounded as the results are (8): past them the oldest goes, and the
/// status row counts what went.
#[test]
fn f_git_own_two_the_questions_waiting_are_bounded() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    the_owners_tag(&mut g, "third");
    let light = Action::Reset {
        to: Oid::parse(&g.repo.rev("light")).unwrap(),
        mode: ResetMode::Hard,
    };
    for k in 1..=10 {
        let asked = (g.panel_mut().view).reconfirm(&format!("Reset {k}"), light.clone());
        assert!(asked.is_empty());
    }
    g.idle(2);
    assert_eq!(
        g.status(),
        "Reset 3 asks again: close the dialog to see it · 2 earlier questions dropped"
    );
}

/// A question waiting behind the owner's dialog is not carried by F12's switch (the state holds
/// no question): the switch asks first and names it.
#[test]
fn f_git_own_two_the_switch_names_a_waiting_question() {
    let git = git_or_skip!();
    let mut g = Gx::new(small(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    the_owners_tag(&mut g, "third");
    let light = Action::Reset {
        to: Oid::parse(&g.repo.rev("light")).unwrap(),
        mode: ResetMode::Hard,
    };
    let end = RunEnd::Confirm {
        title: "Reset".into(),
        action: Box::new(light),
    };
    let tx = a_run_that_ended(&mut g, i, end);
    g.idle(3);
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    let said: Vec<String> = g.texts();
    let line = "The question Reset asks again is not carried to box: switching drops it.";
    assert!(said.iter().any(|t| t.contains(line)), "{said:#?}");
    drop(tx);
}

/// The planner's range lands while the owner reads a command log entry shown whole (Cmd/Ctrl+L,
/// Enter): the entry, a dialog of the body, stays — the range waits for it, as the Backups list
/// does — and once Esc closes the entry the planner opens in the body in the log's place, with
/// the keyboard: a typed letter sets its verb.
#[test]
fn f_git_own_two_the_planner_waits_for_a_log_entry_and_takes_the_keys() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("base");
    let cmd_e = [true, false].map(|pressed| Event::Key {
        key: Key::E,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    });
    g.fx.drv
        .frame(&mut g.fx.b, cmd_e.to_vec(), Modifiers::COMMAND);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].edit_open);
    g.cmd(Key::L);
    g.idle(2);
    g.key(Key::Enter);
    g.idle(2);
    assert!(up(&g, Front::Entry), "the entry shown whole");
    tx.send(read).unwrap();
    g.idle(3);
    assert!(up(&g, Front::Entry), "the owner's entry stays");
    assert!(g.view().planner_model().is_none(), "no planner over it yet");
    g.esc();
    g.idle(3);
    assert!(!up(&g, Front::Entry), "Esc closed the entry");
    assert!(
        g.view().planner_model().is_some(),
        "the planner, its range in"
    );
    g.fx.typed("d");
    g.idle(2);
    let verb = g.view().planner_model().map(|m| m.rows[0].verb.word());
    assert_eq!(verb, Some("drop"), "a typed letter reaches the planner");
}

/// A read the owner asked for, held behind their dialog — F2's range behind the filters — is not
/// carried by F12's switch (no state holds a read): the switch asks first and names it.
#[test]
fn f_git_own_two_the_switch_names_a_held_read() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("B");
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F2), Modifiers::NONE);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].edit_open);
    g.cmd(Key::K);
    g.idle(2);
    tx.send(read).unwrap();
    g.idle(2);
    assert!(g.b().git.panels[i].edit_open.is_some(), "held");
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    let said: Vec<String> = g.texts();
    let line = "The Reword asked for is not carried to box: switching drops it.";
    assert!(said.iter().any(|t| t.contains(line)), "{said:#?}");
}

/// Two of a's things the owner asked for wait for b's menu (another panel's dialog, X4-menu):
/// the load after a's quick `split` (`split_next`, its confirmation not yet due) and a tag's
/// details. F12's switch drops both, and names each, the split in the words its confirmation
/// has once due.
#[test]
fn f_git_own_two_the_switch_names_a_splits_held_load() {
    let git = git_or_skip!();
    let b = other_repo(&git);
    let mut g = Gx::new(small(&git));
    let (ia, ib) = two_panels(&mut g, &b);
    g.key(Key::F10);
    g.idle(2);
    assert!(g.b().git.panels[ib].view.dialog_up(), "b's menu");
    g.fx.b.git.panels[ia].split_next = true;
    let ctx = g.fx.drv.ctx.clone();
    g.fx.b.start_read(&ctx, ia, true, 0);
    let (tx, rx) = mpsc::channel();
    tx.send(("Tag v1.0".to_string(), Ok("tagged by Ann".to_string())))
        .unwrap();
    g.fx.b.git.panels[ia].text_read = Some(git_host::Pending {
        rx,
        handle: Handle::new(),
    });
    g.idle(3);
    let p = &g.b().git.panels[ia];
    assert!(
        p.read.is_some() && p.split_next && !p.split_due,
        "the load held"
    );
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    let said = g.texts().join(" ");
    let line = "The tag details asked for and the Split's confirmation asked for are not carried \
                to box: switching drops them.";
    assert!(said.contains(line), "{said}");
}

/// A quick `split` done with its backup incomplete: its end puts the warning's result up, and
/// the Split's confirmation opens in front of it as ever — a result holds no question
/// (`GitView::holds_questions`) — the result waiting behind it.
#[test]
fn f_git_own_two_a_splits_confirmation_opens_over_its_warning() {
    let git = git_or_skip!();
    let repo = Repo::new(&git);
    repo.write("a.txt", "a\n");
    repo.commit("base");
    repo.write("a.txt", "a, changed\n");
    repo.write("new.txt", "new\n");
    repo.commit("both");
    repo.write("z.txt", "z\n");
    repo.commit("top");
    let mut g = Gx::new(repo);
    g.fx.drv.resize(1000.0, 1400.0);
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("both");
    g.menu("Split…");
    g.idle(2);
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Enter), Modifiers::NONE);
    let (mut end, tx) = held_end(&mut g, i);
    // Its backup's `after` record could not be written: done, with the warning.
    let RunEnd::Edited { outcome, .. } = &mut end else {
        panic!("the split's run ended otherwise")
    };
    let step = (outcome.steps.iter_mut())
        .find(|s| !s.ran.is_empty())
        .expect("a step that ran");
    let mut d = step.ran[0].clone();
    d.ending = crate::git::runner::Ending::NotSent("backup 1 incomplete: the test's".into());
    step.ran.push(d);
    assert!(outcome.warning().is_some());
    tx.send(end).unwrap();
    g.until_quiet("the run, its load and the Split's reads");
    assert!(
        matches!(g.view().edit_draft(), Some(Edit::SplitStop { .. })),
        "the Split's confirmation: {:?} {}",
        g.view().edit_draft(),
        g.status()
    );
    assert!(up(&g, Front::Result), "the warning's result behind it");
}

/// A restore's entries held behind the owner's filters turn stale — a refresh came meanwhile —
/// and are read again at their release, as a prefill's are: the release pass asks for them anew.
#[test]
fn f_git_own_two_a_held_restore_made_stale_is_read_again() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    let ctx = g.fx.drv.ctx.clone();
    let restore = git_panel::GitAction::Restore {
        stamp: "20260101T000000Z".into(),
        only: None,
    };
    g.fx.b.git_actions(&ctx, i, vec![restore]);
    let (read, tx) = held(&mut g.fx.b.git.panels[i].restore_read);
    g.cmd(Key::K);
    g.idle(2);
    tx.send(read).unwrap();
    g.idle(2);
    refreshed(&mut g, i);
    assert!(g.b().git.panels[i].restore_read.is_some(), "still held");
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::Escape), Modifiers::NONE);
    assert!(!up(&g, Front::Filters));
    g.fx.drv.frame(&mut g.fx.b, Vec::new(), Modifiers::NONE);
    assert!(
        g.b().git.panels[i].restore_read.is_some() && !up(&g, Front::Result),
        "asked again, nothing opened on the stale read"
    );
    g.until_quiet("the restore again");
    assert!(
        g.shows("no backup group 20260101T000000Z"),
        "{:#?}",
        g.texts()
    );
}

/// A read on its way when F12 comes — F2's range, no dialog up — is dropped by the switch as a
/// held one is, so the question names it too (after any question it re-decides, as after the
/// viewer's Discard, the same).
#[test]
fn f_git_own_two_the_switch_names_a_read_on_its_way() {
    let git = git_or_skip!();
    let mut g = Gx::new(linear(&git));
    g.ready_in_git();
    let i = g.b().git.panel(g.repo.top()).expect("the panel");
    g.bar_to("B");
    g.fx.drv
        .frame(&mut g.fx.b, pressed(Key::F2), Modifiers::NONE);
    let (_read, _tx) = held(&mut g.fx.b.git.panels[i].edit_open);
    assert!(
        !g.view().dialog_up(),
        "no dialog up: the read is on its way"
    );
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    let said = g.texts().join(" ");
    let line = "The Reword asked for is not carried to box: switching drops it.";
    assert!(said.contains(line), "{said}");
}
