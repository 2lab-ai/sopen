//! X4-quick's tests (10 §17): the quick edits' dialogs in cells through the kit (03 §7.8.1) —
//! reword, squash, author and dates — and their plan's confirmation.
//!
//! Each dialog stands in a host that lays a screen of cells under it and blanks what the dialog
//! says it covers first ([`QuickDialog::covers`]), so a terminal (`term::compose` of the pass's
//! layers) shows the window's cells exactly. No git runs: every dialog is given a planner of the
//! test's own, whose plan names the edit it was asked for. Keys and clicks go through the test
//! kit, several in one pass where it matters.

use super::planner::{self, EditPlanner, QuickDialog, QuickEnd, QuickOpened};
use super::tests_s4::repo;
use crate::git::actions::{Confirmation, Plan, Refused, Repo};
use crate::git::model::{CommitRow, Oid};
use crate::git::rebase::{Author, Commit, CommitterDate, Date, Edit, EditId, Quick, Split};
use crate::gui::browser::ops::dialog::choice_id;
use crate::gui::cells::{self, Buffer, Cells, Grid, Layer, Pen, Position};
use crate::gui::chrome::{self, PALETTE};
use crate::gui::term;
use crate::gui::testkit::{self, Driver};
use crate::gui::Window;
use eframe::egui::{self, Color32, Event, Key, Modifiers, Pos2};
use std::cell::RefCell;
use std::sync::Arc;

const CELL: egui::Vec2 = chrome::CELL;

// ---------------------------------------------------------------------------------------------
// Fixtures.

fn oid(c: char) -> Oid {
    Oid::parse(&c.to_string().repeat(40)).expect("a hex digit")
}

/// 2026-11-11 18:24:00 +0900.
const WHEN: Date = Date {
    seconds: 1_794_389_040,
    offset_minutes: 540,
};

fn ada(name: &str) -> Author {
    Author {
        name: name.into(),
        email: "ada@example.com".into(),
        date: WHEN,
        committer: CommitterDate::Keep,
    }
}

/// Commit `c` on `parent` (none: a root), by `name`: its message `<subject>`, a blank line and a
/// body.
fn commit_by(c: char, parent: Option<char>, subject: &str, name: &str) -> Commit {
    Commit {
        row: CommitRow {
            oid: oid(c),
            parents: parent.into_iter().map(oid).collect(),
            author_name: name.into(),
            author_email: "ada@example.com".into(),
            author_time: WHEN.seconds,
            commit_time: WHEN.seconds,
            subject: subject.into(),
        },
        message: format!("{subject}\n\nbody of {subject}\n"),
        message_lossy: false,
        author: ada(name),
        committed: WHEN,
    }
}

fn commit(c: char, parent: Option<char>, subject: &str) -> Commit {
    commit_by(c, parent, subject, "Ada")
}

/// `third` (c) on `second` (b) on `first` (a, the root).
fn third() -> Commit {
    commit('c', Some('b'), "third")
}

fn second() -> Commit {
    commit('b', Some('a'), "second")
}

thread_local! {
    /// Every edit a test's planner was asked for, in order (each test runs on its own thread).
    static EDITS: RefCell<Vec<Edit>> = const { RefCell::new(Vec::new()) };
}

fn edits() -> Vec<Edit> {
    EDITS.with(|e| e.borrow().clone())
}

/// What the fake planner calls an edit.
fn what(edit: &Edit) -> &'static str {
    match edit {
        Edit::Quick(q) => q.id().as_str(),
        Edit::Plan(_) => "plan",
        Edit::SplitStop { .. } => "split-stop",
        Edit::ContinueSplit(_) => "continue-split",
    }
}

/// The command line the fake planner shows for `edit`: every part of it, so what a dialog shows
/// can be held to what it returns.
fn describe(edit: &Edit) -> String {
    match edit {
        Edit::Quick(Quick::Reword { commit, message }) => {
            format!("reword {} {message:?}", commit.short())
        }
        Edit::Quick(Quick::Squash { commit, message }) => {
            format!("squash {} {message:?}", commit.short())
        }
        Edit::Quick(Quick::Author { commit, author }) => format!(
            "author {} {} <{}> {}{:+} {:?}",
            commit.short(),
            author.name,
            author.email,
            author.date.seconds,
            author.date.offset_minutes,
            author.committer
        ),
        other => format!("git {}", what(other)),
    }
}

