//! X4-state's round trips (10 §17's row, §7.4's v2): a window's git panel taken into the switch's
//! state, its paths out as §7.3 rewrites them (this machine's `/abs` → `<this machine>:/abs`),
//! one line, read as HOST reads it, back to this machine's paths, and given to a fresh window on
//! the same repository, as HOST's switched window is — which shows it as it was; its state taken
//! again and given to a third, as the return to the terminal does, shows it once more. The bar on
//! a commit with two rows marked, the details pane and a filter; Find typed into; the Commit
//! dialog, the filters' and the history search typed into; a plan with a squash row; from v3
//! (X4-state2) Reword's, Squash's and Author's dialogs and the planner's message dialog typed
//! into. Then what the receiving host refuses or lacks: a commit it has not (said, and the window
//! settles), a plan that is not its range (the planner as read), Enters while the Commit or the
//! Reword dialog comes back (nothing runs).

use super::s2::{small, Gx};
use super::*;
use crate::git::history::fixture;
use crate::gui::browser::switch_state::{
    self, GitChoice, GitDialog, GitRow, GitTarget, GitText, SwitchState,
};
use crate::gui::{cells, term};
use std::time::{Duration, Instant};

/// `state` across: out as §7.3 rewrites SOURCE, one line, read as HOST reads it, back to this
/// machine's paths.
fn across(state: &SwitchState) -> SwitchState {
    let out = state
        .rewritten(|s| {
            crate::forward::source_for("laptop", s, Some("mac.local"), |p| Ok(p.to_string()))
        })
        .expect("out");
    assert_eq!(
        out.host.as_deref(),
        Some("mac"),
        "this machine's, as HOST names it"
    );
    let line = out.line();
    assert!(
        line.starts_with(r#"{"v":3,"#) && !line.contains('\n'),
        "{line}"
    );
    let read = switch_state::read(&line, "laptop").expect("HOST reads it");
    read.rewritten(|s| match s.strip_prefix("mac:") {
        Some(abs) => Ok(abs.to_string()),
        None => anyhow::bail!("not this machine's: {s}"),
    })
    .expect("back")
}

/// A fresh window on `g`'s repository, its git the fixture's own, given `state` before its first
/// pass as HOST's switched window is; then passes until it is ready.
fn given(g: &Gx, state: SwitchState) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let local = g.repo.local();
    let git: GitConnect = Arc::new(move || Ok(GitRunner::local(local.clone())));
    let cache = tmp.path().join("cache");
    let launched: Launched = Arc::default();
    let record = launched.clone();
    let b = Browser::new(Start {
        fs: Box::new(LocalFs::new()),
        connect: local_connect(),
        label: None,
        destination: None,
        abs: g.top(),
        cache: cache.clone(),
        state_dir: tmp.path().join("state"),
        launch: Arc::new(move |abs: &str, is_dir: bool| {
            record.lock().unwrap().push((abs.to_string(), is_dir));
            Ok(format!("opened {abs}"))
        }),
        git: Some(git),
    });
    let mut fx = Fx {
        _tmp: tmp,
        root: g.top(),
        cache,
        drv: Driver::new(1000.0, 700.0),
        b,
        launched,
    };
    fx.b.give_state(state).expect("the state is this machine's");
    settle(&mut fx);
    fx
}

/// Passes until the window is ready (git's workers take real time), then two more.
fn settle(fx: &mut Fx) {
    let t = Instant::now();
    while !fx.b.ready() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "not ready within 20 s; status {:?}",
            fx.b.status_line().text
        );
        fx.drv.frame(&mut fx.b, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    fx.drv.idle(&mut fx.b, 2);
}

/// The `SOPEN_CELLS` grid of one more pass, its rows but the status row: a dialog given back
/// opens afresh, so its caret is where an opened one's is, not where the owner left it; the
/// status row says what the last key did (`compare takes exactly one marked row`), which the
/// state does not carry; the foot's free bytes left out (other writers move them).
fn grid(fx: &mut Fx) -> String {
    for p in &mut fx.b.panels {
        p.free = None;
    }
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 700.0))),
        time: Some(10_000.0),
        max_texture_side: Some(8192),
        focused: true,
        ..Default::default()
    };
    let (b, mut pass) = (&mut fx.b, None);
    let mut out = fx.drv.ctx.run_ui(raw, |ui| {
        cells::clear(ui.ctx());
        b.ui(ui);
        pass = Some(term::keep_window_pass(ui.ctx()));
    });
    out.textures_delta.clear();
    let receipt = term::window_receipt(&fx.drv.ctx, &pass.expect("the pass ran"));
    let rows = receipt.lines().skip(1);
    let mut rows: Vec<&str> = rows.take_while(|l| !l.starts_with("attributes ")).collect();
    rows.pop();
    rows.join("\n")
}

fn same(taken: &str, given: &str, what: &str) {
    assert!(
        taken == given,
        "{what}: the same rows\n--- taken\n{taken}\n--- given\n{given}"
    );
}

/// `g`'s state across to a fresh window (HOST's), which shows it as it was and holds it as it
/// was; then that window's state back to a third (the terminal's return), the same once more.
fn round_trip(g: &mut Gx, what: &str) -> (SwitchState, Fx) {
    let before = grid(&mut g.fx);
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    assert_eq!(grid(&mut g.fx), before, "{what}: taking changes nothing");
    let mut host = given(g, across(&state));
    same(&before, &grid(&mut host), what);
    assert_eq!(
        host.b.take_state(&host.drv.ctx),
        state,
        "{what}: HOST holds it as it was"
    );
    let back = host.b.take_state(&host.drv.ctx);
    let mut home = given(g, across(&back));
    same(&before, &grid(&mut home), &format!("{what}, back"));
    (state, home)
}

