//! X4-dialogs2's tests (10 §17): 03 §7.1–§7.5's forms, the result dialog and the stop question
//! in cells, the egui dialog and the forms' egui parts gone.
//!
//! As X4-dialogs' tests do, each dialog stands in a host that lays a screen of cells under it and
//! blanks what the dialog says it covers first, so a terminal (`term::compose` of the pass's
//! layers) shows the window's cells exactly. No git runs: every dialog is given a planner of the
//! test's own. Keys and clicks go through the test kit, several in one pass where it matters.

use super::dialogs::{self, ActionDialog, DialogEnd, Opened, Planner, Prefill, ResultDialog};
use super::menu::Target;
use super::tests_s3::repo;
use super::GitAction;
use crate::git::actions::{
    Action, ActionId, ArchiveFormat, Confirmation, FastForward, MergeOptions, Outcome, Plan,
    Proceed, PullHow, Refused, Remote, Repo, Rev, StepOutcome, StepResult,
};
use crate::git::model::{Change, CommitRow, Oid, RefLabel, StashRow, StatusEntry, StatusState};
use crate::git::runner::{Done, Ending};
use crate::gui::browser::ops::dialog::choice_id;
use crate::gui::cells::{self, Buffer, Cells, Grid, Layer, Pen, Position};
use crate::gui::chrome::{self, PALETTE};
use crate::gui::term;
use crate::gui::testkit::{self, Driver};
use crate::gui::Window;
use eframe::egui::{self, Color32, Event, Key, Modifiers, Pos2};
use std::cell::RefCell;
use std::sync::Arc;

use ActionId as A;

const CELL: egui::Vec2 = chrome::CELL;

// ---------------------------------------------------------------------------------------------
// Fixtures.

fn oid(c: char) -> Oid {
    Oid::parse(&c.to_string().repeat(40)).expect("a hex digit")
}

fn commit(c: char, parents: &[char], subject: &str) -> CommitRow {
    CommitRow {
        oid: oid(c),
        parents: parents.iter().map(|&p| oid(p)).collect(),
        author_name: "Ada".into(),
        author_email: "ada@example.com".into(),
        author_time: 1_794_389_040,
        commit_time: 1_794_389_040,
        subject: subject.into(),
    }
}

fn changed(path: &str, staged: Option<Change>, unstaged: Option<Change>) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        old_path: None,
        state: StatusState::Changed { staged, unstaged },
        lossy: false,
    }
}

/// The S3 fixture (`main` at `a` checked out, `feature` at `b`, both on `origin`, tags `v1` and
/// annotated `v2`, a stash) with a staged Hangul file, an unstaged and an untracked one, and a
/// second, Hangul-named remote.
fn dirty() -> Repo {
    let mut r = repo();
    let mut new = changed("n.txt", None, None);
    new.state = StatusState::Untracked;
    r.status.entries = vec![
        changed("문서.txt", Some(Change::Modified), None),
        changed("u.txt", None, Some(Change::Modified)),
        new,
    ];
    r.remotes.push("백업".into());
    r
}

/// The fixture's own ref `refname`.
fn label(refname: &str) -> RefLabel {
    (repo().refs.into_iter())
        .find(|r| r.refname == refname)
        .unwrap_or_else(|| panic!("no {refname}"))
}

fn stash_row() -> StashRow {
    repo().stashes[0].clone()
}

/// The bar on commit `b` (parent `a`), `c` marked.
fn on_commit() -> Target {
    Target::Commit {
        commit: commit('b', &['a'], "feature work"),
        marked: Vec::new(),
    }
}

thread_local! {
    /// Every action a test's planner was asked for, in order (each test runs on its own thread).
    static ASKED: RefCell<Vec<Action>> = const { RefCell::new(Vec::new()) };
}

fn asked() -> Vec<Action> {
    ASKED.with(|a| a.borrow().clone())
}

fn plan(confirm: Option<Confirmation>) -> Plan {
    Plan {
        confirm,
        backups: Vec::new(),
        message: String::new(),
        steps: Vec::new(),
        changes_files: false,
    }
}

/// A planner whose every plan has a confirmation naming the action, its command line the action
/// whole — so what the plan shows says what runs.
fn fake(action: &Action, _: &Repo) -> Result<Plan, Refused> {
    ASKED.with(|a| a.borrow_mut().push(action.clone()));
    let id = action.id().as_str();
    let confirm = Confirmation {
        title: format!("Fake {id}"),
        loses: Some(format!("{id} loses this")),
        changes: vec![format!("{id}: 11111111 → 22222222")],
        backups: vec![format!("heads/{id}")],
        commands: vec![format!("{action:?}")],
        notes: vec![format!("{id} note")],
    };
    Ok(plan(Some(confirm)))
}

/// [`fake`]'s plan with its command line the action's name alone: a plan of a few short rows,
/// whatever the draft holds (a message of a mebibyte).
fn brief(action: &Action, repo: &Repo) -> Result<Plan, Refused> {
    let mut p = fake(action, repo)?;
    if let Some(c) = &mut p.confirm {
        c.commands = vec![format!("git {}", action.id().as_str())];
    }
    Ok(p)
}

/// [`brief`]'s plan titled with the branch checked out, as the real planner's Merge is (`Merge
/// <rev> into <head>`), while its rows never name it.
fn headed(action: &Action, repo: &Repo) -> Result<Plan, Refused> {
    let mut p = brief(action, repo)?;
    let head = match &repo.head {
        crate::git::model::Head::Branch { refname, .. } => refname.clone(),
        other => format!("{other:?}"),
    };
    if let Some(c) = &mut p.confirm {
        c.title = format!("Merge into {head}");
    }
    Ok(p)
}

fn dialog(opened: Opened) -> ActionDialog {
    match opened {
        Opened::Dialog(d) => *d,
        other => panic!("not a dialog: {other:?}"),
    }
}

fn open(id: ActionId, target: Target, repo: &Repo) -> ActionDialog {
    open_with(id, target, repo, &Prefill::default())
}

fn open_with(id: ActionId, target: Target, repo: &Repo, prefill: &Prefill) -> ActionDialog {
    dialog(ActionDialog::open(id, &target, repo, prefill, fake))
}

// ---------------------------------------------------------------------------------------------
// The host: a screen of cells, the dialog over it.

/// What stands over the host's screen.
enum Over {
    Action(Box<ActionDialog>),
    Result(ResultDialog),
}

struct Host {
    over: Option<Over>,
    planner: Planner,
    ends: Vec<DialogEnd>,
    /// Results closed.
    closed: usize,
    /// What the dialog said it covers, last pass.
    covers: Vec<Cells>,
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let grid = Grid::over(ctx.content_rect());
        // The screen under it, the cells it covers blanked first (cells.rs's rule).
        let body = Pen::new(PALETTE.text, PALETTE.back);
        let mut under = Buffer::empty(grid.all());
        cells::fill(&mut under, grid.all(), body);
        // No blank cell anywhere: a cell left as it was under a dialog shows.
        let words = "한글under".repeat(30);
        for y in 0..grid.rows {
            cells::put(&mut under, 0, y, &words, usize::from(grid.cols), body);
        }
        self.covers = match &self.over {
            Some(Over::Action(d)) => d.covers(&grid),
            Some(Over::Result(r)) => r.covers(&grid),
            None => Vec::new(),
        };
        for c in &self.covers {
            cells::fill(&mut under, *c, body);
        }
        cells::paint(ui.painter(), grid.origin, &under, None);
        let done = match &mut self.over {
            Some(Over::Action(d)) => {
                (d.ui(&ctx, grid, self.planner)).map(|end| self.ends.push(end))
            }
            Some(Over::Result(r)) => r.ui(&ctx, grid).then(|| self.closed += 1),
            None => None,
        };
        if done.is_some() {
            self.over = None;
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

struct Fx {
    drv: Driver,
    host: Host,
}

impl Fx {
    /// `over` on a `cols`×`rows` grid, laid out twice (a new modal is laid out unseen first).
    fn over(over: Over, cols: u16, rows: u16) -> Fx {
        let mut fx = Fx {
            drv: Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y),
            host: Host {
                over: Some(over),
                planner: fake,
                ends: Vec::new(),
                closed: 0,
                covers: Vec::new(),
            },
        };
        fx.idle(2);
        fx
    }

    fn new(d: ActionDialog) -> Fx {
        Fx::over(Over::Action(Box::new(d)), 100, 30)
    }

    fn result(r: ResultDialog) -> Fx {
        Fx::over(Over::Result(r), 100, 30)
    }

    fn idle(&mut self, n: usize) {
        self.drv.idle(&mut self.host, n);
    }

    /// One pass with `events`, then one without.
    fn frame(&mut self, events: Vec<Event>) {
        self.drv.frame(&mut self.host, events, Modifiers::NONE);
        self.idle(1);
    }

    /// One pass with `events` and no other: what the owner saw is the pass before.
    fn raw(&mut self, events: Vec<Event>) {
        self.drv.frame(&mut self.host, events, Modifiers::NONE);
    }

    fn press(&mut self, key: Key) {
        self.drv.key(&mut self.host, key, Modifiers::NONE);
    }

    fn focus(&mut self, on: egui::Id) {
        self.drv.ctx.memory_mut(|m| m.request_focus(on));
        self.idle(2);
    }

    fn focused(&self) -> Option<egui::Id> {
        self.drv.ctx.memory(|m| m.focused())
    }

    /// A click on the first cell of `text` in the box: a choice right after its row's label is
    /// one run with it, so the exact text queries cannot find it alone.
    fn click(&mut self, text: &str) {
        let at = place(&self.boxed(), text);
        self.drv.click(&mut self.host, at_cell(at.x, at.y));
        self.idle(1);
    }

    fn dialog_mut(&mut self) -> &mut ActionDialog {
        match &mut self.host.over {
            Some(Over::Action(d)) => d,
            _ => panic!("no action dialog open"),
        }
    }

    fn draft(&self) -> Action {
        match &self.host.over {
            Some(Over::Action(d)) => d.draft().clone(),
            _ => panic!("no action dialog open"),
        }
    }

    /// The dialog's layer: the one whose first row is a box's top line.
    fn boxed(&self) -> Arc<Layer> {
        boxed(&self.drv).unwrap_or_else(|| panic!("no dialog in cells"))
    }

    /// The box's rows, as one text.
    fn shown(&self) -> String {
        rows(&self.boxed()).join("\n")
    }

    fn ran(&self) -> Option<Action> {
        match &self.host.ends[..] {
            [DialogEnd::Run { action, .. }] => Some(action.clone()),
            [] => None,
            other => panic!("{other:?}"),
        }
    }
}

/// The topmost layer whose top row is a dialog's frame (`╔`): a dialog in cells (the git panel's
/// own frame is double-lined too, under it).
fn boxed(drv: &Driver) -> Option<Arc<Layer>> {
    cells::layers(&drv.ctx).into_iter().rev().find(|l| {
        let a = l.buf.area;
        a.height > 0 && cells::row_text(&l.buf, a.y).0.trim_start().starts_with('╔')
    })
}

fn rows(l: &Layer) -> Vec<String> {
    let a = l.buf.area;
    (a.top()..a.bottom())
        .map(|y| cells::row_text(&l.buf, y).0)
        .collect()
}

/// Where `text` first stands in the layer.
fn place(l: &Arc<Layer>, text: &str) -> Cells {
    let hits = cells::find(std::slice::from_ref(l), text);
    hits.first()
        .unwrap_or_else(|| panic!("{text:?} not in {:#?}", rows(l)))
        .cells
}

fn pen_of(l: &Arc<Layer>, text: &str) -> (Option<Color32>, Option<Color32>) {
    let at = place(l, text);
    let a = l.buf.area;
    let seen = &l.seen()[usize::from(at.y - a.y)][usize::from(at.x - a.x)];
    (seen.fg, seen.bg)
}

fn text_pen() -> (Option<Color32>, Option<Color32>) {
    (Some(PALETTE.dialog_text), Some(PALETTE.dialog_back))
}

fn weak_pen() -> (Option<Color32>, Option<Color32>) {
    (Some(PALETTE.dialog_weak), Some(PALETTE.dialog_back))
}

fn error_pen() -> (Option<Color32>, Option<Color32>) {
    (Some(PALETTE.dialog_error), Some(PALETTE.dialog_back))
}

fn field_pen() -> (Option<Color32>, Option<Color32>) {
    (Some(PALETTE.field_text), Some(chrome::cga::BLACK))
}

/// The middle of cell (`x`, `y`) in points.
fn at_cell(x: u16, y: u16) -> Pos2 {
    Pos2::new((f32::from(x) + 0.5) * CELL.x, (f32::from(y) + 0.5) * CELL.y)
}

/// What a terminal shows of the pass (`term::compose`), every cell the window's: per cell the
/// last layer painted there, the same character, foreground and background, and the same caret;
/// no hand-off box.
fn same_in_a_terminal(drv: &Driver) {
    let layers = cells::layers(&drv.ctx);
    let r = drv.ctx.content_rect();
    let (cols, rows) = ((r.width() / CELL.x) as u16, (r.height() / CELL.y) as u16);
    let screen = term::compose(&drv.ctx, &layers, cols, rows);
    assert_eq!(screen.hand_offs, vec![], "every cell is the window's");
    for y in 0..rows {
        for x in 0..cols {
            let at = Position::new(x, y);
            let want = (layers.iter().rev())
                .find(|l| {
                    l.buf.area.contains(at)
                        && !matches!(l.buf[at].bg, ratatui_core::style::Color::Reset)
                })
                .map(|l| &l.buf[at])
                .expect("a layer under every cell");
            let got = &screen.buf[at];
            assert_eq!(
                (want.symbol(), want.fg, want.bg),
                (got.symbol(), got.fg, got.bg),
                "cell {x},{y}"
            );
        }
    }
    let caret = layers.iter().rev().find_map(|l| l.caret);
    assert_eq!(screen.caret, caret, "the caret's cell");
}

fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers,
    }
}

