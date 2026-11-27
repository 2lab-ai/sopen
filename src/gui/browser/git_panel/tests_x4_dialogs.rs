//! X4-dialogs' tests (10 §17): an action's dialog in cells through the kit (03 §7, §8.1, §8.3) —
//! the line of what is lost, the options, the plan that scrolls, the buttons — for 03 §7.6–§7.7's
//! forms, a restore's entries and every dialog without options.
//!
//! Each dialog stands in a host that lays a screen of cells under it and blanks what the dialog
//! says it covers first ([`ActionDialog::covers`]), so a terminal (`term::compose` of the pass's
//! layers) shows the window's cells exactly. No git runs: every dialog is given a planner of the
//! test's own. Keys and clicks go through the test kit, several in one pass where it matters.

use super::dialogs::{self, ActionDialog, DialogEnd, Opened, Planner, Prefill};
use super::menu::Target;
use super::tests_s3::repo;
use crate::git::actions::{
    Action, ActionId, ConfigScope, Confirmation, Overwrite, Plan, Proceed, PushOptions, Refused,
    Remote, RemoteChange, Repo, Step,
};
use crate::git::backup::{RestoreEntry, RestoreHow, Subject, Value};
use crate::git::model::{
    BackupKind, BackupRef, Change, ObjectType, Oid, RefKind, RefLabel, StashRow, StatusEntry,
    StatusState,
};
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

fn changed(path: &str) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        old_path: None,
        state: StatusState::Changed {
            staged: Some(Change::Modified),
            unstaged: None,
        },
        lossy: false,
    }
}

fn conflict(path: &str) -> StatusEntry {
    StatusEntry {
        path: path.into(),
        old_path: None,
        state: StatusState::Conflict("UU".into()),
        lossy: false,
    }
}

/// The fixture repository with a staged Hangul file, a staged and an unstaged one, and a second,
/// Hangul-named remote.
fn dirty() -> Repo {
    let mut r = repo();
    let mut unstaged = changed("u.txt");
    unstaged.state = StatusState::Changed {
        staged: None,
        unstaged: Some(Change::Modified),
    };
    let mut new = changed("n.txt");
    new.state = StatusState::Untracked;
    r.status.entries = vec![changed("문서.txt"), changed("b.txt"), unstaged, new];
    r.remotes.push("백업".into());
    r
}

fn label(refname: &str, kind: RefKind) -> RefLabel {
    RefLabel {
        object: oid('b'),
        object_type: ObjectType::Commit,
        peeled: None,
        refname: refname.into(),
        kind,
        upstream: None,
        track: String::new(),
        lossy: false,
    }
}

fn remote_label(name: &str) -> RefLabel {
    label(&format!("refs/remotes/{name}"), RefKind::RemoteBranch)
}

fn local(name: &str) -> RefLabel {
    label(&format!("refs/heads/{name}"), RefKind::LocalBranch)
}

fn stash_row() -> StashRow {
    StashRow {
        oid: oid('d'),
        base: oid('a'),
        index: Some(oid('8')),
        untracked: None,
        selector: "stash@{0}".into(),
        commit_time: 1_794_389_040,
        subject: "WIP on main: first".into(),
    }
}

thread_local! {
    /// Every action a test's planner was asked for, in order (each test runs on its own thread).
    static ASKED: RefCell<Vec<Action>> = const { RefCell::new(Vec::new()) };
}

fn asked() -> Vec<Action> {
    ASKED.with(|a| a.borrow().clone())
}

fn plan(confirm: Option<Confirmation>, steps: Vec<Step>) -> Plan {
    Plan {
        confirm,
        backups: Vec::new(),
        message: String::new(),
        steps,
        changes_files: false,
    }
}

/// A planner whose every plan has a confirmation naming the action in each part.
fn fake(action: &Action, _: &Repo) -> Result<Plan, Refused> {
    ASKED.with(|a| a.borrow_mut().push(action.clone()));
    let id = action.id().as_str();
    let confirm = Confirmation {
        title: format!("Fake {id}"),
        loses: Some(format!("{id} loses this")),
        changes: vec![format!("{id}: 11111111 → 22222222")],
        backups: vec![format!("heads/{id}")],
        commands: vec![format!("git {id}")],
        notes: vec![format!("{id} note")],
    };
    Ok(plan(Some(confirm), Vec::new()))
}

/// A planner that refuses everything (03 §8.6).
fn refuse(action: &Action, _: &Repo) -> Result<Plan, Refused> {
    ASKED.with(|a| a.borrow_mut().push(action.clone()));
    Err(Refused("an ignored file is in the way: build/out.o".into()))
}