/// The repository of `small` with `a.txt` staged, its window ready with the keyboard in git.
fn ready() -> Option<Gx> {
    let git = fixture::git()?;
    let repo = small(&git);
    repo.run(&["add", "a.txt"]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    Some(g)
}

/// `base` and three commits on it, the window ready with the keyboard in git.
fn linear() -> Option<Gx> {
    let git = fixture::git()?;
    let repo = fixture::Repo::new(&git);
    for (i, name) in ["base", "A", "B", "C"].into_iter().enumerate() {
        repo.write("c.txt", format!("{i}\n"));
        repo.commit(name);
    }
    let mut g = Gx::new(repo);
    g.ready_in_git();
    Some(g)
}

/// [`linear`] with the planner open above `base`.
fn planning() -> Option<Gx> {
    let mut g = linear()?;
    g.bar_to("base");
    g.cmd(Key::E);
    g.idle(2);
    g.until_quiet("the planner's range");
    assert!(g.panel().view.planner_model().is_some(), "the planner");
    Some(g)
}

/// The bar on a commit with two rows marked, the details pane open on the bar's commit with the
/// keyboard, and a filter set (Tags off): HOST's window shows them, its close brings them back.
#[test]
fn the_bar_marks_details_and_filters_cross() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.cmd(Key::K);
    g.until_quiet("the filters");
    // From the field, past the list, to Tags (no remotes here).
    for _ in 0..2 {
        g.key(Key::Tab);
    }
    g.key(Key::Space);
    g.key(Key::Enter);
    g.until_quiet("the graph with the filter");
    assert!(!g.panel().view.filters().tags, "Tags off");
    g.bar_to("second");
    g.key(Key::Space);
    g.key(Key::Space);
    g.bar_to("first");
    g.key(Key::Enter);
    g.until_quiet("the details");
    let (state, home) = round_trip(&mut g, "the view");
    let part = &state.git[0];
    assert!(matches!(&part.bar, Some(GitRow::Commit(_))), "{part:?}");
    assert_eq!(part.marks.len(), 2, "{part:?}");
    assert!(part.details.as_ref().is_some_and(|d| d.keyboard));
    assert!(part.filters.as_ref().is_some_and(|f| !f.tags));
    let i = home.b.git.panel(&g.top()).expect("its panel");
    assert!(home.b.git.panels[i].view.details_open());
}

/// Find typed into: open with its text, the bar where it was.
#[test]
fn find_typed_into_crosses() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("first");
    g.fx.typed("sec");
    g.until_quiet("find");
    assert_eq!(g.panel().view.find_text(), Some("sec"));
    let (state, home) = round_trip(&mut g, "Find");
    let find = state.git[0]
        .find
        .as_ref()
        .map(|f| (f.text.as_str(), f.case));
    assert_eq!(find, Some(("sec", false)));
    let i = home.b.git.panel(&g.top()).unwrap();
    assert_eq!(home.b.git.panels[i].view.find_text(), Some("sec"));
}

/// The Commit dialog (F7 on the uncommitted row) with a message typed and Skip hooks on: HOST
/// opens it as F7 does, on its own uncommitted row, the message in it — Hangul and a second line,
/// byte for byte — and Skip hooks chosen.
#[test]
fn the_commit_dialog_typed_into_crosses() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.f(Key::F7);
    let message = "반쯤 쓴 message\r\nand a line";
    g.fx.drv.frame(
        &mut g.fx.b,
        vec![Event::Text(message.into())],
        Modifiers::NONE,
    );
    g.until_quiet("the typing");
    // Past Amend to Skip hooks, on.
    g.key(Key::Tab);
    g.key(Key::Tab);
    g.key(Key::Space);
    g.until_quiet("Skip hooks");
    let (state, home) = round_trip(&mut g, "the Commit dialog");
    match &state.git[0].dialog {
        Some(GitDialog::Action {
            id, texts, choices, ..
        }) => {
            assert_eq!(id, "commit");
            let texts: Vec<_> = texts.iter().map(|t| (t.field, t.text.as_str())).collect();
            assert_eq!(texts, [(0, message)]);
            assert_eq!(choices[0].chosen, ["Skip hooks"], "{choices:?}");
        }
        other => panic!("the Commit dialog: {other:?}"),
    }
    let i = home.b.git.panel(&g.top()).unwrap();
    let draft = home.b.git.panels[i].view.draft().cloned();
    assert!(
        matches!(
            &draft,
            Some(crate::git::actions::Action::Commit { message: m, no_verify: true, amend: false })
                if m == message
        ),
        "byte for byte, Skip hooks on: {draft:?}"
    );
}

/// Amend's dialog with `edit it` chosen — the choice that shows its message field — and a new
/// message typed there: HOST opens Amend on its own last commit, the choice first, then the field
/// it shows, with the message.
#[test]
fn a_choice_that_shows_a_field_crosses_before_its_text() {
    let Some(g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    state.git[0].dialog = Some(GitDialog::Action {
        id: "amend".into(),
        on: GitTarget::Uncommitted,
        texts: vec![GitText {
            field: 0,
            text: "amended over there".into(),
        }],
        choices: vec![GitChoice {
            row: 0,
            chosen: vec!["edit it".into()],
        }],
    });
    let host = given(&g, across(&state));
    let i = host.b.git.panel(&g.top()).unwrap();
    let draft = host.b.git.panels[i].view.draft().cloned();
    assert!(
        matches!(&draft, Some(crate::git::actions::Action::Amend { message: Some(m) }) if m == "amended over there"),
        "{draft:?}"
    );
}

/// The filters' dialog with a choice not applied yet and the branches' type-to-filter text.
#[test]
fn the_filters_dialog_typed_into_crosses() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.cmd(Key::K);
    g.until_quiet("the filters");
    g.fx.typed("feat");
    for _ in 0..2 {
        g.key(Key::Tab);
    }
    g.key(Key::Space);
    g.until_quiet("the filters' choices");
    let (state, _home) = round_trip(&mut g, "the filters' dialog");
    match &state.git[0].dialog {
        Some(GitDialog::Filters { filters, typed }) => {
            assert_eq!(typed, "feat");
            assert!(!filters.tags, "not applied yet, carried");
        }
        other => panic!("the filters' dialog: {other:?}"),
    }
    assert!(
        state.git[0].filters.is_none(),
        "the applied ones are git's defaults"
    );
}

/// The history search with two of its fields typed into and Match case on.
#[test]
fn the_history_search_typed_into_crosses() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.with(Key::F, Modifiers::COMMAND | Modifiers::SHIFT);
    g.until_quiet("the history search");
    g.fx.typed("second");
    g.key(Key::Tab);
    g.fx.typed("Z");
    g.until_quiet("the search's fields");
    let (state, _home) = round_trip(&mut g, "the history search");
    match &state.git[0].dialog {
        Some(GitDialog::Search { fields, case }) => {
            assert_eq!(fields[..2], ["second".to_string(), "Z".to_string()]);
            assert!(!case);
        }
        other => panic!("the history search: {other:?}"),
    }
}