/// The primary button pressed or let go at `at`.
fn pointer(at: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn down(k: Key) -> Event {
    key(k, true, Modifiers::NONE)
}

/// Space as egui-winit sends it: the key and its text in one pass.
fn space() -> Vec<Event> {
    vec![down(Key::Space), Event::Text(" ".into())]
}

// ---------------------------------------------------------------------------------------------
// Every action's dialog.

/// The objects whose menus offer every action of 03 §7.1–§7.7 on the fixture.
fn targets() -> Vec<Target> {
    vec![
        Target::Commit {
            commit: commit('b', &['a'], "feature work"),
            marked: vec![commit('c', &['b'], "later")],
        },
        Target::Label(label("refs/heads/feature")),
        Target::Label(label("refs/remotes/origin/feature")),
        Target::Label(label("refs/tags/v2")),
        Target::Stash(stash_row()),
        Target::Uncommitted,
        Target::Files(vec![
            changed("s.txt", Some(Change::Modified), Some(Change::Modified)),
            changed("u.txt", None, Some(Change::Modified)),
        ]),
        Target::Repository,
    ]
}

/// The row's receipt in the tests: every action's dialog — every id of 03 §7 that opens one, the
/// Backups list's and the banner's — is the box in cells, titled with its plan, covering its box
/// and shadow; the terminal shows exactly the window's cells, no hand-off box anywhere.
#[test]
fn x4d2_every_action_dialog_is_cells_in_both_backends() {
    let d = dirty();
    let mut conflicted = dirty();
    let mut c = changed("c.txt", None, None);
    c.state = StatusState::Conflict("UU".into());
    conflicted.status.entries.push(c.clone());
    let mut checked = 0;
    for id in ActionId::ALL {
        let stamp = "20261128T101500Z".to_string();
        let opened = match id {
            A::Restore => ActionDialog::restore(stamp, Vec::new(), None, &d, fake),
            A::DeleteBackup => ActionDialog::confirm(Action::DeleteBackup { stamp }, &d, fake),
            A::Continue | A::Skip | A::Abort | A::BisectReset => {
                let p = match id {
                    A::Continue => Proceed::Continue,
                    A::Skip => Proceed::Skip,
                    A::Abort => Proceed::Abort,
                    _ => Proceed::BisectReset,
                };
                ActionDialog::confirm(Action::Proceed(p), &d, fake)
            }
            A::Ours | A::Theirs | A::Resolved | A::RedoMerge => {
                let files = Target::Files(vec![c.clone()]);
                ActionDialog::open(id, &files, &conflicted, &Prefill::default(), fake)
            }
            _ => {
                let all = targets();
                let Some(target) = all.iter().find(|t| {
                    let offered = super::menu::items(t, &d);
                    offered.iter().any(|i| i.id == id)
                }) else {
                    // A view only the panel carries out, or a history edit (X4-quick's).
                    continue;
                };
                ActionDialog::open(id, target, &d, &Prefill::default(), fake)
            }
        };
        let Opened::Dialog(dialog) = opened else {
            continue;
        };
        let name = id.as_str();
        let fx = Fx::new(*dialog);
        let l = boxed(&fx.drv).unwrap_or_else(|| panic!("{name}: not in cells"));
        let all = rows(&l).join("\n");
        assert!(all.contains(&format!("Fake {name}")), "{name}: {all}");
        assert!(all.contains("[ Run ]  [ Cancel ]"), "{name}: {all}");
        assert_eq!(fx.host.covers.len(), 3, "{name}: the box and its shadow");
        same_in_a_terminal(&fx.drv);
        checked += 1;
    }
    assert!(checked >= 40, "{checked} dialogs");
}

/// 03 §7.1–§7.5's forms: each its options as the kit's fields, message fields and choices — one
/// of several `(•) word`, any of several `[X] word` — with their defaults.
#[test]
fn x4d2_each_form_shows_its_options() {
    let d = dirty();
    let merge = Target::Commit {
        commit: commit('e', &['a', 'b'], "a merge"),
        marked: vec![
            commit('e', &['a', 'b'], "a merge"),
            commit('c', &['b'], "c"),
        ],
    };
    let remote = Target::Label(label("refs/remotes/origin/feature"));
    let local = Target::Label(label("refs/heads/feature"));
    let tag = Target::Label(label("refs/tags/v2"));
    let stash = Target::Stash(stash_row());
    let cases: Vec<(ActionId, Target, Vec<&str>)> = vec![
        (
            A::Tag,
            on_commit(),
            vec![
                "Name",
                "Kind (•) annotated  ( ) lightweight",
                "Message",
                "Push to (•) no remote  ( ) origin  ( ) 백업",
            ],
        ),
        (A::Branch, on_commit(), vec!["Name", "[ ] Check it out"]),
        (A::MoveBranch, on_commit(), vec!["Branch (•) feature"]),
        (
            A::Checkout,
            on_commit(),
            vec!["[ ] Don't ask again in this window"],
        ),
        (
            A::Pick,
            merge.clone(),
            vec![
                "2 commits, oldest first",
                "Parent (•) 1  ( ) 2",
                "[ ] Record origin (-x)  [ ] Don't commit",
            ],
        ),
        (
            A::Revert,
            merge.clone(),
            vec!["2 commits, newest first", "Parent (•) 1  ( ) 2"],
        ),
        (
            A::Merge,
            on_commit(),
            vec![
                "Fast-forward (•) when possible  ( ) never  ( ) only",
                "On conflicts prefer (•) nothing  ( ) ours  ( ) theirs",
                "[ ] Squash",
                "[ ] Don't commit",
                "[ ] Allow unrelated histories",
                "[ ] Skip hooks",
            ],
        ),
        (
            A::Rebase,
            on_commit(),
            vec![
                "[ ] Ignore dates",
                "[X] Stash my changes for it",
                "[ ] Move other branches along",
                "[ Run ]  [ Cancel ]  [ Plan the commits first… ]",
            ],
        ),
        (
            A::Reset,
            on_commit(),
            vec!["Mode ( ) soft  (•) mixed  ( ) keep  ( ) hard"],
        ),
        (
            A::Archive,
            on_commit(),
            vec!["Format (•) zip  ( ) tar", "Save to", "sopen-bbbbbbbb.zip"],
        ),
        (A::Rename, local.clone(), vec!["New name", "feature"]),
        (
            A::DeleteBranch,
            local.clone(),
            vec!["[ ] Force", "Also delete it on [ ] origin"],
        ),
        (
            A::Push,
            local.clone(),
            vec![
                "Remotes [X] origin  [ ] 백업",
                "[X] Set upstream",
                "[ ] Overwrite the remote branch, with lease",
                "[ ] Skip hooks",
            ],
        ),
        (
            A::CheckoutRemote,
            remote.clone(),
            vec![
                "Local name",
                "feature exists: check it out and pull origin/feature into it",
                "How (•) merge  ( ) rebase  ( ) fast-forward only",
                "[ ] Never fast-forward  [ ] Squash",
            ],
        ),
        (
            A::Pull,
            remote.clone(),
            vec![
                "How (•) merge  ( ) rebase  ( ) fast-forward only",
                "[ ] Never fast-forward  [ ] Squash",
            ],
        ),
        (
            A::DeleteTag,
            tag.clone(),
            vec!["Also on (•) no remote  ( ) origin  ( ) 백업"],
        ),
        (
            A::PushTag,
            tag.clone(),
            vec![
                "Remotes [X] origin  [ ] 백업",
                "[ ] Replace the remote's tag when it differs, with lease",
            ],
        ),
        (
            A::StashApply,
            stash.clone(),
            vec!["[ ] Restore the index too (--index)"],
        ),
        (
            A::StashPop,
            stash.clone(),
            vec!["[ ] Restore the index too (--index)"],
        ),
        (A::StashBranch, stash.clone(), vec!["Name"]),
    ];
    for (id, target, shows) in cases {
        let fx = Fx::new(open(id, target, &d));
        let all = fx.shown();
        for text in shows {
            assert!(all.contains(text), "{id:?}: {text:?} not in\n{all}");
        }
        same_in_a_terminal(&fx.drv);
    }
    // Fetch into a local branch: its force, on a remote branch whose local one is not checked
    // out.
    let fx = Fx::new(open(A::FetchInto, remote, &d));
    let all = fx.shown();
    assert!(
        all.contains("[ ] Force, when it is not a fast-forward"),
        "{all}"
    );
}

/// The form's first text field has the keyboard as the dialog opens: Tag's name, the stash
/// branch's, a rename's new name, Save to.
#[test]
fn x4d2_the_first_text_field_has_the_keyboard() {
    let d = dirty();
    let local = Target::Label(label("refs/heads/feature"));
    for (id, target) in [
        (A::Tag, on_commit()),
        (A::Branch, on_commit()),
        (A::Archive, on_commit()),
        (A::Rename, local),
        (A::StashBranch, Target::Stash(stash_row())),
    ] {
        let fx = Fx::new(open(id, target, &d));
        assert_eq!(fx.focused(), Some(dialogs::field_id(0)), "{id:?}");
    }
    let fx = Fx::new(open(A::Merge, on_commit(), &d));
    assert_eq!(fx.focused(), None, "no field: nothing has it");
}

// ---------------------------------------------------------------------------------------------
// The forms' options.

/// Merge's options by the keyboard, presses in the pass's order: `[Space, Tab, Space]` from
/// "never" chooses never, then "only", which the Tab reached; each change plans again, and Enter
/// runs what the dialog then shows.
#[test]
fn x4d2_merge_s_options_by_the_keyboard_plan_again() {
    let mut fx = Fx::new(open(A::Merge, on_commit(), &dirty()));
    fx.focus(choice_id(dialogs::row_id(0), 1));
    let mut events = space();
    events.push(down(Key::Tab));
    events.extend(space());
    fx.frame(events);
    let Action::Merge { options, .. } = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(options.fast_forward, FastForward::Only);
    assert!(fx
        .shown()
        .contains("Fast-forward ( ) when possible  ( ) never  (•) only"));
    assert!(matches!(
        asked().last(),
        Some(Action::Merge {
            options: MergeOptions {
                fast_forward: FastForward::Only,
                ..
            },
            ..
        })
    ));
    fx.click("[ ] Allow unrelated histories");
    fx.click("( ) theirs");
    fx.press(Key::Enter);
    let Some(Action::Merge { options, .. }) = fx.ran() else {
        panic!("{:?}", fx.host.ends);
    };
    assert!(options.allow_unrelated);
    assert_eq!(options.prefer, crate::git::actions::Prefer::Theirs);
}

/// Reset's mode: a click chooses it and plans again; Enter runs it.
#[test]
fn x4d2_reset_s_mode_runs_as_chosen() {
    let mut fx = Fx::new(open(A::Reset, on_commit(), &dirty()));
    fx.click("( ) hard");
    assert!(fx
        .shown()
        .contains("Mode ( ) soft  ( ) mixed  ( ) keep  (•) hard"));
    fx.press(Key::Enter);
    assert_eq!(
        fx.ran(),
        Some(Action::Reset {
            to: oid('b'),
            mode: crate::git::actions::ResetMode::Hard
        })
    );
}

/// Tag: the name has the typing; lightweight drops the message field and the message, annotated
/// brings back what was typed; Push to names a remote.
#[test]
fn x4d2_tag_s_kind_keeps_its_message_and_push_names_a_remote() {
    let mut fx = Fx::new(open(A::Tag, on_commit(), &dirty()));
    fx.frame(vec![Event::Text("v3".into())]);
    fx.focus(dialogs::field_id(1));
    fx.frame(vec![Event::Text("릴리스 노트".into())]);
    assert!(matches!(
        fx.draft(),
        Action::Tag { name, message: Some(m), push: None, .. } if name == "v3" && m == "릴리스 노트"
    ));
    assert_eq!(pen_of(&fx.boxed(), "릴리스 노트"), field_pen());
    fx.click("( ) lightweight");
    assert!(matches!(fx.draft(), Action::Tag { message: None, .. }));
    assert!(!fx.shown().contains("릴리스 노트"), "{}", fx.shown());
    fx.click("( ) annotated");
    assert!(matches!(fx.draft(), Action::Tag { message: Some(m), .. } if m == "릴리스 노트"));
    fx.click("( ) 백업");
    fx.press(Key::Enter);
    assert!(
        matches!(fx.ran(), Some(Action::Tag { push: Some(r), .. }) if r == "백업"),
        "{:?}",
        fx.host.ends
    );
}

/// Push's remotes, any of several in the remotes' order; its options.
#[test]
fn x4d2_push_s_remotes_and_options() {
    let local = Target::Label(label("refs/heads/feature"));
    let mut fx = Fx::new(open(A::Push, local, &dirty()));
    fx.click("[ ] 백업");
    fx.click("[ ] Overwrite the remote branch, with lease");
    fx.click("[X] Set upstream");
    let Action::Push {
        remotes,
        options,
        overwrite,
        ..
    } = fx.draft()
    else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(remotes, ["origin", "백업"]);
    assert!(overwrite && !options.set_upstream);
    // origin off: 백업 alone.
    fx.focus(choice_id(dialogs::row_id(0), 0));
    fx.frame(space());
    assert!(matches!(fx.draft(), Action::Push { remotes, .. } if remotes == ["백업"]));
}

/// Archive: the format changes the path's extension; the path is the draft's.
#[test]
fn x4d2_archive_s_format_changes_its_path() {
    let prefill = Prefill {
        save_dir: "/tmp/내려받기".into(),
        ..Prefill::default()
    };
    let mut fx = Fx::new(open_with(A::Archive, on_commit(), &dirty(), &prefill));
    fx.click("( ) tar");
    let Action::Archive { format, to, .. } = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(format, ArchiveFormat::Tar);
    assert_eq!(to, std::path::Path::new("/tmp/내려받기/sopen-bbbbbbbb.tar"));
    assert!(fx.shown().contains("/tmp/내려받기/sopen-bbbbbbbb.tar"));
}

/// Check out as a local branch: a name that is taken offers the pull into it (its how kept); a
/// new name drops it.
#[test]
fn x4d2_checkout_remote_offers_its_pull_while_the_name_is_taken() {
    let remote = Target::Label(label("refs/remotes/origin/feature"));
    let mut fx = Fx::new(open(A::CheckoutRemote, remote, &dirty()));
    fx.click("( ) rebase");
    assert!(matches!(
        fx.draft(),
        Action::CheckoutRemote {
            pull: Some(PullHow::Rebase),
            ..
        }
    ));
    // A new name: no pull, and its row is gone.
    fx.focus(dialogs::field_id(0));
    fx.drv.key(&mut fx.host, Key::A, testkit::command(false));
    fx.frame(vec![Event::Text("새-기능".into())]);
    assert!(matches!(
        fx.draft(),
        Action::CheckoutRemote { pull: None, name, .. } if name == "새-기능"
    ));
    assert!(!fx.shown().contains("How"), "{}", fx.shown());
    // Taken again: the pull comes back as it was chosen.
    fx.drv.key(&mut fx.host, Key::A, testkit::command(false));
    fx.frame(vec![Event::Text("feature".into())]);
    assert!(matches!(
        fx.draft(),
        Action::CheckoutRemote {
            pull: Some(PullHow::Rebase),
            ..
        }
    ));
}

/// Delete a branch: also on each remote that has it, chosen.
#[test]
fn x4d2_delete_branch_also_on_a_remote() {
    let local = Target::Label(label("refs/heads/feature"));
    let mut fx = Fx::new(open(A::DeleteBranch, local, &dirty()));
    fx.click("[ ] origin");
    fx.click("[ ] Force");
    fx.press(Key::Enter);
    assert_eq!(
        fx.ran(),
        Some(Action::DeleteBranch {
            branch: "feature".into(),
            force: true,
            remotes: vec!["origin".into()]
        })
    );
}

/// Rebase's `[ Plan the commits first… ]`: its click, or Enter on it, opens the planner and runs
/// nothing.
#[test]
fn x4d2_rebase_s_plan_first_is_a_button_of_the_box() {
    let mut fx = Fx::new(open(A::Rebase, on_commit(), &dirty()));
    fx.click("[ Plan the commits first… ]");
    assert_eq!(fx.host.ends, [DialogEnd::PlanFirst(Rev::Commit(oid('b')))]);
    let mut fx = Fx::new(open(A::Rebase, on_commit(), &dirty()));
    fx.focus(dialogs::plan_first_id());
    fx.press(Key::Enter);
    assert_eq!(fx.host.ends, [DialogEnd::PlanFirst(Rev::Commit(oid('b')))]);
}

/// Check out (detached)'s "Don't ask again in this window" is what the screen showed too: chosen
/// and Enter in one pass, nothing runs and the hint says so; the next Enter runs with it — so a
/// later checkout runs without asking only once the owner saw the choice.
#[test]
fn x4d2_dont_ask_again_runs_only_once_shown() {
    let mut fx = Fx::new(open(A::Checkout, on_commit(), &dirty()));
    fx.focus(choice_id(dialogs::row_id(0), 0));
    let mut events = space();
    events.push(down(Key::Enter));
    fx.raw(events);
    assert!(fx.host.ends.is_empty(), "ran unseen: {:?}", fx.host.ends);
    fx.raw(vec![]);
    assert!(fx.shown().contains(dialogs::CHANGED), "{}", fx.shown());
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(
        fx.host.ends,
        [DialogEnd::Run {
            action: Action::Checkout { at: oid('b') },
            dont_ask_again: true
        }]
    );
}

/// Review of X4-dialogs2: what the screen showed includes the title and the lost line, which a
/// plan can change while its rows stay — a refresh that checks out another branch in the pass of
/// an Enter changes Merge's `into <head>` alone: nothing runs, and the next Enter, once the new
/// title is on screen, runs.
#[test]
fn x4d2_a_refresh_that_changes_only_the_title_runs_nothing() {
    let d = ActionDialog::open(
        A::Merge,
        &on_commit(),
        &dirty(),
        &Prefill::default(),
        headed,
    );
    let mut fx = Fx::new(dialog(d));
    fx.host.planner = headed;
    assert!(
        fx.shown().contains("Merge into refs/heads/main"),
        "{}",
        fx.shown()
    );
    let mut moved = dirty();
    moved.head = crate::git::model::Head::Branch {
        refname: "refs/heads/feature".into(),
        lossy: false,
        oid: oid('b'),
    };
    fx.dialog_mut().set_repo(moved);
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "ran unseen: {:?}", fx.host.ends);
    assert!(
        fx.shown().contains("Merge into refs/heads/feature"),
        "{}",
        fx.shown()
    );
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.ran().is_some(), "{:?}", fx.host.ends);
}