/// [`fake`]'s plan, deleting a branch on a remote (03 §8.4).
fn overwrite(action: &Action, repo: &Repo) -> Result<Plan, Refused> {
    let mut p = fake(action, repo)?;
    p.steps = vec![Step::Remote(RemoteChange {
        remote: "origin".into(),
        refname: "refs/heads/feature".into(),
        change: Overwrite::Delete,
        options: PushOptions {
            set_upstream: false,
            no_verify: false,
        },
    })];
    Ok(p)
}

/// A planner whose confirmation runs 40 commands, the first far longer than a row.
fn long(action: &Action, _: &Repo) -> Result<Plan, Refused> {
    let mut commands: Vec<String> = (1..=40).map(|k| format!("git step {k:02}")).collect();
    commands[0] = format!("git first {}", "word ".repeat(30).trim_end());
    let confirm = Confirmation {
        title: format!("Long {}", action.id().as_str()),
        loses: None,
        changes: Vec::new(),
        backups: Vec::new(),
        commands,
        notes: Vec::new(),
    };
    Ok(plan(Some(confirm), Vec::new()))
}

fn dialog(opened: Opened) -> ActionDialog {
    match opened {
        Opened::Dialog(d) => *d,
        other => panic!("not a dialog: {other:?}"),
    }
}

fn open(id: ActionId, target: Target, repo: &Repo, planner: Planner) -> ActionDialog {
    dialog(ActionDialog::open(
        id,
        &target,
        repo,
        &Prefill::default(),
        planner,
    ))
}

// ---------------------------------------------------------------------------------------------
// The host: a screen of cells, the dialog over it.

struct Host {
    dialog: Option<ActionDialog>,
    planner: Planner,
    ends: Vec<DialogEnd>,
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
        let words = "한글 under ".repeat(30);
        for y in 0..grid.rows {
            cells::put(&mut under, 0, y, &words, usize::from(grid.cols), body);
        }
        self.covers = (self.dialog.as_ref()).map_or(Vec::new(), |d| d.covers(&grid));
        for c in &self.covers {
            cells::fill(&mut under, *c, body);
        }
        cells::paint(ui.painter(), grid.origin, &under, None);
        if let Some(d) = &mut self.dialog {
            if let Some(end) = d.ui(&ctx, grid, self.planner) {
                self.dialog = None;
                self.ends.push(end);
            }
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
    /// `d` on a `cols`×`rows` grid, laid out twice (a new modal is laid out unseen first).
    fn new(d: ActionDialog, planner: Planner, cols: u16, rows: u16) -> Fx {
        let mut fx = Fx {
            drv: Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y),
            host: Host {
                dialog: Some(d),
                planner,
                ends: Vec::new(),
                covers: Vec::new(),
            },
        };
        fx.idle(2);
        fx
    }

    fn idle(&mut self, n: usize) {
        self.drv.idle(&mut self.host, n);
    }