fn plan(confirm: Confirmation) -> Plan {
    Plan {
        confirm: Some(confirm),
        backups: Vec::new(),
        message: String::new(),
        steps: Vec::new(),
        changes_files: false,
    }
}

/// A planner whose every plan has a confirmation naming the edit in each part.
fn fake(edit: &Edit, _: &Repo) -> Result<Plan, Refused> {
    EDITS.with(|e| e.borrow_mut().push(edit.clone()));
    let name = what(edit);
    Ok(plan(Confirmation {
        title: format!("Fake {name}"),
        loses: Some(format!("{name} loses this")),
        changes: vec![format!("{name}: 11111111 → 22222222")],
        backups: vec!["heads/main".into()],
        commands: vec![describe(edit)],
        notes: vec![format!("{name} note")],
    }))
}

/// [`fake`]'s plan, which also says the git it was planned on: a refresh with another one is
/// another plan.
fn versioned(edit: &Edit, repo: &Repo) -> Result<Plan, Refused> {
    let mut p = fake(edit, repo)?;
    if let Some(c) = &mut p.confirm {
        c.notes = vec![format!("planned on git {}", repo.version.text)];
    }
    Ok(p)
}

/// A planner that refuses everything (03 §8.6).
fn refuse(edit: &Edit, _: &Repo) -> Result<Plan, Refused> {
    EDITS.with(|e| e.borrow_mut().push(edit.clone()));
    Err(Refused("an ignored file is in the way: build/out.o".into()))
}

/// The same repository, read by another git (a refresh's snapshot).
fn refreshed() -> Repo {
    let mut r = repo();
    r.version.text = "2.46.0".into();
    r
}

fn dialog(opened: QuickOpened) -> QuickDialog {
    match opened {
        QuickOpened::Dialog(d) => *d,
        other => panic!("not a dialog: {other:?}"),
    }
}

/// Quick edit `id` on [`third`], [`second`] below it.
fn open(id: EditId, planner: EditPlanner) -> QuickDialog {
    dialog(QuickDialog::open(
        id,
        &third(),
        Some(&second()),
        &repo(),
        planner,
    ))
}

fn reword(message: &str) -> QuickEnd {
    QuickEnd::Run(Edit::Quick(Quick::Reword {
        commit: oid('c'),
        message: message.into(),
    }))
}

// ---------------------------------------------------------------------------------------------
// The host: a screen of cells, the dialog over it.

struct Host {
    dialog: Option<QuickDialog>,
    planner: EditPlanner,
    ends: Vec<QuickEnd>,
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
    fn new(d: QuickDialog, planner: EditPlanner, cols: u16, rows: u16) -> Fx {
        let mut fx = Fx::unseen(d, planner, cols, rows);
        fx.idle(2);
        fx
    }