/// Move tag keeps the chosen tag's own message (03 §7.4): switching from annotated `alpha` to
/// annotated `beta` loads beta's, never alpha's; an annotated tag whose message is not loaded is
/// refused; a lightweight tag moves lightweight; an edit stays with its tag; and text typed in
/// the pass that chooses another tag lands nowhere, never under the tag chosen (moved from
/// `tests_s3.rs`, in cells).
#[test]
fn x4d2_move_tag_keeps_the_chosen_tag_s_own_message() {
    let (r, prefill) = tags();
    let bar = Target::Commit {
        commit: commit('c', &['b'], "c"),
        marked: Vec::new(),
    };
    let open = || open_with(A::MoveTag, bar.clone(), &r, &prefill);
    let move_tag = |tag: &str, message: Option<&str>| Action::MoveTag {
        tag: tag.into(),
        to: oid('c'),
        message: message.map(str::to_string),
    };
    assert_eq!(open().draft(), &move_tag("alpha", Some("release alpha")));

    // alpha → beta: Run carries beta's message.
    let mut fx = Fx::new(open());
    assert!(fx.shown().contains("release alpha"));
    assert_eq!(fx.focused(), Some(dialogs::field_id(0)), "the message");
    fx.click("( ) beta");
    assert!(fx.shown().contains("release beta"));
    assert!(!fx.shown().contains("release alpha"));
    fx.press(Key::Enter);
    assert_eq!(fx.ran(), Some(move_tag("beta", Some("release beta"))));

    // gamma's message is not loaded: refused, never alpha's text.
    let mut fx = Fx::new(open());
    fx.click("( ) gamma");
    let why = format!("gamma: {}", dialogs::NOT_LOADED);
    assert_eq!(pen_of(&fx.boxed(), &why), error_pen());
    fx.press(Key::Enter);
    assert!(fx.host.ends.is_empty(), "Run is off");
    assert_eq!(fx.draft(), move_tag("gamma", Some("")));
    // Moving it lightweight instead is not refused.
    fx.click("[X] Keep it annotated with its message");
    assert!(!fx.shown().contains(&why));
    fx.press(Key::Enter);
    assert_eq!(fx.ran(), Some(move_tag("gamma", None)));

    // A lightweight tag moves lightweight; an edit to alpha stays alpha's across a switch.
    let mut fx = Fx::new(open());
    fx.frame(vec![down(Key::End), Event::Text(" v2".into())]);
    fx.click("( ) light");
    assert_eq!(fx.draft(), move_tag("light", None));
    fx.click("( ) alpha");
    fx.press(Key::Enter);
    assert_eq!(fx.ran(), Some(move_tag("alpha", Some("release alpha v2"))));

    // Typed in the very pass whose click chooses another tag (one frame: the text, then the press
    // and the release on `( ) light`). egui gives the choice the keyboard before the message reads
    // the pass, so the text lands nowhere — never under the tag chosen.
    let mut fx = Fx::new(open());
    let light = place(&fx.boxed(), "( ) light");
    let at = at_cell(light.x, light.y);
    fx.drv.move_to(&mut fx.host, at);
    fx.frame(vec![
        down(Key::End),
        Event::Text(" v2".into()),
        pointer(at, true),
        pointer(at, false),
    ]);
    assert_eq!(fx.draft(), move_tag("light", None));
    fx.click("[ ] Keep it annotated with its message");
    assert_eq!(
        fx.draft(),
        move_tag("light", Some("")),
        "light's own: none read"
    );
    fx.click("( ) alpha");
    assert_eq!(fx.draft(), move_tag("alpha", Some("release alpha")));

    // A new tag starts with an empty message, not the prefill's commit message.
    let d = open_with(A::Tag, bar.clone(), &r, &prefill);
    assert!(matches!(d.draft(), Action::Tag { message: Some(m), .. } if m.is_empty()));
}