/// A plan with a row moved, a reword with its message typed, an author changed and a squash
/// row: the switch asks first (the plan goes with it), and HOST's planner, opened on its own range,
/// holds the same rows in the same order — verbs, message and author.
#[test]
fn a_plan_with_a_squash_row_crosses() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    // C (the top row) one down: B, C, A.
    g.with(Key::ArrowDown, Modifiers::COMMAND);
    g.until_quiet("the move");
    g.fx.typed("r");
    g.key(Key::Enter);
    g.fx.typed(" edited");
    g.key(Key::Enter);
    g.until_quiet("C's message");
    g.key(Key::ArrowDown);
    g.fx.typed("a");
    g.fx.typed("e");
    g.key(Key::Enter);
    g.until_quiet("A's author");
    g.key(Key::Home);
    g.fx.typed("s");
    g.until_quiet("the squash row");
    type Plan = Vec<(String, &'static str, Option<String>, Option<String>)>;
    let plan = |b: &Browser, top: &str| -> Plan {
        let i = b.git.panel(top).unwrap();
        let model = b.git.panels[i].view.planner_model().expect("the planner");
        (model.rows.iter())
            .map(|r| {
                let author = r.author.as_ref().map(|a| a.name.clone());
                (
                    r.commit.row.subject.clone(),
                    r.verb.word(),
                    r.message.clone(),
                    author,
                )
            })
            .collect()
    };
    let planned = plan(&g.fx.b, &g.top());
    let want: Plan = vec![
        ("B".into(), "squash", None, None),
        // `%B` ends in its newline: the typing goes after it.
        ("C".into(), "reword", Some("C\n edited".into()), None),
        ("A".into(), "pick", None, Some("Anne".into())),
    ];
    assert_eq!(planned, want);
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    assert!(g.painted("The planner's unsaved plan goes with the switch to box."));
    g.fx.drv.click_text_on_top(&mut g.fx.b, "[ Cancel ]");
    g.idle(2);
    let (state, home) = round_trip(&mut g, "the plan");
    assert!(state.git[0].plan.is_some());
    assert_eq!(plan(&home.b, &g.top()), planned);
}

/// A bar on a commit this host has not (a full id git does not know): the window settles, the
/// bar where its graph puts it, and the status row says so.
#[test]
fn a_commit_this_host_lacks_is_said_and_the_window_settles() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    state.git[0].bar = Some(GitRow::Commit("0123456789".repeat(4)));
    let host = given(&g, across(&state));
    let said = host.b.status_line().text;
    assert!(said.ends_with(": the bar's row is not here"), "{said}");
    g.idle(1);
}

/// A plan whose commits are not the range HOST reads above its base (its base moved up a row,
/// every commit known here): the planner opens as read there, and the status row says why.
#[test]
fn a_plan_that_is_not_the_hosts_range_leaves_the_planner_as_read() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.fx.typed("d");
    g.until_quiet("a drop row");
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    // Above A, this host's range is C and B; the plan's rows are C, B and A.
    let plan = state.git[0].plan.as_mut().unwrap();
    plan.base = Some(plan.rows[2].commit.clone());
    let host = given(&g, across(&state));
    let i = host.b.git.panel(&g.top()).unwrap();
    let model = host.b.git.panels[i]
        .view
        .planner_model()
        .expect("the planner");
    assert!(
        model.rows.iter().all(|r| r.verb.word() == "pick"),
        "as read"
    );
    let said = host.b.status_line().text;
    assert!(
        said.contains("its commits are not the range read here"),
        "{said}"
    );
}

/// Enters, typed text and a Space — several in one pass — in every pass while HOST's Commit
/// dialog comes back and in the pass its message goes in: nothing is committed, and the dialog
/// stays, its message exactly as typed there.
#[test]
fn enters_while_the_commit_dialog_comes_back_run_nothing() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.f(Key::F7);
    g.fx.typed("kept");
    g.until_quiet("the typing");
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let head = g.repo.run(&["rev-parse", "HEAD"]);
    let tmp = tempfile::tempdir().unwrap();
    let local = g.repo.local();
    let git: GitConnect = Arc::new(move || Ok(GitRunner::local(local.clone())));
    let mut host = Fx {
        root: g.top(),
        cache: tmp.path().join("cache"),
        drv: Driver::new(1000.0, 700.0),
        b: Browser::new(Start {
            fs: Box::new(LocalFs::new()),
            connect: local_connect(),
            label: None,
            destination: None,
            abs: g.top(),
            cache: tmp.path().join("cache"),
            state_dir: tmp.path().join("state"),
            launch: Arc::new(|abs: &str, _| Ok(format!("opened {abs}"))),
            git: Some(git),
        }),
        launched: Arc::default(),
        _tmp: tmp,
    };
    host.b.give_state(across(&state)).unwrap();
    let enter = || Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    // From the pass its prefill is asked for (the dialog coming back) to the pass its message
    // goes in, two Enters a pass; after it, the dialog's own rule (only what the screen showed
    // runs) is X4-dialogs'.
    let top = g.top();
    let draft = |b: &Browser| {
        let i = b.git.panel(&top)?;
        let p = &b.git.panels[i];
        Some((p.prefill.is_some(), p.view.draft().cloned()))
    };
    // Given: its message is in (the dialog opens on an empty one, this repository having no
    // prepared message).
    let given = |b: &Browser| matches!(draft(b), Some((_, Some(crate::git::actions::Action::Commit { message, .. }))) if !message.is_empty());
    let space = || Event::Key {
        key: Key::Space,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let (t, mut pressed) = (Instant::now(), 0);
    while !given(&host.b) {
        assert!(t.elapsed() < Duration::from_secs(20), "given within 20 s");
        let coming = draft(&host.b).is_some_and(|(prefill, d)| prefill || d.is_some());
        let events = match coming {
            true => vec![
                enter(),
                Event::Text("x".into()),
                space(),
                Event::Text(" ".into()),
                enter(),
            ],
            false => Vec::new(),
        };
        pressed += events.len();
        host.drv.frame(&mut host.b, events, Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(pressed >= 10, "keys while it came back: {pressed}");
    settle(&mut host);
    let i = host.b.git.panel(&g.top()).unwrap();
    let p = &host.b.git.panels[i];
    assert!(p.running.is_empty(), "nothing runs");
    let draft = p.view.draft().cloned();
    assert!(
        matches!(&draft, Some(crate::git::actions::Action::Commit { message, .. }) if message == "kept"),
        "{draft:?}"
    );
    assert_eq!(
        g.repo.run(&["rev-parse", "HEAD"]),
        head,
        "nothing committed"
    );
}

/// What a line names that would act at once on HOST opens nothing there: a view of the menu's
/// (the editor: nothing launched) and an action with no options whose plan asks nothing
/// (Unstage on `a.txt`, which stays staged); the status row says why.
#[test]
fn what_would_act_at_once_is_not_opened() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let base = g.fx.b.take_state(&g.fx.drv.ctx);
    let cases = [
        (
            "editor",
            GitTarget::Repository,
            "it is a view, not a dialog",
        ),
        (
            "unstage",
            GitTarget::Files(vec!["a.txt".into()]),
            "here it would run at once, with no dialog",
        ),
    ];
    for (id, on, why) in cases {
        let mut state = base.clone();
        state.git[0].dialog = Some(GitDialog::Action {
            id: id.into(),
            on,
            texts: Vec::new(),
            choices: Vec::new(),
        });
        let host = given(&g, across(&state));
        assert!(host.launched.lock().unwrap().is_empty(), "{id}: launched");
        assert!(
            host.b.git.panels.iter().all(|p| p.running.is_empty()),
            "{id}"
        );
        let said = host.b.status_line().text;
        assert!(said.ends_with(why), "{id}: {said}");
    }
    g.idle(1);
    let staged = g.repo.run(&["diff", "--cached", "--name-only"]);
    assert_eq!(staged.trim(), "a.txt", "still staged");
}

/// Every text the last pass drew, joined (the question's paragraph is wrapped in rows).
fn said(g: &Gx) -> String {
    let texts = g.fx.drv.texts();
    texts
        .into_iter()
        .map(|(_, _, t)| t)
        .collect::<Vec<_>>()
        .join(" ")
}

/// What is typed in a history edit's own dialog crosses (v3, X4-state2), so the question no
/// longer names it: with a reword's message typed the switch asks nothing; with the planner's
/// message dialog open, the question says only that the plan goes with the switch.
#[test]
fn a_history_edits_own_dialog_asks_nothing_more() {
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.f(Key::F2);
    assert!(g.panel().view.edit_draft().is_some(), "reword's dialog");
    g.until_quiet("the reword dialog");
    g.fx.typed(" again");
    g.idle(2);
    assert_eq!(g.fx.b.before_switch("box"), Before::Go);

    let Some(mut g) = planning() else {
        return;
    };
    g.fx.typed("r");
    g.key(Key::Enter);
    g.until_quiet("the planner's message dialog");
    assert!(g.panel().view.dialog_up(), "its message dialog");
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    g.idle(2);
    let said = said(&g);
    assert!(
        said.contains("The planner's unsaved plan goes with the switch to box.")
            && !said.contains("not carried"),
        "{said}"
    );
}

/// A fresh window on `g`'s repository given `state`, not drawn yet (as [`given`], unsettled).
fn fresh(g: &Gx, state: SwitchState) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let local = g.repo.local();
    let git: GitConnect = Arc::new(move || Ok(GitRunner::local(local.clone())));
    let mut fx = Fx {
        root: g.top(),
        cache: tmp.path().join("cache"),
        drv: Driver::new(1000.0, 700.0),
        b: Browser::new(Start {
            fs: Box::new(LocalFs::new()),
            connect: local_connect(),
            label: None,
            destination: None,
            abs: g.top(),
            cache: tmp.path().join("cache"),
            state_dir: tmp.path().join("state"),
            launch: Arc::new(|abs: &str, _| Ok(format!("opened {abs}"))),
            git: Some(git),
        }),
        launched: Arc::default(),
        _tmp: tmp,
    };
    fx.b.give_state(state).unwrap();
    fx
}