    /// `d` on a `cols`×`rows` grid, no pass run yet.
    fn unseen(d: QuickDialog, planner: EditPlanner, cols: u16, rows: u16) -> Fx {
        Fx {
            drv: Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y),
            host: Host {
                dialog: Some(d),
                planner,
                ends: Vec::new(),
                covers: Vec::new(),
            },
        }
    }

    /// Another dialog in the same window (its egui memory kept), laid out twice.
    fn reopen(&mut self, d: QuickDialog) {
        self.host.dialog = Some(d);
        self.idle(2);
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

    fn dialog_mut(&mut self) -> &mut QuickDialog {
        self.host.dialog.as_mut().expect("still open")
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

    /// Cmd/Ctrl+A in the field `n`, then `text` typed over all of it.
    fn retype(&mut self, n: usize, text: &str) {
        self.focus(planner::quick_field(n));
        self.drv
            .key(&mut self.host, Key::A, testkit::command(false));
        self.frame(vec![Event::Text(text.into())]);
    }

    fn draft(&self) -> Edit {
        (self.host.dialog.as_ref())
            .expect("still open")
            .draft()
            .clone()
    }

    /// The dialog's layer: the one whose rows hold `title`.
    fn boxed(&self, title: &str) -> Arc<Layer> {
        boxed(&self.drv, title).unwrap_or_else(|| panic!("no layer titled {title:?}"))
    }

    /// The dialog's rows, one string.
    fn shown(&self, title: &str) -> String {
        rows(&self.boxed(title)).join("\n")
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
// The dialogs in cells.

/// F2's Reword in cells: its lost line, the message field (typed into in Hangul, at its start, as
/// a message field opens), the plan — what changes, the backups, the command line (a heading's
/// lines two cells in), the note — then `[ Run ]`, `[ Cancel ]` and the hint; the same cells in a
/// terminal, the caret in the message after what was typed.
#[test]
fn x4q_reword_is_cells_in_both_backends() {
    let mut fx = Fx::new(open(EditId::Reword, fake), fake, 100, 30);
    assert_eq!(fx.focused(), Some(planner::quick_field(0)), "the message");
    fx.frame(vec![Event::Text("한글 ".into())]);
    let message = "한글 third\n\nbody of third\n";
    assert_eq!(
        fx.draft(),
        Edit::Quick(Quick::Reword {
            commit: oid('c'),
            message: message.into()
        })
    );
    let l = fx.boxed("Fake reword");
    let all = rows(&l).join("\n");
    for text in [
        "reword loses this",
        "Message",
        "한글 third",
        "body of third",
        "Changes",
        "  reword: 11111111 → 22222222",
        "Backed up first, under refs/sopen-backup/<stamp>/",
        "  heads/main",
        "Runs",
        &format!("  reword cccccccc {message:?}"),
        "reword note",
        "[ Run ]  [ Cancel ]",
        "Enter runs · Esc cancels",
    ] {
        assert!(all.contains(text), "{text:?} not in the dialog:\n{all}");
    }
    assert_eq!(pen_of(&l, "한글 third"), field_pen(), "the message's cells");
    assert_eq!(pen_of(&l, "Changes"), weak_pen(), "a heading");
    assert_eq!(pen_of(&l, "[ Run ]"), text_pen(), "Run is on");
    assert_eq!(pen_of(&l, "Enter runs"), weak_pen(), "the hint");
    let lost = place(&l, "reword loses this").y;
    assert_eq!(place(&l, "Message").y, lost + 2, "a blank row after it");
    let screen = same_in_a_terminal(&fx.drv);
    let caret = screen.caret.expect("the message's caret");
    let typed = place(&l, "한글 ");
    assert_eq!((caret.x, caret.y), (typed.right(), typed.y), "after it");
    fx.press(Key::Enter);
    assert_eq!(fx.host.ends, [reword(message)]);
}

/// Every quick edit (the history row's and F2's and F8's: Drop's has no options) and a
/// confirmation built elsewhere (an edit stop's Split, a split's Continue) is cells: its box
/// titled with its plan, the plan's command line, the buttons, what it covers, the same cells in a
/// terminal.
#[test]
fn x4q_every_quick_edit_and_confirmation_is_cells() {
    let split = Split {
        commit: oid('c'),
        paths: vec!["문서.txt".into()],
    };
    let mut cases: Vec<QuickDialog> = (EditId::ALL.into_iter())
        .filter(|id| *id != EditId::Plan)
        .map(|id| open(id, fake))
        .collect();
    for edit in [
        Edit::SplitStop { commit: oid('c') },
        Edit::ContinueSplit(split),
    ] {
        cases.push(dialog(QuickDialog::confirm(edit, &repo(), fake)));
    }
    for d in cases {
        let edit = d.draft().clone();
        let name = what(&edit);
        let fx = Fx::new(d, fake, 100, 30);
        let title = format!("Fake {name}");
        let l = boxed(&fx.drv, &title).unwrap_or_else(|| panic!("{name}: not in cells"));
        let all = rows(&l).join("\n");
        let runs = describe(&edit);
        assert!(all.contains(&runs), "{name}: {runs:?} not in\n{all}");
        assert!(all.contains("[ Run ]  [ Cancel ]"), "{name}:\n{all}");
        assert_eq!(fx.host.covers.len(), 3, "{name}: the box and its shadow");
        same_in_a_terminal(&fx.drv);
    }
}

/// Squash's message field holds both messages joined, the older first, a blank line between, a
/// row of the field each of their lines; Enter returns them byte for byte.
#[test]
fn x4q_squash_s_field_holds_both_messages() {
    let mut fx = Fx::new(open(EditId::Squash, fake), fake, 100, 30);
    let l = fx.boxed("Fake squash");
    let first = place(&l, "second");
    assert_eq!(pen_at(&l, first.x, first.y), field_pen());
    let row = |text: &str| place(&l, text).y - first.y;
    assert_eq!(row("body of second"), 2);
    assert_eq!(row("third"), 4);
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.ends,
        [QuickEnd::Run(Edit::Quick(Quick::Squash {
            commit: oid('c'),
            message: "second\n\nbody of second\n\nthird\n\nbody of third\n".into()
        }))]
    );
}

/// Author and date: Name, Email and Author date are fields (Name has the keyboard), the
/// committer date a row of one of several; a click and Space choose it, each plans again; a date
/// that does not read is refused in the error colour, `[ Run ]` off, and Enter runs nothing.
#[test]
fn x4q_author_s_fields_and_its_committer_date() {
    let mut fx = Fx::new(open(EditId::Author, fake), fake, 100, 30);
    assert_eq!(fx.focused(), Some(planner::quick_field(0)), "Name");
    let l = fx.boxed("Fake author");
    let all = rows(&l).join("\n");
    for text in [
        "Name",
        "Email",
        "Author date",
        "Committer date (•) keep  ( ) now  ( ) same as the author date",
    ] {
        assert!(all.contains(text), "{text:?} not in\n{all}");
    }
    for value in ["Ada", "ada@example.com", "2026-11-11 18:24:00 +0900"] {
        assert_eq!(pen_of(&l, value), field_pen(), "{value}");
    }
    let with = |committer| {
        Edit::Quick(Quick::Author {
            commit: oid('c'),
            author: Author {
                committer,
                ..ada("Ada")
            },
        })
    };
    fx.click("( ) now");
    assert_eq!(fx.draft(), with(CommitterDate::Now));
    assert_eq!(edits().last(), Some(&with(CommitterDate::Now)), "planned");
    assert!(fx
        .shown("Fake author")
        .contains(&describe(&with(CommitterDate::Now))));
    fx.focus(choice_id(planner::committer_id(), 2));
    fx.frame(space());
    assert_eq!(fx.draft(), with(CommitterDate::Author));

    // Refused, the dialog shows no plan: its title is the edit's own name.
    fx.retype(2, "yesterday");
    let l = fx.boxed("Author and date");
    assert_eq!(pen_of(&l, planner::BAD_DATE), error_pen());
    assert_eq!(pen_of(&l, "[ Run ]"), weak_pen(), "off");
    let planned = edits().len();
    fx.press(Key::Enter);
    fx.click("[ Run ]");
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    assert_eq!(edits().len(), planned, "not planned while refused");
    fx.press(Key::Escape);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel]);
}