/// Move tag's fixture: annotated `alpha` and `beta` with their messages read, annotated `gamma`
/// whose message is not, lightweight `light`; the prefill's commit message is another dialog's.
fn tags() -> (Repo, Prefill) {
    let mut r = repo();
    let tag = |name: &str, annotated: bool| {
        let mut t = label("refs/tags/v2");
        t.refname = format!("refs/tags/{name}");
        if !annotated {
            t.object = oid('a');
            t.object_type = crate::git::model::ObjectType::Commit;
            t.peeled = None;
        }
        t
    };
    r.refs.retain(|l| l.kind != crate::git::model::RefKind::Tag);
    r.refs.extend([
        tag("alpha", true),
        tag("beta", true),
        tag("gamma", true),
        tag("light", false),
    ]);
    let prefill = Prefill {
        tag_messages: [("alpha", "release alpha"), ("beta", "release beta")]
            .into_iter()
            .map(|(t, m)| (t.to_string(), m.to_string()))
            .collect(),
        // A commit message meant for another dialog is never a tag's.
        message: Some("a commit message".into()),
        ..Prefill::default()
    };
    (r, prefill)
}

/// The painting the dialog compares what would run with is a revision, not a copy (the
/// coordinator's decision, X4-quick's review): with Amend's message of a mebibyte open, ten idle
/// passes copy and compare none of it — and the guard stays exact: a letter typed and Enter in one
/// pass runs nothing, the next Enter runs the message with it.
#[test]
fn x4d2_idle_passes_copy_no_draft() {
    let line = "한글 메시지의 한 줄과 an English part of the same line, 길게 이어집니다\n";
    let message = line.repeat((1 << 20) / line.len() + 1);
    let prefill = Prefill {
        message: Some(message.clone()),
        ..Prefill::default()
    };
    let amend = ActionDialog::open(A::Amend, &Target::Uncommitted, &dirty(), &prefill, brief);
    let mut fx = Fx::new(dialog(amend));
    fx.host.planner = brief;
    fx.click("( ) edit it");
    fx.idle(2);
    assert!(matches!(fx.draft(), Action::Amend { message: Some(m) } if m.len() > 1 << 20));
    dialogs::copied();
    fx.idle(10);
    assert_eq!(
        dialogs::copied(),
        0,
        "bytes copied or compared in ten idle passes"
    );
    fx.raw(vec![Event::Text("!".into()), down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "ran unseen: {:?}", fx.host.ends);
    fx.raw(vec![]);
    fx.raw(vec![down(Key::Enter)]);
    let Some(Action::Amend { message: Some(m) }) = fx.ran() else {
        panic!("{:?}", fx.host.ends);
    };
    assert_eq!(m, format!("!{message}"));
}

/// Every field of an action's dialog opens afresh — caret, selection and undo cleared — whatever
/// an earlier dialog left under the same ids (found by X4-quick; the coordinator's decision): Edit
/// a remote on `origin`, its name's caret moved to the start and a letter typed, cancelled; opened
/// again on `백업`, what is typed lands at the end of its name, where a new field's caret is.
#[test]
fn x4d2_a_dialog_s_fields_open_afresh() {
    let remote = |name: &str, url: &str| Remote {
        name: name.into(),
        url: url.into(),
        push_url: None,
    };
    let prefill = Prefill {
        remotes: vec![
            remote("origin", "https://example.com/x.git"),
            remote("백업", "/srv/백업.git"),
        ],
        ..Prefill::default()
    };
    let edit = |repo: &Repo| open_with(A::EditRemote, Target::Repository, repo, &prefill);
    let mut fx = Fx::new(edit(&dirty()));
    assert_eq!(fx.focused(), Some(dialogs::field_id(0)), "the name");
    fx.press(Key::Home);
    fx.frame(vec![Event::Text("x".into())]);
    assert!(
        matches!(fx.draft(), Action::EditRemote { now, .. } if now.name == "xorigin"),
        "{:?}",
        fx.draft()
    );
    fx.press(Key::Escape);
    assert_eq!(fx.host.ends, [DialogEnd::Cancel]);
    // Again, on 백업: the repository's first remote now.
    let mut other = dirty();
    other.remotes = vec!["백업".into(), "origin".into()];
    fx.host.over = Some(Over::Action(Box::new(edit(&other))));
    fx.idle(2);
    fx.frame(vec![Event::Text("y".into())]);
    assert!(
        matches!(fx.draft(), Action::EditRemote { now, .. } if now.name == "백업y"),
        "{:?}",
        fx.draft()
    );
}

// ---------------------------------------------------------------------------------------------
// The result dialog (03 §3.8, §8.3).

fn done(line: &str, code: i32, stderr: &str) -> Done {
    Done {
        ending: Ending::Exit(code),
        stdout: Vec::new(),
        stdout_cut: false,
        stderr: stderr.into(),
        entry: super::tests_s3::entry(line, Ending::Exit(code), stderr, 0),
    }
}

/// A run of three steps whose second failed (`failed`) or whose every step was done.
fn outcome(failed: bool) -> Outcome {
    let push = "git push --progress origin refs/heads/b:refs/heads/b";
    let error = "error: failed to push some refs to 'origin'\nhint: fetch first";
    let (code, stderr) = if failed { (1, error) } else { (0, "") };
    let (second, third) = if failed {
        (StepOutcome::Failed, StepOutcome::NotRun)
    } else {
        (StepOutcome::Done, StepOutcome::Done)
    };
    Outcome {
        steps: vec![
            StepResult {
                line: "backup".into(),
                outcome: StepOutcome::Done,
                ran: vec![done("git update-ref --stdin", 0, "")],
            },
            StepResult {
                line: push.into(),
                outcome: second,
                ran: vec![done(push, code, stderr)],
            },
            StepResult {
                line: "git branch -d b".into(),
                outcome: third,
                ran: Vec::new(),
            },
        ],
        backup: Some("20261123T102405Z".into()),
    }
}

/// A failed step (03 §3.8) in cells: the box titled with the action lists each step `done`,
/// `failed` (in the error colour) or `not run`, each word two blanks before its line; git's
/// stderr a row a line; the log entry's command line in the weak colour; the backup line; `[ OK ]`
/// and the hint. The same cells in a terminal; Enter closes it. Every step done: no dialog
/// (moved from `tests_s3.rs`, in cells).
#[test]
fn x4d2_the_result_dialog_lists_each_step_and_the_error() {
    assert!(ResultDialog::of("Delete branch b", &outcome(false)).is_none());
    assert_eq!(
        dialogs::status_text("Deleted branch b", &outcome(false)),
        "Deleted branch b · backup 20261123T102405Z · Ctrl/⌘+Z undoes"
    );
    let mut quiet_outcome = outcome(false);
    quiet_outcome.backup = None;
    assert_eq!(
        dialogs::status_text("Fetched", &quiet_outcome),
        "Fetched",
        "no backup, no undo"
    );
    let result = ResultDialog::of("Delete branch b", &outcome(true)).expect("a step failed");
    let mut fx = Fx::result(result);
    let l = fx.boxed();
    let all = rows(&l).join("\n");
    let push = "git push --progress origin refs/heads/b:refs/heads/b";
    for text in [
        "Delete branch b",
        "done     backup",
        &format!("failed   {push}"),
        "not run  git branch -d b",
        "error: failed to push some refs to 'origin'",
        "hint: fetch first",
        "backup 20261123T102405Z · Ctrl/⌘+Z undoes",
        "[ OK ]",
        "Enter or Esc closes",
    ] {
        assert!(all.contains(text), "{text:?} not in\n{all}");
    }
    assert_eq!(pen_of(&l, "failed"), error_pen());
    assert_eq!(pen_of(&l, "not run"), text_pen());
    assert_eq!(pen_of(&l, "Enter or Esc closes"), weak_pen());
    // The command line under the error, in the weak colour.
    let hint = place(&l, "hint: fetch first");
    let line = (rows(&l).iter().enumerate())
        .skip(usize::from(hint.y - l.buf.area.y) + 1)
        .find(|(_, r)| r.contains(push))
        .map(|(k, _)| k as u16 + l.buf.area.y)
        .expect("the command line under the error");
    let cells = cells::find(std::slice::from_ref(&l), push);
    let under = cells.iter().find(|h| h.cells.y == line).expect("found");
    let a = l.buf.area;
    let seen = &l.seen()[usize::from(under.cells.y - a.y)][usize::from(under.cells.x - a.x)];
    assert_eq!((seen.fg, seen.bg), weak_pen());
    same_in_a_terminal(&fx.drv);
    assert_eq!(fx.host.covers.len(), 3, "the box and its shadow");
    fx.press(Key::Enter);
    assert_eq!(fx.host.closed, 1);

    let mut fx = Fx::result(ResultDialog::error(
        "Backups",
        "fatal: not a git repository",
        Some("git for-each-ref refs/sopen-backup/"),
    ));
    let all = fx.shown();
    assert!(all.contains("fatal: not a git repository"), "{all}");
    assert!(all.contains("git for-each-ref refs/sopen-backup/"), "{all}");
    same_in_a_terminal(&fx.drv);
    fx.press(Key::Escape);
    assert_eq!(fx.host.closed, 1);
    // OK, and a click outside the box, close it too.
    let mut fx = Fx::result(ResultDialog::error("Backups", "fatal: x", None));
    fx.click("[ OK ]");
    assert_eq!(fx.host.closed, 1);
    let mut fx = Fx::result(ResultDialog::error("Backups", "fatal: x", None));
    fx.drv.click(&mut fx.host, at_cell(1, 1));
    fx.idle(1);
    assert_eq!(fx.host.closed, 1);
}

/// The result dialog masks whatever it shows (03 §3.7), a URL the owner typed included (moved
/// from `tests_s3.rs`, in cells).
#[test]
fn x4d2_the_result_dialog_masks_credentials() {
    let secret = "https://ada:s3cret@example.com/x.git";
    let masked = "https://ada:***@example.com/x.git";
    let mut failed = outcome(true);
    failed.steps[1].line = format!("git remote add origin {secret}");
    failed.steps[1].ran[0].entry.line = format!("git remote add origin {secret}");
    failed.steps[1].ran[0].stderr = format!("fatal: remote origin already exists: {secret}");
    let fx = Fx::result(ResultDialog::of("Add a remote", &failed).unwrap());
    let all = fx.shown();
    assert!(
        all.contains(&format!("git remote add origin {masked}")),
        "{all}"
    );
    assert!(
        all.contains(&format!("fatal: remote origin already exists: {masked}")),
        "{all}"
    );
    assert!(!all.contains("s3cret"), "{all}");
    let fx = Fx::result(ResultDialog::error(
        "Edit a remote",
        &format!("fatal: could not read {secret}"),
        Some(&format!("git remote set-url origin {secret}")),
    ));
    let all = fx.shown();
    assert!(all.contains(&format!("fatal: could not read {masked}")));
    assert!(all.contains(&format!("git remote set-url origin {masked}")));
    assert!(!all.contains("s3cret"), "{all}");
}

/// Check out of a branch refused over a local change (the row's receipt's result): one step,
/// failed, no backup written — so no backup line — and git's stderr whole: its tab stands at the
/// eighth cell, a control character escaped.
#[test]
fn x4d2_a_refused_checkout_s_result_has_no_backup_line() {
    let line = "git checkout --no-overwrite-ignore 이전";
    let stderr = "error: Your local changes to the following files would be overwritten by \
                  checkout:\n\ta.txt\nPlease commit your changes or stash them before you switch \
                  branches.\nAborting\u{7}";
    let refused = Outcome {
        steps: vec![StepResult {
            line: line.into(),
            outcome: StepOutcome::Failed,
            ran: vec![done(line, 1, stderr)],
        }],
        backup: None,
    };
    let fx = Fx::result(ResultDialog::of("Check out 이전", &refused).unwrap());
    let all = fx.shown();
    assert!(all.contains(&format!("failed   {line}")), "{all}");
    assert!(all.contains("        a.txt"), "the tab: {all}");
    assert!(all.contains("Aborting\\u{7}"), "escaped: {all}");
    assert!(!all.contains("backup"), "{all}");
    same_in_a_terminal(&fx.drv);
}

/// A result's text scrolls whole (03 §3.8): past what the box holds, the wheel over it and the
/// scroll bar on the border bring the rest, its last line too; the box stays inside the grid.
#[test]
fn x4d2_the_result_s_text_scrolls_whole() {
    let text: String = (1..=120)
        .map(|k| format!("line {k:03} of git's stderr\n"))
        .collect();
    let mut fx = Fx::result(ResultDialog::error(
        "Fetch",
        &text,
        Some("git fetch origin"),
    ));
    let l = fx.boxed();
    let all = rows(&l).join("\n");
    assert!(all.contains("line 001"), "{all}");
    assert!(!all.contains("line 120"), "{all}");
    assert!(fx.host.covers[0].bottom() <= 30, "{:?}", fx.host.covers);
    let border = fx.host.covers[0].right() - 1;
    let first = place(&l, "line 001");
    let seen = l.seen();
    let a = l.buf.area;
    let at = |y: u16| {
        seen[usize::from(y - a.y)][usize::from(border - a.x)]
            .symbol
            .clone()
    };
    assert_eq!(at(first.y), "↑", "the scroll bar on the border");
    // The wheel, a notch at a time, to the end.
    let middle = at_cell(first.x + 5, first.y + 2);
    for _ in 0..60 {
        fx.drv.wheel(&mut fx.host, middle, -1.0);
    }
    let all = fx.shown();
    assert!(all.contains("line 120"), "{all}");
    assert!(all.contains("git fetch origin"), "{all}");
    same_in_a_terminal(&fx.drv);
}

// ---------------------------------------------------------------------------------------------
// In the panel: the result and the stop question.

/// The panel blanks under its result and its stop question, and each stands over it in cells.
#[test]
fn x4d2_the_panel_blanks_under_the_result_and_the_stop_question() {
    for stop in [false, true] {
        let mut view = super::tests::fixture_view();
        view.set_repo(dirty());
        let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
        if stop {
            fx.host.view.ask_stop("Commit".into(), 7);
        } else {
            (fx.host.view).show_text("Backups", "fatal: not a git repository", None);
        }
        fx.idle();
        fx.idle();
        let grid = Grid::over(fx.host.rect);
        let covered = fx.host.view.covered(&grid);
        assert_eq!(covered.len(), 3, "the box and its shadow");
        let panel = super::tests_x4_graph::panel(&fx);
        let seen = panel.seen();
        for r in &covered {
            for y in r.top()..r.bottom() {
                for x in r.left()..r.right() {
                    let s = &seen[usize::from(y)][usize::from(x)];
                    assert_eq!(
                        (s.symbol.as_str(), s.bg),
                        (" ", Some(PALETTE.back)),
                        "({x}, {y})"
                    );
                }
            }
        }
        let l = boxed(&fx.drv).expect("the dialog in cells");
        assert_eq!(l.buf.area.x, covered[0].x);
    }
}

/// "Stop git?" (03 §3.4) in cells: what runs, what stopping may leave, `[ Yes, stop it ]` and
/// `[ No ]`, the hint. Enter or Yes stops the write; Esc, No or a click outside lets it run; Enter
/// on the focused No is No.
#[test]
fn x4d2_the_stop_question_is_cells_and_its_answers() {
    let question = || {
        let mut view = super::tests::fixture_view();
        view.set_repo(dirty());
        let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
        fx.host.view.ask_stop("Commit".into(), 7);
        fx.idle();
        fx.idle();
        fx
    };
    let mut fx = question();
    let l = boxed(&fx.drv).expect("in cells");
    let all = rows(&l).join("\n");
    for text in [
        "Stop git?",
        "Commit is still running.",
        "It may stop between steps; the panel then shows the state it left.",
        "[ Yes, stop it ]  [ No ]",
        dialogs::STOP_HINT,
    ] {
        assert!(all.contains(text), "{text:?} not in\n{all}");
    }
    assert_eq!(pen_of(&l, dialogs::STOP_HINT), weak_pen());
    assert_eq!(fx.press(Key::Enter), [GitAction::StopWrite(7)]);
    assert!(!fx.host.view.dialog_up());

    let mut fx = question();
    assert!(fx.press(Key::Escape).is_empty());
    assert!(!fx.host.view.dialog_up(), "Esc lets it run");

    let mut fx = question();
    fx.drv.click_text_on_top(&mut fx.host, "[ Yes, stop it ]");
    fx.idle();
    assert_eq!(fx.take(), [GitAction::StopWrite(7)]);

    let mut fx = question();
    fx.drv.click_text_on_top(&mut fx.host, "[ No ]");
    fx.idle();
    assert!(fx.take().is_empty());
    assert!(!fx.host.view.dialog_up());

    let mut fx = question();
    fx.drv
        .ctx
        .memory_mut(|m| m.request_focus(dialogs::stop_no_id()));
    fx.idle();
    fx.idle();
    assert!(fx.press(Key::Enter).is_empty(), "Enter on No");
    assert!(!fx.host.view.dialog_up());
}

/// The question comes up on its own, over a write the owner pressed Esc for: an Enter in its first
/// passes — laid out unseen, then the first it shows — stops nothing; the next one does.
#[test]
fn x4d2_an_enter_before_the_question_showed_stops_nothing() {
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
    fx.host.view.ask_stop("Commit".into(), 7);
    for _ in 0..2 {
        fx.drv
            .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
        assert!(fx.take().is_empty(), "stopped unseen");
    }
    assert!(fx.host.view.dialog_up());
    fx.drv
        .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
    assert_eq!(fx.take(), [GitAction::StopWrite(7)]);
}

/// Review of X4-dialogs2: a question answered and asked again in the very next pass over the same
/// write starts afresh — an Enter in that pass stops nothing.
#[test]
fn x4d2_a_stop_question_asked_again_at_once_starts_afresh() {
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
    fx.host.view.ask_stop("Commit".into(), 7);
    fx.drv.idle(&mut fx.host, 3);
    fx.drv
        .frame(&mut fx.host, vec![down(Key::Escape)], Modifiers::NONE);
    assert!(!fx.host.view.dialog_up(), "answered No");
    fx.host.view.ask_stop("Commit".into(), 7);
    fx.drv
        .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
    assert!(fx.take().is_empty(), "stopped what it had not shown yet");
}

/// A terminal for the stop question's burst: it sends its bursts back to back — each waiting
/// while the next is unread, so the backend prints only the last pass of a burst — one burst once
/// the backend has asked `gap` times since the last (its pass is done and printed), then ends.
struct Bursts {
    bursts: std::collections::VecDeque<Vec<crossterm::event::Event>>,
    now: std::collections::VecDeque<crossterm::event::Event>,
    asked: usize,
    /// The asks, a millisecond each, before the next burst: 3 sends it before the terminal's 8 ms
    /// floor lets a pass egui asked for run; 30 lets them run first (F-git-outcome).
    gap: usize,
    /// The stops asked so far when each burst was sent.
    stops: std::rc::Rc<std::cell::Cell<usize>>,
    sent_at: std::rc::Rc<RefCell<Vec<usize>>>,
}

impl term::Tty for Bursts {
    fn raw(&mut self, _on: bool) -> std::io::Result<()> {
        Ok(())
    }
    fn size(&mut self) -> std::io::Result<(u16, u16)> {
        Ok((100, 30))
    }
    fn probe(&mut self) -> term::Probed {
        term::Probed {
            kitty: false,
            ambiguous: false,
        }
    }
    fn drop_input(&mut self) {}
    fn next(
        &mut self,
        _wait: std::time::Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        if let Some(ev) = self.now.pop_front() {
            return Ok(Some(ev));
        }
        self.asked += 1;
        assert!(self.asked < 20_000, "the script ran out without an end");
        if self.asked >= self.gap {
            if let Some(burst) = self.bursts.pop_front() {
                self.sent_at.borrow_mut().push(self.stops.get());
                self.asked = 0;
                self.now.extend(burst);
                return Ok(self.now.pop_front());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        Ok(None)
    }
    fn pending(&mut self) -> std::io::Result<bool> {
        Ok(!self.now.is_empty())
    }
    fn stopped(&self) -> Option<i32> {
        (self.bursts.is_empty() && self.now.is_empty() && self.asked >= self.gap)
            .then_some(libc::SIGTERM)
    }
}

/// The git panel in the terminal, counting the stops it asked for.
struct InTerminal {
    host: super::tests::Host,
    stops: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Window for InTerminal {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.host.ui(ui);
        let stops = (self.host.actions.iter())
            .filter(|a| matches!(a, GitAction::StopWrite(7)))
            .count();
        self.stops.set(stops);
    }

    fn ready(&self) -> bool {
        true
    }
}

/// Through the terminal: the question is up, and `[Enter, Enter]` come back to back before its
/// first pass is printed — neither stops git; an Enter once it is printed does.
#[test]
fn x4d2_a_stop_the_terminal_never_printed_was_never_asked() {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
    let enter = CEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let stops = std::rc::Rc::new(std::cell::Cell::new(0));
    let sent_at = std::rc::Rc::new(RefCell::new(Vec::new()));
    let tty = Bursts {
        bursts: [vec![enter.clone(), enter.clone()], vec![enter]].into(),
        now: Default::default(),
        // The first burst before any pass is printed.
        asked: 3,
        gap: 3,
        stops: stops.clone(),
        sent_at: sent_at.clone(),
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut b = term::Backend::new(tty, std::io::sink(), ctx, true);
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    view.ask_stop("Commit".into(), 7);
    let mut win = InTerminal {
        host: super::tests::Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: egui::Rect::NOTHING,
            under: Vec::new(),
            side: None,
        },
        stops,
    };
    let mut no_switch =
        |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, term::End::Signal(libc::SIGTERM));
    assert_eq!(*sent_at.borrow(), [0, 0], "nothing stopped before the last");
    let stopped = (win.host.actions.iter())
        .filter(|a| matches!(a, GitAction::StopWrite(7)))
        .count();
    assert_eq!(stopped, 1, "{:?}", win.host.actions);
}

// ---------------------------------------------------------------------------------------------
// Review round 1.

/// Edit a remote's dialog over `origin`, and over `백업` when the repository lists it first; both
/// remotes' URLs read.
fn remotes() -> (Prefill, Repo) {
    let remote = |name: &str, url: &str| Remote {
        name: name.into(),
        url: url.into(),
        push_url: None,
    };
    let prefill = Prefill {
        remotes: vec![
            remote("origin", "https://example.com/x.git"),
            remote("백업", "/srv/백업.git"),
        ],
        ..Prefill::default()
    };
    let mut other = dirty();
    other.remotes = vec!["백업".into(), "origin".into()];
    (prefill, other)
}

fn remote_name(fx: &Fx) -> String {
    match fx.draft() {
        Action::EditRemote { now, .. } => now.name,
        other => panic!("{other:?}"),
    }
}

/// A field reopened keeps no undo of the one before under its id: Cmd/Ctrl+Z in the reopened
/// name leaves `백업`.
#[test]
fn x4d2_a_reopened_field_has_nothing_to_undo() {
    let (prefill, other) = remotes();
    let edit = |repo: &Repo| open_with(A::EditRemote, Target::Repository, repo, &prefill);
    let mut fx = Fx::new(edit(&dirty()));
    fx.frame(vec![Event::Text("x".into())]);
    assert_eq!(remote_name(&fx), "originx");
    fx.press(Key::Escape);
    fx.host.over = Some(Over::Action(Box::new(edit(&other))));
    fx.idle(2);
    fx.drv.key(&mut fx.host, Key::Z, testkit::command(false));
    assert_eq!(remote_name(&fx), "백업", "nothing to undo");
}

/// A field reopened keeps no selection of the one before: all of the name selected and the
/// dialog cancelled, a letter typed in the reopened one is added, not typed over it.
#[test]
fn x4d2_a_reopened_field_has_nothing_selected() {
    let (prefill, other) = remotes();
    let edit = |repo: &Repo| open_with(A::EditRemote, Target::Repository, repo, &prefill);
    let mut fx = Fx::new(edit(&dirty()));
    fx.drv.key(&mut fx.host, Key::A, testkit::command(false));
    fx.press(Key::Escape);
    fx.host.over = Some(Over::Action(Box::new(edit(&other))));
    fx.idle(2);
    fx.frame(vec![Event::Text("y".into())]);
    assert_eq!(remote_name(&fx), "백업y", "nothing selected");
}

/// A message field reopened in the very next pass — which the kit's own reset of a field not laid
/// out the pass before does not reach — has its caret at its start (`fresh_fields`' message).
#[test]
fn x4d2_a_message_reopened_at_once_starts_afresh() {
    let commit = |message: Option<&str>| {
        let prefill = Prefill {
            message: message.map(String::from),
            ..Prefill::default()
        };
        open_with(A::Commit, Target::Uncommitted, &dirty(), &prefill)
    };
    let mut fx = Fx::new(commit(None));
    fx.frame(vec![Event::Text("abc".into())]);
    fx.raw(vec![down(Key::Escape)]);
    assert_eq!(fx.host.ends, [DialogEnd::Cancel]);
    fx.host.over = Some(Over::Action(Box::new(commit(Some("msg")))));
    fx.raw(vec![]);
    fx.raw(vec![]);
    fx.frame(vec![Event::Text("X".into())]);
    assert!(
        matches!(fx.draft(), Action::Commit { message, .. } if message == "Xmsg"),
        "{:?}",
        fx.draft()
    );
}

/// A field that shows after the dialog opened (Amend's message once "edit it" is chosen) starts
/// afresh too, whatever an earlier dialog's field under its id left: the kit resets a field not
/// laid out the pass before (a pin: it held before round 1).
#[test]
fn x4d2_a_field_shown_after_opening_starts_afresh() {
    let mut fx = Fx::new(open(A::Stash, Target::Uncommitted, &dirty()));
    fx.frame(vec![Event::Text("abc".into())]);
    fx.press(Key::Home);
    fx.press(Key::Escape);
    let prefill = Prefill {
        message: Some("old".into()),
        ..Prefill::default()
    };
    let amend = open_with(A::Amend, Target::Uncommitted, &dirty(), &prefill);
    fx.host.over = Some(Over::Action(Box::new(amend)));
    fx.idle(2);
    fx.click("( ) edit it");
    assert_eq!(fx.focused(), Some(dialogs::field_id(0)));
    fx.drv.key(&mut fx.host, Key::Z, testkit::command(false));
    fx.frame(vec![Event::Text("X".into())]);
    assert_eq!(
        fx.draft(),
        Action::Amend {
            message: Some("Xold".into())
        }
    );
}

/// A field whose text a choice swaps — Move tag's message for another tag's, Edit a remote's name
/// for another remote's — has nothing to undo: Cmd/Ctrl+Z there never brings back the tag's or the
/// remote's before.
#[test]
fn x4d2_a_swapped_field_has_nothing_to_undo() {
    let (r, prefill) = tags();
    let bar = Target::Commit {
        commit: commit('c', &['b'], "c"),
        marked: Vec::new(),
    };
    let mut fx = Fx::new(open_with(A::MoveTag, bar, &r, &prefill));
    fx.frame(vec![down(Key::End), Event::Text(" v2".into())]);
    fx.click("( ) beta");
    fx.focus(dialogs::field_id(0));
    fx.drv.key(&mut fx.host, Key::Z, testkit::command(false));
    assert!(
        matches!(fx.draft(), Action::MoveTag { message: Some(m), .. } if m == "release beta"),
        "{:?}",
        fx.draft()
    );
    let (prefill, _) = remotes();
    let edit = open_with(A::EditRemote, Target::Repository, &dirty(), &prefill);
    let mut fx = Fx::new(edit);
    fx.frame(vec![Event::Text("x".into())]);
    fx.click("( ) 백업");
    fx.focus(dialogs::field_id(0));
    fx.drv.key(&mut fx.host, Key::Z, testkit::command(false));
    assert_eq!(remote_name(&fx), "백업");
}

/// A run whose backup is incomplete (`Outcome::warning`) has its result too: the warning in the
/// error colour in place of the backup's undo line.
#[test]
fn x4d2_a_result_s_warning_replaces_its_backup_line() {
    let mut warned = outcome(false);
    let why = "backup 20261123T102405Z incomplete: refs/heads/b was not written";
    warned.steps[0].ran[0].ending = Ending::NotSent(why.into());
    let fx = Fx::result(ResultDialog::of("Delete branch b", &warned).expect("its warning"));
    let l = fx.boxed();
    assert_eq!(pen_of(&l, why), error_pen());
    let all = fx.shown();
    assert!(!all.contains("undoes"), "no backup line: {all}");
    same_in_a_terminal(&fx.drv);
}

/// The panel covers only the dialog it draws: `modals` draws the first of the menu, a quick edit,
/// the action's dialog, the result and "Stop git?", so a result that waits behind the menu (a run
/// failed while the menu was open) blanks nothing — `covered` is the menu's box and shadow alone
/// (none while the menu is egui's), and the graph beside the menu shows as it did.
#[test]
fn x4d2_a_result_behind_the_menu_blanks_nothing() {
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
    fx.host.view.open_menu();
    assert!(fx.host.view.menu.is_some(), "the menu");
    fx.idle();
    fx.idle();
    let alone = super::tests_x4_graph::panel(&fx).seen();
    (fx.host.view).show_text("Push", "error: failed to push some refs to 'origin'", None);
    fx.idle();
    fx.idle();
    let grid = Grid::over(fx.host.rect);
    let menu = (fx.host.view.menu.as_ref())
        .expect("still up")
        .covers(&grid);
    assert_eq!(fx.host.view.covered(&grid), menu, "the menu's box alone");
    assert!(
        super::tests_x4_graph::panel(&fx).seen() == alone,
        "the panel blanked cells for the result behind the menu"
    );
}

/// The result wraps its rows once a width, for its covers and its painting alike: with a 64 KiB
/// stderr and forty steps, ten idle passes wrap nothing; a narrower window wraps once — what a
/// result never laid out wraps for one look at that width — and the passes after it nothing.
#[test]
fn x4d2_the_result_wraps_once_a_width() {
    let stderr: String = (0..1024)
        .map(|k| format!("remote: line {k:04} of a long refusal\twith words that wrap\n"))
        .collect();
    assert!(stderr.len() >= 50 << 10, "{}", stderr.len());
    let push = |k: usize| format!("git push --progress origin refs/heads/b{k}:refs/heads/b{k}");
    let mut steps: Vec<StepResult> = (0..39)
        .map(|k| StepResult {
            line: push(k),
            outcome: StepOutcome::Done,
            ran: vec![done(&push(k), 0, "")],
        })
        .collect();
    steps.push(StepResult {
        line: push(39),
        outcome: StepOutcome::Failed,
        ran: vec![done(&push(39), 1, &stderr)],
    });
    let failed = Outcome {
        steps,
        backup: None,
    };
    let walked = || super::details::WALKED.with(|w| w.replace(0));
    let mut fx = Fx::result(ResultDialog::of("Push", &failed).expect("a step failed"));
    walked();
    fx.idle(10);
    assert_eq!(walked(), 0, "ten idle passes wrapped");
    fx.drv.resize(70.0 * CELL.x, 30.0 * CELL.y);
    fx.raw(vec![]);
    let resized = walked();
    let narrow = Grid::over(fx.drv.ctx.content_rect());
    let fresh = ResultDialog::of("Push", &failed).expect("a step failed");
    walked();
    fresh.covers(&narrow);
    let once = walked();
    assert!(once > 0);
    assert_eq!(resized, once, "the resize wrapped once");
    fx.idle(5);
    assert_eq!(walked(), 0);
}

/// Two results of the same run are equal whatever either has wrapped: the rows it keeps are no
/// part of it (the fix-check).
#[test]
fn x4d2_a_result_s_kept_rows_are_no_part_of_it() {
    let a = ResultDialog::error("Backups", "fatal: not a git repository", None);
    let b = a.clone();
    let screen = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 480.0));
    assert!(!a.covers(&Grid::over(screen)).is_empty());
    assert_eq!(a, b);
}