/// Passes until `g`'s repository has its panel in `fx`.
fn until_the_panel(fx: &mut Fx, top: &str) -> usize {
    let t = Instant::now();
    loop {
        if let Some(i) = fx.b.git.panel(top) {
            return i;
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "the panel within 20 s"
        );
        fx.drv.frame(&mut fx.b, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A full commit id no repository has.
fn nowhere() -> String {
    "fedcba9876".repeat(4)
}

/// Every commit a part names is looked up here first, and what this host lacks is left out and
/// said: a mark (the others kept), a plan with one row of it (no planner opened), a dialog on it
/// (none opened).
#[test]
fn what_this_host_lacks_is_left_out_and_said() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("second");
    let base = g.fx.b.take_state(&g.fx.drv.ctx);
    let bar = base.git[0].bar.clone().unwrap();
    let GitRow::Commit(second) = bar.clone() else {
        panic!("{bar:?}");
    };
    let mut marks = base.clone();
    marks.git[0].marks = vec![bar.clone(), GitRow::Commit(nowhere())];
    let host = given(&g, across(&marks));
    let i = host.b.git.panel(&g.top()).unwrap();
    let kept = host.b.take_state(&host.drv.ctx).git[0].marks.clone();
    assert_eq!(kept, [bar], "the mark this host has");
    assert!(host
        .b
        .status_line()
        .text
        .ends_with(": 1 of its marks are not here"));
    assert!(host.b.git.panels[i]
        .view
        .marked(&RowKey::Commit(Oid::parse(&second).unwrap())));

    let mut tag = base.clone();
    tag.git[0].dialog = Some(GitDialog::Action {
        id: "tag".into(),
        on: GitTarget::Commit {
            commit: nowhere(),
            marked: Vec::new(),
        },
        texts: Vec::new(),
        choices: Vec::new(),
    });
    let host = given(&g, across(&tag));
    let i = host.b.git.panel(&g.top()).unwrap();
    assert!(host.b.git.panels[i].view.draft().is_none(), "no Tag dialog");
    let said = host.b.status_line().text;
    assert!(
        said.ends_with("its dialog was not opened again: what it acts on is not here"),
        "{said}"
    );

    let Some(mut p) = planning() else {
        return;
    };
    p.fx.typed("d");
    p.until_quiet("a drop row");
    let mut plan = p.fx.b.take_state(&p.fx.drv.ctx);
    plan.git[0].plan.as_mut().unwrap().rows[1].commit = nowhere();
    let host = given(&p, across(&plan));
    let i = host.b.git.panel(&p.top()).unwrap();
    assert!(
        host.b.git.panels[i].view.planner_model().is_none(),
        "no planner"
    );
    let said = host.b.status_line().text;
    assert!(
        said.ends_with("its plan was not given: 1 of its commits are not here"),
        "{said}"
    );
}

/// Archive's path is on the machine that shows the window: a line that names one is not given it
/// — HOST's own download folder stands — and HOST's state carries none back.
#[test]
fn archives_path_stays_this_hosts_own() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("second");
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    let Some(GitRow::Commit(second)) = state.git[0].bar.clone() else {
        panic!("the bar");
    };
    state.git[0].dialog = Some(GitDialog::Action {
        id: "archive".into(),
        on: GitTarget::Commit {
            commit: second,
            marked: Vec::new(),
        },
        texts: vec![GitText {
            field: 0,
            text: "/elsewhere/not-yours.zip".into(),
        }],
        choices: Vec::new(),
    });
    let host = given(&g, across(&state));
    let i = host.b.git.panel(&g.top()).unwrap();
    let draft = host.b.git.panels[i].view.draft().cloned();
    let Some(crate::git::actions::Action::Archive { to, .. }) = draft else {
        panic!("the Archive dialog: {draft:?}");
    };
    assert!(to.starts_with(crate::gui::binary::download_dir()), "{to:?}");
    let back = host.b.take_state(&host.drv.ctx);
    match &back.git[0].dialog {
        Some(GitDialog::Action { id, texts, .. }) => assert!(id == "archive" && texts.is_empty()),
        other => panic!("{other:?}"),
    }
}

/// One panel's plan and its dialog both cross: the plan first, in the planner opened on this
/// host's range, then the dialog — the filters', over the planner.
#[test]
fn a_panels_plan_and_its_dialog_both_cross() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.fx.typed("d");
    g.until_quiet("a drop row");
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    assert!(state.git[0].dialog.is_none(), "no dialog yet");
    let filters = switch_state::GitFilters {
        branches: None,
        hidden_remotes: Vec::new(),
        tags: false,
        stashes: true,
        first_parent: false,
        order: switch_state::GitOrder::Date,
        reflog: false,
    };
    state.git[0].dialog = Some(GitDialog::Filters {
        filters,
        typed: "ma".into(),
    });
    let host = given(&g, across(&state));
    let i = host.b.git.panel(&g.top()).unwrap();
    let view = &host.b.git.panels[i].view;
    let model = view.planner_model().expect("the planner");
    assert_eq!(model.rows[0].verb.word(), "drop");
    assert!(view.fronts()[2].0, "the filters' dialog");
    let back = host.b.take_state(&host.drv.ctx);
    assert!(back.git[0].plan.is_some() && back.git[0].dialog.is_some());
}