    /// One pass with `events`, then one without.
    fn frame(&mut self, events: Vec<Event>) {
        self.drv.frame(&mut self.host, events, Modifiers::NONE);
        self.idle(1);
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

    fn click(&mut self, text: &str) {
        self.drv.click_text_on_top(&mut self.host, text);
        self.idle(1);
    }

    /// A click on the first cell of `text` in the dialog titled `title`: a choice right after its
    /// row's label is one run with it, so the exact text queries cannot find it alone.
    fn click_in(&mut self, title: &str, text: &str) {
        let at = place(&self.boxed(title), text);
        self.drv.click(&mut self.host, at_cell(at.x, at.y));
        self.idle(1);
    }

    fn draft(&self) -> Action {
        self.host
            .dialog
            .as_ref()
            .expect("still open")
            .draft()
            .clone()
    }

    /// The dialog's layer: the one whose rows hold `title`.
    fn boxed(&self, title: &str) -> Arc<Layer> {
        boxed(&self.drv, title).unwrap_or_else(|| panic!("no layer titled {title:?}"))
    }
}

fn boxed(drv: &Driver, title: &str) -> Option<Arc<Layer>> {
    cells::layers(&drv.ctx).into_iter().find(|l| {
        let a = l.buf.area;
        (a.top()..a.bottom()).any(|y| cells::row_text(&l.buf, y).0.contains(title))
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

fn pen_at(l: &Layer, x: u16, y: u16) -> (Option<Color32>, Option<Color32>) {
    let a = l.buf.area;
    let seen = &l.seen()[usize::from(y - a.y)][usize::from(x - a.x)];
    (seen.fg, seen.bg)
}

fn pen_of(l: &Arc<Layer>, text: &str) -> (Option<Color32>, Option<Color32>) {
    let at = place(l, text);
    pen_at(l, at.x, at.y)
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
/// last layer painted there (one whose cell has no colour of its own lets the one under show),
/// the same character, foreground and background, and the same caret; no hand-off box.
fn same_in_a_terminal(drv: &Driver) -> term::Screen {
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
    screen
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

fn down(k: Key) -> Event {
    key(k, true, Modifiers::NONE)
}

/// Space as egui-winit sends it: the key and its text in one pass.
fn space() -> Vec<Event> {
    vec![down(Key::Space), Event::Text(" ".into())]
}

// ---------------------------------------------------------------------------------------------
// The Commit dialog (03 §7.6).

/// F7's Commit dialog in cells: its lost line, the message field (typed in Hangul) and the two
/// options as `[ ]` choices, the plan — what changes, the backups, the command lines (a heading's
/// lines two cells in), the notes — then `[ Run ]`, `[ Cancel ]` and the hint; the same cells in a
/// terminal, the caret in the message.
#[test]
fn x4d_the_commit_dialog_is_cells_in_both_backends() {
    let d = open(A::Commit, Target::Uncommitted, &dirty(), fake);
    let mut fx = Fx::new(d, fake, 100, 30);
    fx.frame(vec![Event::Text("한글 메시지".into())]);
    let l = fx.boxed("Fake commit");
    let all = rows(&l).join("\n");
    for text in [
        "commit loses this",
        "Message",
        "한글 메시지",
        "[ ] Amend the last commit instead  [ ] Skip hooks",
        "Changes",
        "  commit: 11111111 → 22222222",
        "Backed up first, under refs/sopen-backup/<stamp>/",
        "  heads/commit",
        "Runs",
        "  git commit",
        "commit note",
        "[ Run ]  [ Cancel ]",
        "Enter runs · Esc cancels",
    ] {
        assert!(all.contains(text), "{text:?} not in the dialog:\n{all}");
    }
    assert_eq!(
        pen_of(&l, "한글 메시지"),
        field_pen(),
        "the message's cells"
    );
    assert_eq!(pen_of(&l, "Changes"), weak_pen(), "a heading");
    assert_eq!(pen_of(&l, "Enter runs"), weak_pen(), "the hint");
    assert_eq!(
        fx.draft(),
        Action::Commit {
            message: "한글 메시지".into(),
            amend: false,
            no_verify: false
        }
    );
    let screen = same_in_a_terminal(&fx.drv);
    let caret = screen.caret.expect("the message's caret");
    let typed = place(&l, "한글 메시지");
    assert_eq!(
        (caret.x, caret.y),
        (typed.right(), typed.y),
        "after the text"
    );
}

/// The parts stand in 03 §8.1's order: what is lost first, then the plan — what changes, the
/// backups, the commands, the notes — and §8.4's warning in the error colour, then the buttons
/// and the hint; a blank row between the parts (whole cells, 10 §6).
#[test]
fn x4d_the_parts_stand_in_order_and_the_warning_is_red() {
    let target = Target::Label(remote_label("origin/feature"));
    let d = open(A::DeleteRemoteBranch, target, &repo(), overwrite);
    let fx = Fx::new(d, overwrite, 100, 30);
    let l = fx.boxed("Fake delete-remote-branch");
    let warning = "deletes refs/heads/feature on origin, with a lease on the value read at Run";
    let order: Vec<u16> = [
        "delete-remote-branch loses this",
        "Changes",
        "Backed up first",
        "Runs",
        "delete-remote-branch note",
        warning,
        "[ Run ]",
        "Enter runs",
    ]
    .iter()
    .map(|t| place(&l, t).y)
    .collect();
    let mut sorted = order.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(order, sorted, "{:#?}", rows(&l));
    assert_eq!(pen_of(&l, warning), error_pen());
    assert_eq!(pen_of(&l, "[ Run ]"), text_pen(), "Run is on");
    let lost = place(&l, "delete-remote-branch loses this").y;
    assert_eq!(place(&l, "Changes").y, lost + 2, "a blank row after it");
    same_in_a_terminal(&fx.drv);
}

/// `[ Run ]` is off — the weak colour, no click — while the plan refuses, the refusal in the error
/// colour; Enter then runs nothing, Esc cancels.
#[test]
fn x4d_run_is_off_while_the_plan_refuses() {
    let files = Target::Files(vec![changed("b.txt")]);
    let d = open(A::Discard, files, &dirty(), refuse);
    let mut fx = Fx::new(d, refuse, 100, 30);
    let l = fx.boxed("Discard changes");
    let why = "an ignored file is in the way: build/out.o";
    assert_eq!(pen_of(&l, why), error_pen());
    assert_eq!(pen_of(&l, "[ Run ]"), weak_pen(), "off");
    fx.press(Key::Enter);
    fx.click("[ Run ]");
    assert!(fx.host.ends.is_empty(), "nothing runs: {:?}", fx.host.ends);
    fx.press(Key::Escape);
    assert_eq!(fx.host.ends, [DialogEnd::Cancel]);
}

/// What the dialog covers is its box and its shadow, and the screen under it is blank there; the
/// forms egui still draws (03 §7.1–§7.5's, until X4-dialogs2) cover nothing and are egui's box.
#[test]
fn x4d_the_dialog_covers_its_box_and_shadow() {
    let d = open(A::Stash, Target::Uncommitted, &dirty(), fake);
    let fx = Fx::new(d, fake, 100, 30);
    let l = fx.boxed("Fake stash");
    assert_eq!(
        fx.host.covers.len(),
        3,
        "the box and its shadow's two strips"
    );
    let frame = fx.host.covers[0];
    assert_eq!(
        cells::row_text(&l.buf, frame.y).0.chars().next(),
        Some('╔'),
        "the box's corner"
    );
    let under = cells::layers(&fx.drv.ctx)[0].clone();
    for strip in &fx.host.covers[1..] {
        let seen = &under.seen()[usize::from(strip.y)][usize::from(strip.x)];
        assert_eq!(seen.symbol, " ", "the screen blank under the shadow");
    }
    // An egui form: no cells, nothing covered.
    let bar = Target::Commit {
        commit: super::tests::commit(2, &[1], "Ann", 0, "second"),
        marked: Vec::new(),
    };
    let d = open(A::Reset, bar, &repo(), fake);
    let fx = Fx::new(d, fake, 100, 30);
    assert!(fx.host.covers.is_empty());
    assert!(boxed(&fx.drv, "Fake reset").is_none(), "egui's dialog");
    assert!(fx.drv.find_text("Fake reset").is_some());
}

// ---------------------------------------------------------------------------------------------
// The keyboard.

/// Enter runs (03 §8.1) — but each Enter goes to what has the keyboard at that point of the pass:
/// on a focused button it is that button's press. `[Tab, Enter]` from Run presses Cancel (the Tab
/// came first), `[Enter, Tab]` runs.
#[test]
fn x4d_enter_presses_the_focused_button_in_the_pass_s_order() {
    let stash = Target::Stash(stash_row());
    let d = || open(A::StashDrop, stash.clone(), &repo(), fake);
    let mut fx = Fx::new(d(), fake, 100, 30);
    assert_eq!(fx.focused(), None, "no field: nothing has the keyboard");
    fx.focus(dialogs::cancel_id());
    fx.press(Key::Enter);
    assert_eq!(fx.host.ends, [DialogEnd::Cancel], "Enter on Cancel");

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.focus(dialogs::run_id());
    fx.frame(vec![down(Key::Tab), down(Key::Enter)]);
    assert_eq!(fx.host.ends, [DialogEnd::Cancel], "the Tab, then Enter");

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.focus(dialogs::run_id());
    fx.frame(vec![down(Key::Enter), down(Key::Tab)]);
    let ran = DialogEnd::Run {
        action: Action::StashDrop { stash: stash_row() },
        dont_ask_again: false,
    };
    assert_eq!(
        fx.host.ends,
        std::slice::from_ref(&ran),
        "Enter, then the Tab"
    );

    // `[Tab, Enter]` from the last button: the Tab leaves nothing with the keyboard in that pass
    // (egui hands it round on the next), so Enter is the dialog's and runs — as Run, where the
    // keyboard comes round to, would.
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.focus(dialogs::cancel_id());
    fx.frame(vec![down(Key::Tab), down(Key::Enter)]);
    assert_eq!(fx.host.ends, std::slice::from_ref(&ran), "round to Run");

    // Nothing focused: Enter runs.
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.press(Key::Enter);
    assert_eq!(fx.host.ends, [ran]);
}

/// F-kit-tab in an action's dialog: `[Tab, Space]` from a field types the blank there and presses
/// nothing the Tab reaches (the field read the whole pass); `[Space, Tab]` on a focused `[ Run ]`
/// presses Run once and never Cancel, which the Tab gave the keyboard to.
#[test]
fn x4d_space_in_one_pass_with_a_tab_presses_only_what_had_the_keyboard() {
    let edit = || {
        let prefill = Prefill {
            remotes: vec![Remote {
                name: "origin".into(),
                url: "https://example.com/x.git".into(),
                push_url: None,
            }],
            ..Prefill::default()
        };
        let opened =
            ActionDialog::open(A::EditRemote, &Target::Repository, &dirty(), &prefill, fake);
        Fx::new(dialog(opened), fake, 100, 30)
    };
    let mut fx = edit();
    fx.focus(dialogs::field_id(2));
    let mut events = vec![down(Key::Tab)];
    events.extend(space());
    fx.frame(events);
    assert!(
        fx.host.ends.is_empty(),
        "Run not pressed: {:?}",
        fx.host.ends
    );
    assert_eq!(
        fx.focused(),
        Some(dialogs::run_id()),
        "the Tab moved on to Run"
    );

    let mut fx = edit();
    fx.focus(dialogs::run_id());
    let mut events = space();
    events.push(down(Key::Tab));
    fx.frame(events);
    assert!(
        matches!(&fx.host.ends[..], [DialogEnd::Run { .. }]),
        "Run, once, not Cancel: {:?}",
        fx.host.ends
    );
}

/// Typing, Shift+Enter and Enter in one pass: the message holds the new line and Enter runs the
/// action with exactly what was typed before it.
#[test]
fn x4d_typing_and_enter_in_one_pass_run_what_was_typed() {
    let d = open(A::Commit, Target::Uncommitted, &dirty(), fake);
    let mut fx = Fx::new(d, fake, 100, 30);
    fx.frame(vec![
        Event::Text("가".into()),
        key(Key::Enter, true, Modifiers::SHIFT),
        Event::Text("나".into()),
        down(Key::Enter),
    ]);
    assert_eq!(
        fx.host.ends,
        [DialogEnd::Run {
            action: Action::Commit {
                message: "가\n나".into(),
                amend: false,
                no_verify: false
            },
            dont_ask_again: false
        }]
    );
}

/// Space chooses one of several and turns any of several, once a press, and each change plans
/// again: Name and email's "globally", Commit's "Skip hooks" (three presses in one pass).
#[test]
fn x4d_choices_by_the_keyboard_plan_again() {
    let d = open(A::User, Target::Repository, &dirty(), fake);
    let mut fx = Fx::new(d, fake, 100, 30);
    assert_eq!(
        fx.focused(),
        Some(dialogs::field_id(0)),
        "Name has the typing"
    );
    fx.focus(choice_id(dialogs::row_id(0), 1));
    fx.frame(space());
    let Action::User { scope, .. } = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(scope, ConfigScope::Global);
    assert!(matches!(
        asked().last(),
        Some(Action::User {
            scope: ConfigScope::Global,
            ..
        })
    ));
    let l = fx.boxed("Fake user");
    let all = rows(&l).join("\n");
    assert!(
        all.contains("Where ( ) for this repository  (•) globally"),
        "{all}"
    );

    let d = open(A::Commit, Target::Uncommitted, &dirty(), fake);
    let mut fx = Fx::new(d, fake, 100, 30);
    fx.focus(choice_id(dialogs::row_id(0), 1));
    let mut thrice = space();
    thrice.extend(space());
    thrice.extend(space());
    fx.frame(thrice);
    assert!(
        matches!(
            fx.draft(),
            Action::Commit {
                no_verify: true,
                amend: false,
                ..
            }
        ),
        "{:?}",
        fx.draft()
    );
}

/// Amend's "edit it" brings its message field, which takes the keyboard as it shows; "keep its
/// message" keeps what was typed for when it is chosen again.
#[test]
fn x4d_amend_s_message_takes_the_keyboard_when_it_shows() {
    let prefill = Prefill {
        message: Some("old".into()),
        ..Prefill::default()
    };
    let opened = ActionDialog::open(A::Amend, &Target::Uncommitted, &dirty(), &prefill, fake);
    let mut fx = Fx::new(dialog(opened), fake, 100, 30);
    assert_eq!(fx.draft(), Action::Amend { message: None });
    fx.click("( ) edit it");
    fx.idle(1);
    assert_eq!(
        fx.draft(),
        Action::Amend {
            message: Some("old".into())
        }
    );
    assert_eq!(
        fx.focused(),
        Some(dialogs::field_id(0)),
        "the message has it"
    );
    fx.frame(vec![Event::Text("new ".into())]);
    assert_eq!(
        fx.draft(),
        Action::Amend {
            message: Some("new old".into())
        },
        "typed at its start, as a message opens"
    );
    fx.click_in("Fake amend", "( ) keep its message");
    assert_eq!(fx.draft(), Action::Amend { message: None });
    fx.click("( ) edit it");
    assert_eq!(
        fx.draft(),
        Action::Amend {
            message: Some("new old".into())
        }
    );
}

// ---------------------------------------------------------------------------------------------
// The restore (03 §8.3).

fn entries(stamp: &str) -> Vec<RestoreEntry> {
    let backup_ref = |kind: BackupKind, name: &str, at: char| BackupRef {
        kind,
        refname: format!("refs/sopen-backup/{stamp}/{name}"),
        oid: oid(at),
        subject: "first".into(),
    };
    vec![
        RestoreEntry {
            subject: Subject::Ref("refs/heads/기능".into()),
            how: RestoreHow::CheckedOut,
            backup: Some(backup_ref(
                BackupKind::Head("기능".into()),
                "heads/기능",
                'a',
            )),
            before: Value::Object(oid('a')),
            now: Value::Object(oid('b')),
            moved_since: false,
            chosen: false,
            replace: false,
        },
        RestoreEntry {
            subject: Subject::Ref("refs/heads/feature".into()),
            how: RestoreHow::Ref,
            backup: Some(backup_ref(
                BackupKind::Head("feature".into()),
                "heads/feature",
                'b',
            )),
            before: Value::Object(oid('b')),
            now: Value::Object(oid('c')),
            moved_since: true,
            chosen: false,
            replace: false,
        },
        RestoreEntry {
            subject: Subject::Path("build/out.txt".into()),
            how: RestoreHow::Files,
            backup: Some(backup_ref(BackupKind::Files, "files", 'e')),
            before: Value::Object(oid('f')),
            now: Value::Occupied,
            moved_since: false,
            chosen: false,
            replace: false,
        },
    ]
}

/// A restore's dialog in cells: each entry a row of its own, chosen unless it moved since, its
/// `moved since (now …)` under it; the occupied path's `[ ] replace` beside it and its rule under
/// it. A click on `[ ] replace` and Space on an entry plan again; the same cells in a terminal.
#[test]
fn x4d_a_restore_lists_its_entries_in_cells() {
    let stamp = "20261128T101500Z";
    let d = dialog(ActionDialog::restore(
        stamp.into(),
        entries(stamp),
        None,
        &repo(),
        fake,
    ));
    let mut fx = Fx::new(d, fake, 100, 30);
    let l = fx.boxed("Fake restore");
    let all = rows(&l).join("\n");
    for text in [
        "[X] refs/heads/기능 → aaaaaaaa",
        "[ ] refs/heads/feature → bbbbbbbb",
        "    moved since (now cccccccc)",
        "[X] build/out.txt → ffffffff  [ ] replace",
        "    occupied: left as it is unless replace is chosen",
    ] {
        assert!(all.contains(text), "{text:?} not in the dialog:\n{all}");
    }
    let moved = place(&l, "moved since").y;
    assert_eq!(moved, place(&l, "refs/heads/feature").y + 1, "under it");
    same_in_a_terminal(&fx.drv);
    fx.click("[ ] replace");
    let Some(Action::Restore { entries, .. }) = asked().last().cloned() else {
        panic!("{:?}", asked());
    };
    assert!(entries[2].replace, "replace chosen for the occupied path");
    // Space on the entry that moved: its second, explicit choice.
    fx.focus(choice_id(dialogs::row_id(1), 0));
    fx.frame(space());
    let Action::Restore { entries, .. } = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(
        entries.iter().map(|e| e.chosen).collect::<Vec<_>>(),
        [true, true, true]
    );
    fx.press(Key::Enter);
    assert!(matches!(
        &fx.host.ends[..],
        [DialogEnd::Run {
            action: Action::Restore { .. },
            ..
        }]
    ));
}

/// In an ambiguous-wide terminal a restore entry's `→`, sopen's own between a name and a value,
/// is §10's `>`, written at the source (T38); the plan's lines, the planner's text with names in
/// it, and the hint's `·` stay as written. The box is the same cells either way.
#[test]
fn x4d_an_ambiguous_wide_terminal_reads_a_restore_entry_s_arrow_as_a_stand_in() {
    let stamp = "20261128T101500Z";
    let d = || {
        dialog(ActionDialog::restore(
            stamp.into(),
            entries(stamp),
            None,
            &repo(),
            fake,
        ))
    };
    let fx = Fx::new(d(), fake, 100, 30);
    let narrow = fx.host.covers.clone();
    let mut drv = Driver::new(800.0, 480.0);
    cells::set_ambiguous_wide(&drv.ctx, true);
    let mut host = Host {
        dialog: Some(d()),
        planner: fake,
        ends: Vec::new(),
        covers: Vec::new(),
    };
    drv.idle(&mut host, 2);
    let l = boxed(&drv, "Fake restore").expect("in cells");
    let all = rows(&l).join("\n");
    assert!(all.contains("[X] refs/heads/기능 > aaaaaaaa"), "{all}");
    assert!(
        all.contains("[X] build/out.txt > ffffffff  [ ] replace"),
        "{all}"
    );
    assert!(all.contains("  restore: 11111111 → 22222222"), "{all}");
    assert!(all.contains("Enter runs · Esc cancels"), "{all}");
    // What it said it covers is where it stood: the box and its shadow, as in a narrow terminal.
    let stood = narrow[0].union(narrow[1]).union(narrow[2]);
    assert_eq!(l.buf.area, stood, "the same box");
    assert_eq!(host.covers, narrow);
    same_in_a_terminal(&drv);
}

/// The pointer: a click on `[ Run ]` runs, on `[ Cancel ]` cancels, outside the box cancels.
#[test]
fn x4d_clicks_on_the_buttons_and_outside_the_box() {
    let stash = Target::Stash(stash_row());
    let d = || open(A::StashDrop, stash.clone(), &repo(), fake);
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.click("[ Run ]");
    assert!(
        matches!(&fx.host.ends[..], [DialogEnd::Run { .. }]),
        "{:?}",
        fx.host.ends
    );
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.click("[ Cancel ]");
    assert_eq!(fx.host.ends, [DialogEnd::Cancel]);
    let mut fx = Fx::new(d(), fake, 100, 30);
    let frame = fx.host.covers[0];
    assert!(frame.x > 2, "room left of the box: {frame:?}");
    fx.drv.click(&mut fx.host, at_cell(1, frame.y + 2));
    fx.idle(1);
    assert_eq!(fx.host.ends, [DialogEnd::Cancel], "a click outside it");
}

// ---------------------------------------------------------------------------------------------
// The plan that scrolls.

/// The plan's rows stand up to three fifths of the grid and then scroll — the wheel over the box
/// and the box's scroll bar on its right border — and give way first on a short grid; a line
/// longer than a row wraps two cells in.
#[test]
fn x4d_the_plan_scrolls_past_three_fifths_of_the_grid() {
    let stash = Target::Stash(stash_row());
    let d = open(A::StashDrop, stash.clone(), &repo(), long);
    let mut fx = Fx::new(d, long, 100, 40);
    let l = fx.boxed("Long stash-drop");
    let all = rows(&l);
    let (runs, top) = (place(&l, "Runs").y, l.buf.area.y);
    let row = |y: u16| all[usize::from(y - top)].clone();
    assert!(row(runs + 1).contains("git first word"), "{all:#?}");
    // The long line goes on two cells in (the box 76 cells inside: 74 a row).
    assert!(
        row(runs + 2).starts_with("║   word"),
        "the long line's second row: {:?}",
        row(runs + 2)
    );
    // 24 rows (40 × 3 / 5): the heading, the long line's three, 20 steps (02 to 21).
    assert!(row(runs + 23).contains("git step 21"), "{all:#?}");
    assert!(!row(runs + 24).contains("git step"), "{all:#?}");
    let frame = fx.host.covers[0];
    let border = frame.right() - 1;
    let seen = l.seen();
    let at = |y: u16| {
        let row = &seen[usize::from(y - top)];
        row[usize::from(border - l.buf.area.x)].symbol.clone()
    };
    assert_eq!(at(runs), "↑", "the scroll bar on the border");
    assert_eq!(at(runs + 23), "↓");
    // The wheel over the box scrolls the rows.
    let middle = at_cell(frame.x + 10, runs + 5);
    fx.drv.wheel(&mut fx.host, middle, -1.0);
    let l = fx.boxed("Long stash-drop");
    assert!(
        !rows(&l).join("\n").contains("git first"),
        "scrolled: {:#?}",
        rows(&l)
    );
    same_in_a_terminal(&fx.drv);

    // A short grid: the plan gives way; the buttons and the hint stay inside it.
    let d = open(A::StashDrop, stash, &repo(), long);
    let fx = Fx::new(d, long, 100, 16);
    let l = fx.boxed("Long stash-drop");
    assert!(fx.host.covers[0].bottom() < 16, "{:?}", fx.host.covers);
    let all = rows(&l).join("\n");
    assert!(
        all.contains("[ Run ]") && all.contains("Enter runs"),
        "{all}"
    );
    same_in_a_terminal(&fx.drv);
}

// ---------------------------------------------------------------------------------------------
// Every form and every dialog without options.

/// 03 §7.6–§7.7's forms and every dialog without options are cells: each its box titled with its
/// plan, its options, the same cells in a terminal.
#[test]
fn x4d_every_form_here_and_every_dialog_without_options_is_cells() {
    let d = dirty();
    let mut conflicted = dirty();
    conflicted.status.entries.push(conflict("c.txt"));
    let remote = Target::Label(remote_label("origin/feature"));
    let remotes = "Remote (•) origin  ( ) 백업";
    let confirm = |action: Action| dialog(ActionDialog::confirm(action, &d, fake));
    let cases: Vec<(ActionDialog, &str)> = vec![
        (
            open(A::Commit, Target::Uncommitted, &d, fake),
            "[ ] Skip hooks",
        ),
        (
            open(A::Amend, Target::Uncommitted, &d, fake),
            "Message (•) keep its message  ( ) edit it",
        ),
        (
            open(A::Stash, Target::Uncommitted, &d, fake),
            "[ ] Include untracked files",
        ),
        (
            open(A::ResetChanges, Target::Uncommitted, &d, fake),
            "(•) mixed (unstage all)",
        ),
        (
            open(A::Clean, Target::Uncommitted, &d, fake),
            "[X] Folders too (-d)",
        ),
        (
            open(A::Fetch, Target::Repository, &d, fake),
            "[ ] Prune  [ ] Prune tags",
        ),
        (open(A::FetchRemote, Target::Repository, &d, fake), remotes),
        (open(A::PruneRemote, Target::Repository, &d, fake), remotes),
        (open(A::DeleteRemote, Target::Repository, &d, fake), remotes),
        (
            open(A::AddRemote, Target::Repository, &d, fake),
            "[X] Fetch now",
        ),
        (
            open(A::EditRemote, Target::Repository, &d, fake),
            "Push URL (blank: the fetch URL)",
        ),
        (
            open(A::User, Target::Repository, &d, fake),
            "(•) for this repository",
        ),
        (
            open(A::UserRemove, Target::Repository, &d, fake),
            "(•) this repository",
        ),
        (
            open(
                A::Discard,
                Target::Files(vec![changed("문서.txt")]),
                &d,
                fake,
            ),
            "git discard",
        ),
        (
            open(
                A::Ours,
                Target::Files(vec![conflict("c.txt")]),
                &conflicted,
                fake,
            ),
            "git ours",
        ),
        (
            open(A::StashDrop, Target::Stash(stash_row()), &d, fake),
            "git stash-drop",
        ),
        (
            open(A::DeleteRemoteBranch, remote, &d, fake),
            "git delete-remote-branch",
        ),
        (
            open(A::CheckoutBranch, Target::Label(local("feature")), &d, fake),
            "git checkout-branch",
        ),
        (open(A::Stage, Target::Uncommitted, &d, fake), "git stage"),
        (
            confirm(Action::DeleteBackup {
                stamp: "20261128T101500Z".into(),
            }),
            "Deleting a backup is the one removal sopen does without a backup.",
        ),
        (confirm(Action::Proceed(Proceed::Continue)), "git continue"),
        (
            confirm(Action::Fetch {
                prune: false,
                prune_tags: false,
            }),
            "git fetch",
        ),
    ];
    for (dialog, shows) in cases {
        let id = dialog.draft().id().as_str().to_string();
        let fx = Fx::new(dialog, fake, 100, 30);
        let title = format!("Fake {id}");
        let l = boxed(&fx.drv, &title).unwrap_or_else(|| panic!("{id}: not in cells"));
        let all = rows(&l).join("\n");
        assert!(all.contains(shows), "{id}: {shows:?} not in\n{all}");
        assert!(!fx.host.covers.is_empty(), "{id}");
        same_in_a_terminal(&fx.drv);
    }
}

/// Edit a remote's fields each hold their own; the Hangul remote's choice loads its URLs; a typed
/// URL reaches the action, a field left alone writes nothing.
#[test]
fn x4d_edit_a_remote_s_fields_and_its_remote() {
    let prefill = Prefill {
        remotes: vec![
            Remote {
                name: "origin".into(),
                url: "https://example.com/x.git".into(),
                push_url: None,
            },
            Remote {
                name: "백업".into(),
                url: "/srv/백업.git".into(),
                push_url: Some("/srv/push.git".into()),
            },
        ],
        ..Prefill::default()
    };
    let opened = ActionDialog::open(A::EditRemote, &Target::Repository, &dirty(), &prefill, fake);
    let mut fx = Fx::new(dialog(opened), fake, 100, 30);
    fx.click("( ) 백업");
    let Action::EditRemote { was, now } = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(
        (was.name.as_str(), now.url.as_str()),
        ("백업", "/srv/백업.git")
    );
    fx.idle(1);
    let l = fx.boxed("Fake edit-remote");
    assert_eq!(
        pen_of(&l, "/srv/백업.git"),
        field_pen(),
        "its URL in the field"
    );
    fx.focus(dialogs::field_id(1));
    fx.drv.key(&mut fx.host, Key::A, testkit::command(false));
    fx.frame(vec![Event::Text("/srv/새.git".into())]);
    fx.press(Key::Enter);
    match &fx.host.ends[..] {
        [DialogEnd::Run {
            action: Action::EditRemote { was, now },
            ..
        }] => {
            assert_eq!(now.url, "/srv/새.git");
            assert_eq!(now.push_url, was.push_url, "the push URL left alone");
            assert_eq!(now.name, "백업");
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// In the panel.

/// The git panel blanks the cells its open dialog covers (`GitView::covered`, X4-graph's seam),
/// and the dialog stands over them in cells.
#[test]
fn x4d_the_panel_blanks_under_its_dialog() {
    let mut view = super::tests::fixture_view();
    view.set_repo(dirty());
    let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
    let asked = fx
        .host
        .view
        .open_action(A::Stash, Target::Uncommitted, Prefill::default());
    assert!(asked.is_empty(), "{asked:?}");
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
    let l = boxed(&fx.drv, "Stash the changes").expect("the dialog in cells");
    assert_eq!(l.buf.area.x, covered[0].x);
}