// ---------------------------------------------------------------------------------------------
// The release frame cost.

/// The git panel's release frame cost at 1000×700 with Merge's dialog open over it, planned by
/// the real planner — run alone: `cargo test --release --lib --no-default-features -- --ignored
/// --exact gui::browser::git_panel::tests_x4_dialogs2::frame_cost_at_1000_by_700 --nocapture`.
/// Printed, no budget set.
#[test]
#[ignore]
fn frame_cost_at_1000_by_700() {
    use std::time::Instant;
    let mut view = super::tests::fixture_view();
    view.set_repo(super::tests_s3::repo());
    let mut fx = super::tests::Fx::new(1000.0, 700.0, view);
    let commit = super::tests::commit(8, &[6], "Bob", super::tests::day(8), "feature: second");
    let target = Target::Commit {
        commit,
        marked: Vec::new(),
    };
    let asked = (fx.host.view).open_action(ActionId::Merge, target, Prefill::default());
    assert!(asked.is_empty(), "{asked:?}");
    fx.drv.idle(&mut fx.host, 20);
    assert!(fx.host.view.dialog_up(), "Merge's dialog is open");
    let n = 400;
    let started = Instant::now();
    for _ in 0..n {
        fx.drv.frame(&mut fx.host, Vec::new(), Modifiers::NONE);
    }
    println!(
        "the git panel's frame at 1000x700 with Merge's dialog: {:?}",
        started.elapsed() / n
    );
}