/// `[ Run ]` is off while the plan refuses, the refusal in the error colour; Enter and a click on
/// it run nothing.
#[test]
fn x4q_run_is_off_while_the_plan_refuses() {
    let mut fx = Fx::new(open(EditId::Drop, refuse), refuse, 100, 30);
    let l = fx.boxed("Drop");
    let why = "an ignored file is in the way: build/out.o";
    assert_eq!(pen_of(&l, why), error_pen());
    assert_eq!(pen_of(&l, "[ Run ]"), weak_pen());
    fx.press(Key::Enter);
    fx.click("[ Run ]");
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
}

/// What the dialog covers is its box and its shadow, and the screen under it is blank there.
#[test]
fn x4q_the_dialog_covers_its_box_and_shadow() {
    let fx = Fx::new(open(EditId::Reword, fake), fake, 100, 30);
    let l = fx.boxed("Fake reword");
    assert_eq!(fx.host.covers.len(), 3, "{:?}", fx.host.covers);
    let frame = fx.host.covers[0];
    assert_eq!(
        cells::row_text(&l.buf, frame.y).0.chars().next(),
        Some('╔'),
        "the box's corner"
    );
    let under = cells::layers(&fx.drv.ctx)[0].clone();
    let seen = under.seen();
    for strip in &fx.host.covers[1..] {
        for y in strip.top()..strip.bottom() {
            for x in strip.left()..strip.right() {
                let cell = &seen[usize::from(y)][usize::from(x)].symbol;
                assert_eq!(cell, " ", "the screen blank under the shadow at {x},{y}");
            }
        }
    }
}

/// A box the grid holds less of than all (Author's fields never give way) says so first, in the
/// error colour; its Run is off and Enter edits nothing, Esc still cancels.
#[test]
fn x4q_a_box_taller_than_the_grid_never_runs() {
    let mut fx = Fx::new(open(EditId::Author, fake), fake, 100, 14);
    let l = fx.boxed(super::dialogs::TOO_SHORT);
    assert_eq!(pen_of(&l, super::dialogs::TOO_SHORT), error_pen());
    assert_eq!(pen_of(&l, "[ Run ]"), weak_pen(), "off");
    fx.press(Key::Enter);
    fx.frame(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    fx.press(Key::Escape);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel]);
}