/// The owner's own dialog, opened in the panel while its part is still being given, is not put
/// aside for the part's: the part's is not opened, and the status row says so.
#[test]
fn the_owners_own_dialog_stays_in_its_place() {
    let Some(g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    let fields = [
        "msg".to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ];
    state.git[0].dialog = Some(GitDialog::Search {
        fields,
        case: false,
    });
    let mut host = fresh(&g, across(&state));
    let i = until_the_panel(&mut host, &g.top());
    host.b.git.panels[i].view.open_filters();
    settle(&mut host);
    let fronts = host.b.git.panels[i].view.fronts();
    assert!(
        fronts[2].0 && !fronts[3].0,
        "the owner's filters, no search: {fronts:?}"
    );
    let said = host.b.status_line().text;
    assert!(
        said.ends_with("its dialog was not given: one of yours is open"),
        "{said}"
    );
}

/// A listing that ends the putting back before a part is all given (the owner's, a file
/// operation's or a git write's re-read) names it on
/// the status row.
#[test]
fn a_listing_of_your_own_names_the_parts_not_given() {
    let Some(g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let mut host = fresh(&g, across(&state));
    until_the_panel(&mut host, &g.top());
    let event = BrowserEvent::FilesChanged {
        host: None,
        dirs: vec![g.top()],
    };
    host.b.event(&host.drv.ctx.clone(), event);
    settle(&mut host);
    assert!(host.b.given.is_none());
    let said = host.b.status_line().text;
    assert!(
        said.ends_with(": not given: a listing started before it was"),
        "{said}"
    );
}

/// The list's question up while a part's dialog would open (the owner's F7 meanwhile): one
/// dialog at a time — the part's is not opened, and the status row says why.
#[test]
fn a_parts_dialog_waits_for_no_other_dialog() {
    let Some(g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    state.git_focus = false;
    let fields = [
        "msg".to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ];
    state.git[0].dialog = Some(GitDialog::Search {
        fields,
        case: false,
    });
    let mut host = fresh(&g, across(&state));
    let i = until_the_panel(&mut host, &g.top());
    host.drv.key(&mut host.b, Key::F7, Modifiers::NONE);
    settle(&mut host);
    assert!(
        host.b.ops.open_dialog(&host.drv.ctx).is_some(),
        "the make-folder dialog"
    );
    assert!(
        !host.b.git.panels[i].view.fronts()[3].0,
        "no history search"
    );
    let said = host.b.status_line().text;
    assert!(said.contains("another dialog has the keyboard"), "{said}");
}

/// A restore's dialog is not carried: the switch asks first and says it drops what is chosen.
#[test]
fn a_restores_dialog_asks_first() {
    let Some(mut g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let view = &mut g.panel_mut().view;
    let asked = view.open_restore("20260101T000000Z".into(), Vec::new(), None);
    assert!(
        asked.is_empty() && view.draft().is_some(),
        "the restore's dialog"
    );
    g.idle(2);
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    g.idle(2);
    let line = "What is chosen in the restore's dialog is not carried to box: switching drops it.";
    assert!(said(&g).contains(line), "{}", said(&g));
}

/// The owner's own dialog opened while a part's plan is still opening (its range being read):
/// the plan goes in, the part's dialog does not take the owner's place, and the status row says
/// so.
#[test]
fn the_owners_dialog_opened_while_the_plan_opens_stays() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.fx.typed("d");
    g.until_quiet("a drop row");
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    let fields = [
        "msg".to_string(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ];
    state.git[0].dialog = Some(GitDialog::Search {
        fields,
        case: false,
    });
    let mut host = fresh(&g, across(&state));
    let i = until_the_panel(&mut host, &g.top());
    let t = Instant::now();
    while host.b.git.panels[i].edit_open.is_none() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "the planner's range asked for"
        );
        host.drv.frame(&mut host.b, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    host.b.git.panels[i].view.open_filters();
    settle(&mut host);
    let view = &host.b.git.panels[i].view;
    assert_eq!(
        view.planner_model().map(|m| m.rows[0].verb.word()),
        Some("drop")
    );
    let fronts = view.fronts();
    assert!(
        fronts[2].0 && !fronts[3].0,
        "the owner's filters, no search: {fronts:?}"
    );
    let said = host.b.status_line().text;
    assert!(
        said.ends_with("its dialog was not given: one of yours is open"),
        "{said}"
    );
}

/// The question names every thing a switch drops, in one sentence: of a panel not on screen (the
/// bar on a file, its preview in the panel's place), its planner's plan and its open dialog — a
/// quick edit's, which a panel not on screen does not carry (`git_carried`): the state holds no
/// git panel.
#[test]
fn the_question_names_everything_dropped() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.key(Key::Tab);
    g.idle(2);
    g.fx.select("c.txt");
    g.fx.settle();
    g.idle(2);
    assert!(g.fx.b.git_carried().is_empty(), "the panel not on screen");
    let view = &mut g.panel_mut().view;
    let model = view.planner_model().expect("the planner").clone();
    let c = model.rows[0].commit.row.oid.clone();
    let asked = view.open_edit(crate::git::rebase::EditId::Reword, &c, &model);
    assert!(
        asked.is_empty() && view.edit_draft().is_some(),
        "Reword's dialog"
    );
    g.idle(2);
    assert!(
        g.fx.b.take_state(&g.fx.drv.ctx).git.is_empty(),
        "nothing of it carried"
    );
    assert_eq!(g.fx.b.before_switch("box"), Before::Held);
    g.idle(2);
    let both =
        "The unsaved plan of a planner not on screen and the open dialog of a git panel not \
                on screen are not carried to box: switching drops them.";
    assert!(said(&g).contains(both), "{}", said(&g));
}

/// A putting back that ends while a part's planner is opening (a listing started) stops that
/// read: no planner opens after it ended.
#[test]
fn a_plan_whose_giving_ended_opens_no_planner_later() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.fx.typed("d");
    g.until_quiet("a drop row");
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let mut host = fresh(&g, across(&state));
    let i = until_the_panel(&mut host, &g.top());
    let t = Instant::now();
    while host.b.git.panels[i].edit_open.is_none() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "the planner's range asked for"
        );
        host.drv.frame(&mut host.b, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    let event = BrowserEvent::FilesChanged {
        host: None,
        dirs: vec![g.top()],
    };
    host.b.event(&host.drv.ctx.clone(), event);
    assert!(host.b.given.is_none(), "the putting back ended");
    assert!(host.b.git.panels[i].edit_open.is_none(), "its read stopped");
    settle(&mut host);
    host.drv.idle(&mut host.b, 10);
    assert!(
        host.b.git.panels[i].view.planner_model().is_none(),
        "no planner later"
    );
}

/// A switch asked for while a given state's git panel is still being put back says so first:
/// what goes is that state's part as it came.
#[test]
fn a_switch_while_the_git_panel_is_put_back_says_so() {
    let Some(g) = ready() else {
        eprintln!("skipped: no git");
        return;
    };
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let mut host = fresh(&g, across(&state));
    until_the_panel(&mut host, &g.top());
    assert!(host.b.given.is_some(), "still being put back");
    assert_eq!(host.b.before_switch("box"), Before::Held);
    host.drv.idle(&mut host.b, 2);
    let texts = host.drv.texts();
    let said: Vec<&str> = texts.iter().map(|(_, _, t)| t.as_str()).collect();
    let said = said.join(" ");
    let line = "The git panel is still being put back: switching now sends it as it came.";
    assert!(said.contains(line), "{said}");
}

// X4-state2 (v3): what is typed in a history edit's own dialog crosses — a quick edit's (reword,
// squash, author) and the planner's message and author dialogs.

/// The open quick edit's draft in `fx`'s panel of `top`.
fn quick_draft(b: &Browser, top: &str) -> Option<crate::git::rebase::Edit> {
    let i = b.git.panel(top)?;
    b.git.panels[i].view.edit_draft().cloned()
}

/// `text` typed over all of the field that has the typing (Cmd/Ctrl+A first), in one pass.
fn replace_typed(g: &mut Gx, text: &str) {
    g.cmd(Key::A);
    g.idle(1);
    g.fx.drv
        .frame(&mut g.fx.b, vec![Event::Text(text.into())], Modifiers::NONE);
    g.until_quiet("the typing");
}

/// Reword's dialog (F2) with a Hangul message of two lines, CRLF between them, typed over the
/// commit's: the switch asks nothing, and HOST opens Reword as F2 does on its own row of the
/// commit, the message byte for byte; its close brings it back.
#[test]
fn rewords_dialog_typed_into_crosses() {
    use crate::git::rebase::{Edit, Quick};
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.f(Key::F2);
    let message = "다시 쓴 메시지\r\n둘째 줄";
    replace_typed(&mut g, message);
    assert!(
        matches!(quick_draft(g.b(), &g.top()), Some(Edit::Quick(Quick::Reword { message: m, .. })) if m == message),
        "typed: {:?}",
        quick_draft(g.b(), &g.top())
    );
    assert_eq!(g.fx.b.before_switch("box"), Before::Go, "it asks nothing");
    let (state, home) = round_trip(&mut g, "reword");
    let line = state.line();
    assert!(
        line.contains(r#""dialog":{"reword":{"commit":""#)
            && line.contains(r#""message":"다시 쓴 메시지\r\n둘째 줄""#),
        "{line}"
    );
    let draft = quick_draft(&home.b, &g.top());
    assert!(
        matches!(&draft, Some(Edit::Quick(Quick::Reword { message: m, .. })) if m == message),
        "byte for byte: {draft:?}"
    );
}

/// Squash's dialog (the menu's) with the two messages' join typed over: HOST opens Squash on its
/// own row, the message in it.
#[test]
fn squashs_dialog_typed_into_crosses() {
    use crate::git::rebase::{Edit, Quick};
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.menu("Squash into the commit below…");
    let message = "A and B, as one\n\nthe second paragraph";
    replace_typed(&mut g, message);
    let (state, home) = round_trip(&mut g, "squash");
    assert!(
        state.line().contains(r#""dialog":{"squash":{"commit":""#),
        "{}",
        state.line()
    );
    let draft = quick_draft(&home.b, &g.top());
    assert!(
        matches!(&draft, Some(Edit::Quick(Quick::Squash { message: m, .. })) if m == message),
        "{draft:?}"
    );
}

/// Author's dialog (the menu's) with a name typed and the committer date "now" — not its
/// default: HOST opens it on its own row, the name in it and "now" chosen, by its word.
#[test]
fn authors_dialog_typed_into_crosses() {
    use crate::git::rebase::{CommitterDate, Edit, Quick};
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.menu("Author and date…");
    replace_typed(&mut g, "배아 Kim");
    g.click("( ) now");
    g.until_quiet("the committer date");
    let (state, home) = round_trip(&mut g, "author");
    let line = state.line();
    assert!(
        line.contains(r#""dialog":{"author":{"commit":""#)
            && line.contains(r#""name":"배아 Kim""#)
            && line.contains(r#""committer":"now""#),
        "{line}"
    );
    let draft = quick_draft(&home.b, &g.top());
    assert!(
        matches!(&draft, Some(Edit::Quick(Quick::Author { author, .. }))
            if author.name == "배아 Kim" && author.committer == CommitterDate::Now),
        "{draft:?}"
    );
}

/// The planner's own message dialog (Enter on a reword row) with text typed in it: HOST's planner
/// opens it on the same row over the plan, the text in it — and its Enter keeps it there.
#[test]
fn the_planners_message_dialog_typed_into_crosses() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.fx.typed("r");
    g.key(Key::Enter);
    g.fx.typed(" edited");
    g.until_quiet("the planner's message dialog");
    assert!(g.panel().view.dialog_up(), "its message dialog");
    let (state, mut home) = round_trip(&mut g, "the planner's message");
    let line = state.line();
    assert!(
        line.contains(r#""dialog":{"plan_message":{"commit":""#)
            && line.contains(r#""message":"C\n edited""#),
        "{line}"
    );
    let i = home.b.git.panel(&g.top()).unwrap();
    assert!(home.b.git.panels[i].view.dialog_up(), "the dialog on HOST");
    home.drv.key(&mut home.b, Key::Enter, Modifiers::NONE);
    home.drv.idle(&mut home.b, 2);
    let view = &home.b.git.panels[i].view;
    let model = view.planner_model().expect("the planner");
    assert_eq!(model.rows[0].message.as_deref(), Some("C\n edited"));
    assert!(!view.dialog_up(), "kept");
}

/// Enters, typed text and a Space — several in one pass — in every pass while HOST's Reword
/// dialog comes back and in the pass its message goes in: nothing is reworded, nothing runs, and
/// the dialog stays with its message exactly as typed there.
#[test]
fn enters_while_rewords_dialog_comes_back_run_nothing() {
    use crate::git::rebase::{Edit, Quick};
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.f(Key::F2);
    replace_typed(&mut g, "kept");
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let head = g.repo.run(&["rev-parse", "HEAD"]);
    let mut host = fresh(&g, across(&state));
    let key = |key| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let top = g.top();
    let given = |b: &Browser| matches!(quick_draft(b, &top), Some(Edit::Quick(Quick::Reword { message, .. })) if message == "kept");
    let (t, mut pressed) = (Instant::now(), 0);
    while !given(&host.b) {
        assert!(t.elapsed() < Duration::from_secs(20), "given within 20 s");
        let coming = (host.b.git.panel(&top)).is_some_and(|i| {
            let p = &host.b.git.panels[i];
            p.edit_open.is_some() || p.view.edit_draft().is_some()
        });
        let events = match coming {
            true => vec![
                key(Key::Enter),
                Event::Text("x".into()),
                key(Key::Space),
                key(Key::Enter),
            ],
            false => Vec::new(),
        };
        pressed += events.len();
        host.drv.frame(&mut host.b, events, Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(pressed >= 8, "keys while it came back: {pressed}");
    settle(&mut host);
    let i = host.b.git.panel(&top).unwrap();
    assert!(host.b.git.panels[i].running.is_empty(), "nothing runs");
    assert!(given(&host.b), "{:?}", quick_draft(&host.b, &top));
    assert_eq!(g.repo.run(&["rev-parse", "HEAD"]), head, "nothing reworded");
    // The control: once shown, its Enter rewords.
    host.drv.key(&mut host.b, Key::Enter, Modifiers::NONE);
    let t = Instant::now();
    let ran = |b: &Browser| (b.git.panels.iter()).all(|p| p.running.is_empty());
    while g.repo.run(&["rev-parse", "HEAD"]) == head || !ran(&host.b) {
        assert!(t.elapsed() < Duration::from_secs(20), "the control's run");
        host.drv.frame(&mut host.b, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    let b = g.repo.run(&["log", "-1", "--format=%s", "HEAD~1"]);
    assert_eq!(b.trim(), "kept", "B reworded");
}

/// The owner's own dialog, opened while a given Reword's range is still being read, stays: the
/// read stops, no Reword opens later, and the status row says so.
#[test]
fn the_owners_dialog_opened_while_a_quick_edit_opens_stays() {
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.f(Key::F2);
    replace_typed(&mut g, "theirs");
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let mut host = fresh(&g, across(&state));
    let i = until_the_panel(&mut host, &g.top());
    let t = Instant::now();
    while host.b.git.panels[i].edit_open.is_none() {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "Reword's range asked for"
        );
        host.drv.frame(&mut host.b, Vec::new(), Modifiers::NONE);
        std::thread::sleep(Duration::from_millis(5));
    }
    host.b.git.panels[i].view.open_filters();
    settle(&mut host);
    host.drv.idle(&mut host.b, 10);
    let view = &host.b.git.panels[i].view;
    assert!(view.fronts()[2].0, "the owner's filters");
    assert!(
        view.edit_draft().is_none(),
        "no Reword: {:?}",
        view.edit_draft()
    );
    let said = host.b.status_line().text;
    assert!(
        said.ends_with("its dialog was not given: one of yours is open"),
        "{said}"
    );
}

/// A quick edit's dialog on a commit this host has not: none opens, and the status row says so.
#[test]
fn a_quick_edit_on_a_commit_this_host_lacks_is_said() {
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.f(Key::F2);
    replace_typed(&mut g, "theirs");
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let line = state.line();
    let Some(at) = line.find(r#""reword":{"commit":""#) else {
        panic!("Reword's dialog in the state: {line}");
    };
    let at = at + r#""reword":{"commit":""#.len();
    let line = format!("{}{}{}", &line[..at], nowhere(), &line[at + 40..]);
    let state = switch_state::read(&line, "HOST").expect("it reads");
    let host = given(&g, across(&state));
    let i = host.b.git.panel(&g.top()).unwrap();
    assert!(
        host.b.git.panels[i].view.edit_draft().is_none(),
        "no Reword"
    );
    let said = host.b.status_line().text;
    assert!(
        said.ends_with("its dialog was not opened again: what it acts on is not here"),
        "{said}"
    );
}

/// The planner's own author dialog (`A` on a row) with a name typed and the committer date "now"
/// — not its default: HOST's planner opens it on the same row over the plan, the name in it and
/// "now" chosen, by its word — and its Enter keeps them there.
#[test]
fn the_planners_author_dialog_typed_into_crosses() {
    use crate::git::rebase::CommitterDate;
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.fx.typed("a");
    g.until_quiet("the planner's author dialog");
    assert!(g.panel().view.dialog_up(), "its author dialog");
    replace_typed(&mut g, "배아 Kim");
    g.click("( ) now");
    g.until_quiet("the committer date");
    let (state, mut home) = round_trip(&mut g, "the planner's author");
    let line = state.line();
    assert!(
        line.contains(r#""dialog":{"plan_author":{"commit":""#)
            && line.contains(r#""name":"배아 Kim""#)
            && line.contains(r#""committer":"now""#),
        "{line}"
    );
    let i = home.b.git.panel(&g.top()).unwrap();
    assert!(home.b.git.panels[i].view.dialog_up(), "the dialog on HOST");
    home.drv.key(&mut home.b, Key::Enter, Modifiers::NONE);
    home.drv.idle(&mut home.b, 2);
    let view = &home.b.git.panels[i].view;
    let model = view.planner_model().expect("the planner");
    let author = model.rows[0].author.as_ref();
    let kept = author.map(|a| (a.name.as_str(), a.committer));
    assert_eq!(kept, Some(("배아 Kim", CommitterDate::Now)));
    assert!(!view.dialog_up(), "kept");
}

/// The planner's own dialog opens only over the plan given: a plan whose commits are not HOST's
/// range (its base moved up a row) leaves the planner as read, and its author dialog — on a row
/// that range has — is not opened on it; the status row says so.
#[test]
fn the_planners_dialog_opens_only_over_its_plan() {
    let Some(mut g) = planning() else {
        eprintln!("skipped: no git");
        return;
    };
    g.fx.typed("a");
    g.until_quiet("the planner's author dialog");
    let mut state = g.fx.b.take_state(&g.fx.drv.ctx);
    assert!(
        state.line().contains(r#""plan_author""#),
        "{}",
        state.line()
    );
    // Above A, this host's range is C and B; the plan's rows are C, B and A, the dialog on C.
    let plan = state.git[0].plan.as_mut().unwrap();
    plan.base = Some(plan.rows[2].commit.clone());
    let host = given(&g, across(&state));
    let i = host.b.git.panel(&g.top()).unwrap();
    let view = &host.b.git.panels[i].view;
    assert!(view.planner_model().is_some(), "the planner, as read");
    assert!(!view.dialog_up(), "no author dialog on it");
    let said = host.b.status_line().text;
    assert!(
        said.ends_with("its dialog was not given: its plan was not"),
        "{said}"
    );
}

/// Every text the last pass of `fx` drew, joined.
fn drawn(fx: &Fx) -> String {
    let texts = fx.drv.texts();
    texts
        .into_iter()
        .map(|(_, _, t)| t)
        .collect::<Vec<_>>()
        .join(" ")
}

/// F2 and Cmd/Ctrl+K in one pass: Reword's range is read while the filters' dialog opens, and
/// Reword opens in front of it (`modals`, over the filters). With its message typed, the state
/// carries the one in front — Reword's, one git dialog at a time — and the switch asks first,
/// naming the filters' dialog behind it, which it drops.
#[test]
fn the_dialog_in_front_is_carried_and_the_one_behind_is_named() {
    use crate::git::rebase::{Edit, Quick};
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    let press = |key, modifiers| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    let both = vec![
        press(Key::F2, Modifiers::NONE),
        press(Key::K, Modifiers::COMMAND),
    ];
    g.fx.drv.frame(&mut g.fx.b, both, Modifiers::NONE);
    let top = g.top();
    g.until("Reword's dialog", |b| quick_draft(b, &top).is_some());
    g.until_quiet("its reads");
    assert!(
        g.panel().view.fronts()[2].0,
        "the filters' dialog behind it"
    );
    replace_typed(&mut g, "in front");
    assert!(
        matches!(quick_draft(g.b(), &top), Some(Edit::Quick(Quick::Reword { message, .. })) if message == "in front"),
        "typed into Reword: {:?}",
        quick_draft(g.b(), &top)
    );
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    assert!(
        state.line().contains(r#""dialog":{"reword":{"commit":""#),
        "the one in front: {}",
        state.line()
    );
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    let line = "What is chosen in the filters' dialog is not carried to box: switching drops it.";
    assert!(said(&g).contains(line), "{}", said(&g));
}

/// A given quick edit's fields open afresh: HOST's Author dialog laid its own, shorter name out
/// before the given one went in, its caret at that name's end — and what is typed later goes at
/// the end of the given name, not where HOST's caret stood.
#[test]
fn a_given_quick_edits_fields_open_afresh() {
    use crate::git::rebase::{Edit, Quick};
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.menu("Author and date…");
    let name = "Beatrice Kimberly-Longname";
    replace_typed(&mut g, name);
    let state = g.fx.b.take_state(&g.fx.drv.ctx);
    let mut host = given(&g, across(&state));
    host.drv
        .frame(&mut host.b, vec![Event::Text("x".into())], Modifiers::NONE);
    host.drv.idle(&mut host.b, 2);
    let draft = quick_draft(&host.b, &g.top());
    let want = format!("{name}x");
    assert!(
        matches!(&draft, Some(Edit::Quick(Quick::Author { author, .. })) if author.name == want),
        "{draft:?}"
    );
}

/// Author's dialog with its date half typed (`2026-01-0`, which does not read yet): it crosses as
/// typed, and HOST's dialog says the date's refusal, its Run off — an Enter there runs nothing.
#[test]
fn a_half_typed_author_date_crosses_as_typed() {
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.menu("Author and date…");
    // Past Name and Email to the author date.
    g.key(Key::Tab);
    g.key(Key::Tab);
    g.idle(1);
    replace_typed(&mut g, "2026-01-0");
    let (state, mut home) = round_trip(&mut g, "a half-typed date");
    assert!(
        state.line().contains(r#""date":"2026-01-0""#),
        "{}",
        state.line()
    );
    let refusal = "the author date is not YYYY-MM-DD";
    assert!(drawn(&home).contains(refusal), "{}", drawn(&home));
    let head = g.repo.run(&["rev-parse", "HEAD"]);
    home.drv.key(&mut home.b, Key::Enter, Modifiers::NONE);
    settle(&mut home);
    assert_eq!(g.repo.run(&["rev-parse", "HEAD"]), head, "nothing ran");
    assert!(quick_draft(&home.b, &g.top()).is_some(), "the dialog stays");
}

/// Author's dialog with a 5,000-byte name pasted — past what a state holds of a name (4,096
/// bytes), so the switch would leave it out (`fit`): the switch asks first, naming it as too
/// long to carry.
#[test]
fn a_quick_edit_too_long_to_carry_asks_first() {
    let Some(mut g) = linear() else {
        eprintln!("skipped: no git");
        return;
    };
    g.bar_to("B");
    g.menu("Author and date…");
    replace_typed(&mut g, &"n".repeat(5000));
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    let line = "What is typed in the history edit's dialog (too long to carry) is not carried to \
                box: switching drops it.";
    assert!(said(&g).contains(line), "{}", said(&g));
}

/// The switch's question asks in place whether the dialog that crosses is within what a state
/// holds, every pass it is up: with Reword's dialog of a mebibyte message in front of a restore's
/// (which makes the switch ask), ten passes of the question copy none of the message.
#[test]
fn the_question_copies_no_message_while_it_is_up() {
    use crate::gui::browser::git_panel;
    let Some(git) = fixture::git() else {
        eprintln!("skipped: no git");
        return;
    };
    let repo = fixture::Repo::new(&git);
    for (i, name) in ["base", "A"].into_iter().enumerate() {
        repo.write("c.txt", format!("{i}\n"));
        repo.commit(name);
    }
    let message = format!("big\n\n{}", "0123456789abcde\n".repeat(1 << 16));
    let file = repo.tmp.path().join("big-message.txt");
    std::fs::write(&file, &message).unwrap();
    repo.write("c.txt", "big\n");
    repo.run(&["add", "-A"]);
    repo.run(&["commit", "-q", "-F", file.to_str().unwrap()]);
    let mut g = Gx::new(repo);
    g.ready_in_git();
    g.bar_to("big");
    g.f(Key::F2);
    g.fx.typed("x");
    g.until_quiet("Reword's dialog");
    // A restore's dialog behind it makes the switch ask; Reword's, in front, crosses.
    let view = &mut g.panel_mut().view;
    view.open_restore("20260101T000000Z".into(), Vec::new(), None);
    g.idle(2);
    assert_eq!(g.fx.b.before_switch("box"), Before::Held, "it asks first");
    g.idle(2);
    assert!(g.fx.b.switch_asking(), "the question is up");
    git_panel::quick_copied();
    g.idle(10);
    assert!(g.fx.b.switch_asking(), "still up");
    assert_eq!(git_panel::quick_copied(), 0, "ten passes copied none of it");
}