// ---------------------------------------------------------------------------------------------
// F-git-outcome: a result up stays until it is closed, and the results after it wait.

/// The panel over the S3 fixture, nothing up.
fn results_panel() -> super::tests::Fx {
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
    fx.idle();
    fx
}

/// The failed push's result (`outcome(true)`), as its run's end shows it.
fn push_failed() -> ResultDialog {
    ResultDialog::of("Push", &outcome(true)).expect("a step failed")
}

/// A read's failure, as the Backups list's read shows it.
fn backups_failed() -> ResultDialog {
    ResultDialog::error("Backups", "fatal: not a git repository", None)
}

/// The result the panel shows.
fn result_up(fx: &super::tests::Fx) -> Option<&ResultDialog> {
    fx.host.view.result.as_ref()
}

/// The topmost dialog's rows as one line of words — its frame, its shadow and the blanks between
/// rows gone — so a line the box wraps reads whole.
fn box_words(fx: &super::tests::Fx) -> String {
    let frame = |c: char| c.is_whitespace() || "║│░▒▓█╔╗╚╝═─".contains(c);
    let l = boxed(&fx.drv).expect("a dialog in cells");
    let rows: Vec<String> = rows(&l)
        .iter()
        .map(|r| r.trim_matches(frame).to_string())
        .collect();
    rows.join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The last pass's cells hold `text`, in any layer.
fn on_screen(fx: &super::tests::Fx, text: &str) -> bool {
    !cells::find(&cells::layers(&fx.drv.ctx), text).is_empty()
}

/// A write that succeeds while an earlier write's failure is up leaves the failure up, unread
/// until the owner closes it; the status row says the success's words, as the host sets them.
#[test]
fn x4d2_a_success_after_a_failure_leaves_the_failure_up() {
    let mut fx = results_panel();
    let said = fx.host.view.show_outcome("Push", "Pushed", &outcome(true));
    fx.host.view.status = Some(said);
    fx.idle();
    fx.idle();
    assert!(on_screen(&fx, "error: failed to push some refs"));
    let deleted = "Deleted branch b";
    let said = (fx.host.view).show_outcome("Delete branch b", deleted, &outcome(false));
    fx.host.view.status = Some(said);
    fx.idle();
    assert_eq!(
        result_up(&fx),
        Some(&push_failed()),
        "the failure was replaced"
    );
    assert!(on_screen(&fx, "error: failed to push some refs"));
    assert_eq!(
        fx.host.view.status_line().text,
        dialogs::status_text(deleted, &outcome(false)),
        "the latest run's words"
    );
    fx.press(Key::Enter);
    assert_eq!(result_up(&fx), None);
    assert!(!fx.host.view.dialog_up(), "the success has no dialog");
}

/// A second failure — both arriving before a pass, as one poll can hand them — waits behind the
/// first and shows once the first is closed, in arrival order; the covers and `dialog_up` follow
/// the one shown.
#[test]
fn x4d2_a_second_failure_waits_for_the_first() {
    let mut fx = results_panel();
    fx.host.view.show_outcome("Push", "Pushed", &outcome(true));
    (fx.host.view).show_text("Backups", "fatal: not a git repository", None);
    fx.idle();
    fx.idle();
    let grid = Grid::over(fx.host.rect);
    assert_eq!(
        result_up(&fx),
        Some(&push_failed()),
        "the first shows first"
    );
    assert!(on_screen(&fx, "error: failed to push some refs"));
    assert!(!on_screen(&fx, "fatal: not a git repository"));
    assert_eq!(fx.host.view.covered(&grid), push_failed().covers(&grid));
    fx.press(Key::Enter);
    assert_eq!(
        result_up(&fx),
        Some(&backups_failed()),
        "the second after it"
    );
    fx.idle();
    fx.idle();
    assert!(on_screen(&fx, "fatal: not a git repository"));
    assert!(!on_screen(&fx, "error: failed to push some refs"));
    assert_eq!(fx.host.view.covered(&grid), backups_failed().covers(&grid));
    assert!(fx.host.view.dialog_up());
    fx.press(Key::Enter);
    assert_eq!(result_up(&fx), None);
    assert!(!fx.host.view.dialog_up());
    assert!(fx.host.view.covered(&grid).is_empty());
}

/// `[Enter, Enter]` in one pass closes the result up and not the one behind it; nor do Enters in
/// the passes before the next one showed (laid out unseen, then the first pass it paints); the
/// Enter after them closes it.
#[test]
fn x4d2_two_enters_in_one_pass_close_one_result() {
    let mut fx = results_panel();
    fx.host.view.show_outcome("Push", "Pushed", &outcome(true));
    (fx.host.view).show_text("Backups", "fatal: not a git repository", None);
    fx.idle();
    fx.idle();
    let enters = vec![down(Key::Enter), down(Key::Enter)];
    fx.drv.frame(&mut fx.host, enters, Modifiers::NONE);
    assert_eq!(result_up(&fx), Some(&backups_failed()), "both closed");
    for _ in 0..2 {
        fx.drv
            .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
        assert_eq!(result_up(&fx), Some(&backups_failed()), "closed unseen");
    }
    fx.drv
        .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
    assert_eq!(result_up(&fx), None);
}

/// 03 §7.2: "delete anyway?" replaces the error it answers — the result its run's end just
/// added, up alone or waiting behind another — and never a result up before it.
#[test]
fn x4d2_delete_anyway_replaces_only_the_error_it_answers() {
    let forced = Action::DeleteBranch {
        branch: "feature".into(),
        force: true,
        remotes: Vec::new(),
    };
    let refused = || outcome(true);
    // Alone: the question in its place.
    let mut fx = results_panel();
    (fx.host.view).show_outcome("Delete branch feature", "Deleted", &refused());
    assert!(fx.host.view.confirm_again(forced.clone()).is_empty());
    assert_eq!(result_up(&fx), None, "the error it answers");
    assert!(fx.host.view.dialog.is_some(), "delete anyway?");
    // Behind others — one up, one waiting before it: those stay, and are all that is left once
    // the question goes.
    let mut fx = results_panel();
    fx.host.view.show_outcome("Push", "Pushed", &outcome(true));
    fx.idle();
    fx.idle();
    (fx.host.view).show_text("Backups", "fatal: not a git repository", None);
    (fx.host.view).show_outcome("Delete branch feature", "Deleted", &refused());
    assert!(fx.host.view.confirm_again(forced).is_empty());
    assert!(fx.host.view.dialog.is_some(), "delete anyway?");
    assert_eq!(result_up(&fx), Some(&push_failed()), "the result up before");
    fx.idle();
    fx.idle();
    fx.press(Key::Escape);
    assert!(fx.host.view.dialog.is_none(), "the question went");
    assert_eq!(result_up(&fx), Some(&push_failed()));
    fx.idle();
    fx.idle();
    fx.press(Key::Enter);
    assert_eq!(
        result_up(&fx),
        Some(&backups_failed()),
        "the one waiting before the answered error"
    );
    fx.idle();
    fx.idle();
    fx.press(Key::Enter);
    assert_eq!(result_up(&fx), None, "the answered error came back");
    assert!(!fx.host.view.dialog_up());
}

/// The git panel in the terminal, saying which result is up after each pass: 0 the first
/// (`results.0`), 1 the second, 2 none.
struct ResultsInTerminal {
    host: super::tests::Host,
    results: (ResultDialog, ResultDialog),
    up: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Window for ResultsInTerminal {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.host.ui(ui);
        let up = match &self.host.view.result {
            Some(r) if *r == self.results.0 => 0,
            Some(r) if *r == self.results.1 => 1,
            None => 2,
            Some(_) => 3,
        };
        self.up.set(up);
    }

    fn ready(&self) -> bool {
        true
    }
}

/// Through the terminal, which prints no pass while input waits: the push's failure printed, the
/// Backups read's waiting, `[Enter, a focus report, a focus report, Enter]` back to back close
/// the first and not the second — laid out unseen and then painted in passes never printed, it
/// was not on screen at the second Enter —; once the second is printed, an Enter closes it.
#[test]
fn x4d2_a_result_the_terminal_never_printed_stays() {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
    let enter = CEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let up = std::rc::Rc::new(std::cell::Cell::new(9));
    let sent_at = std::rc::Rc::new(RefCell::new(Vec::new()));
    let tty = Bursts {
        bursts: [
            vec![CEvent::FocusGained],
            vec![CEvent::FocusGained],
            vec![
                enter.clone(),
                CEvent::FocusGained,
                CEvent::FocusGained,
                enter.clone(),
            ],
            vec![CEvent::FocusGained],
            vec![enter],
        ]
        .into(),
        now: Default::default(),
        asked: 0,
        gap: 3,
        stops: up.clone(),
        sent_at: sent_at.clone(),
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut b = term::Backend::new(tty, std::io::sink(), ctx, true);
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    view.show_outcome("Push", "Pushed", &outcome(true));
    view.show_text("Backups", "fatal: not a git repository", None);
    let mut win = ResultsInTerminal {
        host: super::tests::Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: egui::Rect::NOTHING,
            under: Vec::new(),
            side: None,
        },
        results: (push_failed(), backups_failed()),
        up: up.clone(),
    };
    let mut no_switch =
        |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, term::End::Signal(libc::SIGTERM));
    assert_eq!(
        *sent_at.borrow(),
        [0, 0, 0, 1, 1],
        "which result was up as each burst came"
    );
    assert_eq!(up.get(), 2, "the second closed once printed");
}