// ---------------------------------------------------------------------------------------------
// The keyboard, and what runs.

/// Typing, Shift+Enter and Enter in one pass: the message holds the new line, but the screen
/// showed the plan before it, so that Enter runs nothing and the hint says the plan changed; the
/// next Enter runs exactly what was typed.
#[test]
fn x4q_typing_and_enter_in_one_pass_show_the_plan_before_it_runs() {
    let mut fx = Fx::new(open(EditId::Reword, fake), fake, 100, 30);
    fx.frame(vec![
        Event::Text("가".into()),
        key(Key::Enter, true, Modifiers::SHIFT),
        Event::Text("나".into()),
        down(Key::Enter),
    ]);
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    let all = fx.shown("Fake reword");
    assert!(all.contains(super::dialogs::CHANGED), "{all}");
    fx.press(Key::Enter);
    assert_eq!(fx.host.ends, [reword("가\n나third\n\nbody of third\n")]);
}

/// Only what the screen showed runs, each in raw passes with none between: a refresh with another
/// plan in the pass of the Enter, and Enter in the dialog's first passes (nothing of it seen) run
/// nothing, and the next Enter runs what shows then.
#[test]
fn x4q_only_what_the_last_pass_painted_runs() {
    let mut fx = Fx::new(open(EditId::Drop, versioned), versioned, 100, 30);
    assert!(fx.shown("Fake drop").contains("planned on git 2.45.0"));
    fx.dialog_mut().set_repo(refreshed());
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "refresh: {:?}", fx.host.ends);
    assert!(fx.shown("Fake drop").contains("planned on git 2.46.0"));
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends.len(), 1, "the plan shown runs");

    let mut fx = Fx::unseen(open(EditId::Drop, fake), fake, 100, 30);
    fx.raw(vec![down(Key::Enter)]);
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "first passes: {:?}", fx.host.ends);
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends.len(), 1);
}

/// An option in one pass and Enter in the next: that frame painted the old plan beside the new
/// choice, so nothing runs; the next Enter runs the new one.
#[test]
fn x4q_an_option_then_enter_in_the_next_pass_runs_nothing() {
    let mut fx = Fx::new(open(EditId::Author, fake), fake, 100, 30);
    fx.focus(choice_id(planner::committer_id(), 1));
    fx.raw(space());
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "the old plan: {:?}", fx.host.ends);
    fx.raw(vec![down(Key::Enter)]);
    match &fx.host.ends[..] {
        [QuickEnd::Run(Edit::Quick(Quick::Author { author, .. }))] => {
            assert_eq!(author.committer, CommitterDate::Now)
        }
        other => panic!("{other:?}"),
    }
}

/// A pass the host did not print is never what the owner saw: printed with plan A, a refresh's
/// plan B in a pass not printed, then Enter runs nothing; once B is printed, Enter runs it. A pass
/// not printed that changes nothing keeps what shows: Enter after it runs.
#[test]
fn x4q_a_pass_not_printed_was_never_seen() {
    let mut fx = Fx::new(open(EditId::Drop, versioned), versioned, 100, 30);
    fx.dialog_mut().set_repo(refreshed());
    fx.raw(vec![]);
    cells::set_presented(&fx.drv.ctx, false);
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "plan B unseen: {:?}", fx.host.ends);
    cells::set_presented(&fx.drv.ctx, true);
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends.len(), 1);

    let mut fx = Fx::new(open(EditId::Drop, fake), fake, 100, 30);
    fx.raw(vec![]);
    cells::set_presented(&fx.drv.ctx, false);
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends.len(), 1, "nothing changed: it runs");
}

/// Each Enter goes to what has the keyboard at its point of the pass: on Cancel it cancels;
/// `[Tab, Enter]` from Run presses Cancel and from the message runs (the Tab gave Run the
/// keyboard); Enter and Esc in one pass cancel; Shift+Enter is the message's new line.
#[test]
fn x4q_enter_goes_to_what_has_the_keyboard() {
    let d = || open(EditId::Reword, fake);
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.focus(planner::quick_cancel_id());
    fx.press(Key::Enter);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel], "Enter on Cancel");

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.focus(planner::quick_run_id());
    fx.frame(vec![down(Key::Tab), down(Key::Enter)]);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel], "the Tab, then Enter");

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.frame(vec![down(Key::Tab), down(Key::Enter)]);
    assert_eq!(fx.host.ends, [reword("third\n\nbody of third\n")], "on Run");

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.frame(vec![down(Key::Enter), down(Key::Escape)]);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel]);

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.frame(vec![key(Key::Enter, true, Modifiers::SHIFT)]);
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    let Edit::Quick(Quick::Reword { message, .. }) = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(
        message, "\nthird\n\nbody of third\n",
        "a new line at its start"
    );
}