/// The first result follows the same rule: an Enter in its first passes — laid out unseen, then
/// the first it paints — closes nothing (it came as the owner pressed a key meant for the graph);
/// the next one does.
#[test]
fn x4d2_a_result_closes_only_once_shown() {
    let mut fx = results_panel();
    (fx.host.view).show_text("Backups", "fatal: not a git repository", None);
    for _ in 0..2 {
        fx.drv
            .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
        assert_eq!(result_up(&fx), Some(&backups_failed()), "closed unseen");
    }
    fx.drv
        .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
    assert_eq!(result_up(&fx), None);
}

/// At most `RESULTS_WAITING` wait behind the one up: past them the oldest waiting goes, so the
/// one up stays and the newest — the one "delete anyway?" may answer — is always kept; the next
/// result shown says, in a line of its own, how many were not and where their commands are (one
/// past the cap drops one, three past it three).
#[test]
fn x4d2_the_results_waiting_are_bounded() {
    let title = |k: usize| format!("Read {k:02}");
    for over in [1, 3] {
        let mut fx = results_panel();
        let n = super::RESULTS_WAITING + 1 + over;
        for k in 0..n {
            (fx.host.view).show_text(&title(k), "fatal: x", None);
        }
        // The whole line, its count and where the commands that ran are, none of it cut (the box
        // may wrap it).
        let log = crate::gui::browser::keys::Mods::Command.prefix();
        let tail = format!("not shown — the command log ({log}L) has the commands that ran");
        let notice = match over {
            1 => format!("1 earlier result {tail}"),
            n => format!("{n} earlier results {tail}"),
        };
        let mut shown = Vec::new();
        for _ in 0..=n {
            if result_up(&fx).is_none() {
                break;
            }
            fx.idle();
            fx.idle();
            let k = (0..n).find(|&k| on_screen(&fx, &title(k)));
            shown.push((
                k.expect("a result on screen"),
                box_words(&fx).contains(&notice),
            ));
            fx.press(Key::Enter);
        }
        let kept: Vec<(usize, bool)> = (0..n)
            .filter(|&k| k == 0 || k > over)
            .map(|k| (k, k == over + 1))
            .collect();
        assert_eq!(shown, kept, "{over} past the cap");
    }
}