/// egui moves the keyboard for a Shift+Tab on the next pass, and once a pass: `[Shift+Tab, Enter]`
/// from Run runs nothing and leaves the keyboard on the message, where the same keys in two passes
/// land; `[Tab, Tab, Enter]` from the message runs nothing (egui stops on Run).
#[test]
fn x4q_an_enter_egui_cannot_follow_is_dropped() {
    let d = || open(EditId::Reword, fake);
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.focus(planner::quick_run_id());
    fx.frame(vec![
        key(Key::Tab, true, Modifiers::SHIFT),
        down(Key::Enter),
    ]);
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    assert_eq!(fx.focused(), Some(planner::quick_field(0)));

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.frame(vec![down(Key::Tab), down(Key::Tab), down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "ran: {:?}", fx.host.ends);
    assert_eq!(
        fx.focused(),
        Some(planner::quick_run_id()),
        "egui moved once"
    );
}

/// egui moves the keyboard for an arrow at the pass's end, where no field has it: `[→, Enter]`
/// and `[→, Space]` from a focused `[ Run ]` press nothing and leave the keyboard on Cancel. In a
/// field the arrows are its caret's: `[←, Enter]` in author's date field runs.
#[test]
fn x4q_an_arrow_moves_the_keyboard_only_outside_a_field() {
    for last in [vec![down(Key::Enter)], space()] {
        let mut fx = Fx::new(open(EditId::Drop, fake), fake, 100, 30);
        fx.focus(planner::quick_run_id());
        let mut events = vec![down(Key::ArrowRight)];
        events.extend(last.clone());
        fx.frame(events);
        assert!(fx.host.ends.is_empty(), "{last:?}: {:?}", fx.host.ends);
        assert_eq!(fx.focused(), Some(planner::quick_cancel_id()), "{last:?}");
    }
    let mut fx = Fx::new(open(EditId::Author, fake), fake, 100, 30);
    fx.focus(planner::quick_field(2));
    fx.frame(vec![down(Key::ArrowLeft), down(Key::Enter)]);
    assert!(
        matches!(
            &fx.host.ends[..],
            [QuickEnd::Run(Edit::Quick(Quick::Author { .. }))]
        ),
        "{:?}",
        fx.host.ends
    );
}

/// A Space after a Shift+Tab in a pass that began on a text field is the field's blank (the kit
/// keeps any other control from pressing it): the message gets `a b` from
/// `[Shift+Tab, a, Space, b]`.
#[test]
fn x4q_a_space_after_a_shift_tab_in_a_field_is_typed() {
    let mut fx = Fx::new(open(EditId::Reword, fake), fake, 100, 30);
    let mut events = vec![
        key(Key::Tab, true, Modifiers::SHIFT),
        Event::Text("a".into()),
    ];
    events.extend(space());
    events.push(Event::Text("b".into()));
    fx.frame(events);
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    let Edit::Quick(Quick::Reword { message, .. }) = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(message, "a bthird\n\nbody of third\n");
}

/// The buttons take the kit's presses (F-kit-tab): `[Tab, Space]` from the message types the blank
/// there and presses nothing the Tab reaches; `[Space, Tab]` on a focused `[ Run ]` runs once and
/// never presses the Cancel the Tab reached.
#[test]
fn x4q_space_with_a_tab_presses_only_what_had_the_keyboard() {
    let d = || open(EditId::Reword, fake);
    let mut fx = Fx::new(d(), fake, 100, 30);
    let mut events = vec![down(Key::Tab)];
    events.extend(space());
    fx.frame(events);
    assert!(
        fx.host.ends.is_empty(),
        "Run not pressed: {:?}",
        fx.host.ends
    );
    assert_eq!(fx.focused(), Some(planner::quick_run_id()));
    let Edit::Quick(Quick::Reword { message, .. }) = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(
        message, " third\n\nbody of third\n",
        "the blank typed there"
    );

    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.focus(planner::quick_run_id());
    let mut events = space();
    events.push(down(Key::Tab));
    fx.frame(events);
    assert_eq!(
        fx.host.ends,
        [reword("third\n\nbody of third\n")],
        "once, not Cancel"
    );
}

/// The pointer: a click on `[ Run ]` runs, on `[ Cancel ]` cancels, outside the box cancels.
#[test]
fn x4q_clicks_on_the_buttons_and_outside_the_box() {
    let d = || open(EditId::Drop, fake);
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.click("[ Run ]");
    assert_eq!(fx.host.ends.len(), 1, "{:?}", fx.host.ends);
    assert!(matches!(fx.host.ends[0], QuickEnd::Run(_)));
    let mut fx = Fx::new(d(), fake, 100, 30);
    fx.click("[ Cancel ]");
    assert_eq!(fx.host.ends, [QuickEnd::Cancel]);
    let mut fx = Fx::new(d(), fake, 100, 30);
    let frame = fx.host.covers[0];
    assert!(frame.x > 2, "room left of the box: {frame:?}");
    fx.drv.click(&mut fx.host, at_cell(1, frame.y + 2));
    fx.idle(1);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel], "a click outside it");
}

/// A dialog's fields open afresh, whatever an earlier quick edit's dialog left under the same ids:
/// the message with its caret at its start (the kit's rule for a message field), a one-line field
/// with its caret at its end (as a new field opens) — never where the last dialog's caret stood.
#[test]
fn x4q_a_dialog_s_fields_open_afresh() {
    let mut fx = Fx::new(open(EditId::Author, fake), fake, 100, 30);
    fx.press(Key::Home);
    fx.press(Key::Escape);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel]);
    let grace = commit_by('c', Some('b'), "third", "Grace Hopper");
    fx.reopen(dialog(QuickDialog::open(
        EditId::Author,
        &grace,
        None,
        &repo(),
        fake,
    )));
    fx.frame(vec![Event::Text("!".into())]);
    let Edit::Quick(Quick::Author { author, .. }) = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(author.name, "Grace Hopper!", "typed at its end");

    let mut fx = Fx::new(open(EditId::Reword, fake), fake, 100, 30);
    fx.press(Key::End);
    fx.press(Key::Escape);
    fx.reopen(open(EditId::Reword, fake));
    fx.frame(vec![Event::Text("x".into())]);
    let Edit::Quick(Quick::Reword { message, .. }) = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(message, "xthird\n\nbody of third\n", "typed at its start");

    // The next dialog in the very next pass, the message laid in the one before (the kit's own
    // fresh start is for a field it did not lay there): typed at its start all the same.
    let mut fx = Fx::new(open(EditId::Reword, fake), fake, 100, 30);
    fx.press(Key::End);
    fx.raw(vec![down(Key::Escape)]);
    assert_eq!(fx.host.ends, [QuickEnd::Cancel]);
    fx.host.dialog = Some(open(EditId::Reword, fake));
    fx.raw(vec![Event::Text("x".into())]);
    fx.idle(1);
    let Edit::Quick(Quick::Reword { message, .. }) = fx.draft() else {
        panic!("{:?}", fx.draft());
    };
    assert_eq!(message, "xthird\n\nbody of third\n", "in the next pass");
}

// ---------------------------------------------------------------------------------------------
// Through the terminal.

/// A terminal that sends its bursts of events back to back — each waiting while the next is
/// unread, so the backend prints only the last pass of a burst — one burst once the backend has
/// asked twice since the last (its pass is done and printed), then ends the run.
struct Bursts {
    bursts: std::collections::VecDeque<Vec<crossterm::event::Event>>,
    now: std::collections::VecDeque<crossterm::event::Event>,
    asked: usize,
    /// The dialog's ends so far, as the host left them, when each burst was sent.
    ends: std::rc::Rc<std::cell::Cell<usize>>,
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
        if self.asked >= 3 {
            if let Some(burst) = self.bursts.pop_front() {
                self.sent_at.borrow_mut().push(self.ends.get());
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
        (self.bursts.is_empty() && self.now.is_empty() && self.asked >= 3).then_some(libc::SIGTERM)
    }
}

/// [`Host`] in the terminal, saying how many ends it has.
struct InTerminal {
    host: Host,
    ends: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Window for InTerminal {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.host.ui(ui);
        self.ends.set(self.host.ends.len());
    }