/// A terminal that sends no key releases, at a press's pace: Enter on the first result, then
/// Enter again once the terminal is idle, closes both — the next result, of the first one's size,
/// is painted where the screen shows it before the second press.
#[test]
fn x4d2_each_press_in_a_terminal_closes_one_result() {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
    let enter = CEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let up = std::rc::Rc::new(std::cell::Cell::new(9));
    let sent_at = std::rc::Rc::new(RefCell::new(Vec::new()));
    let tty = Bursts {
        bursts: [
            vec![CEvent::FocusGained],
            vec![CEvent::FocusGained],
            vec![enter.clone()],
            vec![enter],
        ]
        .into(),
        now: Default::default(),
        asked: 0,
        gap: 30,
        stops: up.clone(),
        sent_at: sent_at.clone(),
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut b = term::Backend::new(tty, std::io::sink(), ctx, true);
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    // Two results of one size: egui settles no box that changed size between them.
    let restore = || ResultDialog::error("Restore", "fatal: not a backup group 7", None);
    view.show_text("Backups", "fatal: not a git repository", None);
    view.show_text("Restore", "fatal: not a backup group 7", None);
    let mut win = ResultsInTerminal {
        host: super::tests::Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: egui::Rect::NOTHING,
            under: Vec::new(),
            side: None,
        },
        results: (backups_failed(), restore()),
        up: up.clone(),
    };
    let mut no_switch =
        |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, term::End::Signal(libc::SIGTERM));
    assert_eq!(
        *sent_at.borrow(),
        [0, 0, 0, 1],
        "which result was up as each press came"
    );
    assert_eq!(up.get(), 2, "the second press closed the second result");
}