    fn ready(&self) -> bool {
        true
    }
}

/// `d` run in a terminal that sends `bursts`: the host's ends, and how many it had as each burst
/// was sent.
fn in_a_terminal(
    d: QuickDialog,
    planner: EditPlanner,
    bursts: Vec<Vec<crossterm::event::Event>>,
) -> (Vec<QuickEnd>, Vec<usize>) {
    let ends = std::rc::Rc::new(std::cell::Cell::new(0));
    let sent_at = std::rc::Rc::new(RefCell::new(Vec::new()));
    let tty = Bursts {
        bursts: bursts.into(),
        now: Default::default(),
        asked: 0,
        ends: ends.clone(),
        sent_at: sent_at.clone(),
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut b = term::Backend::new(tty, std::io::sink(), ctx, true);
    let mut win = InTerminal {
        host: Host {
            dialog: Some(d),
            planner,
            ends: Vec::new(),
            covers: Vec::new(),
        },
        ends,
    };
    let mut no_switch =
        |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, term::End::Signal(libc::SIGTERM));
    let sent = sent_at.borrow().clone();
    (win.host.ends, sent)
}

/// The terminal prints no pass while input waits, and a pass it never printed was never seen:
/// Reword's message typed into and Enter in one read (`[x, a focus report, Enter]`) run nothing —
/// the screen still showed the message without the `x` — and the next Enter, after the new plan
/// was printed, runs it with the `x`.
#[test]
fn x4q_a_pass_the_terminal_never_printed_was_never_seen() {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
    let press = |code| CEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let bursts = vec![
        vec![CEvent::FocusGained],
        vec![CEvent::FocusGained],
        vec![
            press(KeyCode::Char('x')),
            CEvent::FocusGained,
            press(KeyCode::Enter),
        ],
        vec![press(KeyCode::Enter)],
    ];
    let (ends, sent_at) = in_a_terminal(open(EditId::Reword, fake), fake, bursts);
    assert_eq!(sent_at, [0, 0, 0, 0], "nothing ran before the last Enter");
    assert_eq!(ends, [reword("xthird\n\nbody of third\n")]);
}

/// A click is two passes (its press and its release), and when they come in one read the press's
/// pass is not printed: a click on `[ Run ]` still runs, the screen showing exactly what it ran.
#[test]
fn x4q_a_click_on_run_in_one_read_of_the_terminal_runs() {
    use crossterm::event::{
        Event as CEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    let fx = Fx::new(open(EditId::Drop, fake), fake, 100, 30);
    let run = place(&fx.boxed("Fake drop"), "[ Run ]");
    let mouse = |kind| {
        CEvent::Mouse(MouseEvent {
            kind,
            column: run.x + 2,
            row: run.y,
            modifiers: KeyModifiers::NONE,
        })
    };
    let bursts = vec![
        vec![CEvent::FocusGained],
        vec![CEvent::FocusGained],
        vec![
            mouse(MouseEventKind::Down(MouseButton::Left)),
            mouse(MouseEventKind::Up(MouseButton::Left)),
        ],
    ];
    let (ends, _) = in_a_terminal(open(EditId::Drop, fake), fake, bursts);
    assert!(
        matches!(&ends[..], [QuickEnd::Run(Edit::Quick(Quick::Drop { .. }))]),
        "the click was refused: {ends:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// In the panel.

/// The git panel's quick edit dialog is one of its dialogs (`dialog_up`): the panel blanks the
/// cells it covers (`GitView::covered`, X4-graph's seam), and it stands over them in cells.
#[test]
fn x4q_the_panel_blanks_under_its_quick_dialog() {
    let mut view = super::tests::fixture_view();
    view.set_repo(repo());
    let mut fx = super::tests::Fx::new(1000.0, 600.0, view);
    let model = super::tests_s4::model();
    let asked = fx.host.view.open_edit(EditId::Reword, &oid('c'), &model);
    assert!(asked.is_empty(), "{asked:?}");
    assert!(fx.host.view.dialog_up(), "a dialog of the panel's");
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
    let stood = covered[0].union(covered[1]).union(covered[2]);
    let boxes = cells::layers(&fx.drv.ctx);
    let l = (boxes.iter())
        .find(|l| l.buf.area == stood)
        .expect("the dialog in cells, where it said it stands");
    assert!(rows(l).join("\n").contains("Message"), "{:#?}", rows(l));
}
