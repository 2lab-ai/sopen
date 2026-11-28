//! X4-planner2's tests (10 §17): the planner's own dialogs in cells (03 §7.8.2) — a row's
//! message, the author and dates, the run's confirmation and Esc's question.

use super::planner::{self, PlannerEnd, PlannerView, Seams};
use super::tests_s4::{model, repo};
use crate::git::actions::{Confirmation, Plan, Refused, Repo};
use crate::git::model::{Change, StatusEntry, StatusState};
use crate::git::rebase::{Edit, Model, Row, Verb};
use crate::gui::browser::ops::dialog::choice_id;
use crate::gui::cells::{self, Buffer, Cells, Grid, Layer, Pen, Position};
use crate::gui::chrome::{self, CELL, PALETTE};
use crate::gui::term;
use crate::gui::testkit::{self, Driver};
use crate::gui::Window;
use eframe::egui::{self, Event, Key, Modifiers};
use std::cell::RefCell;
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Fixtures: the planner over a screen of cells, with a plan that says what it plans.

/// A plan naming each part of the model it plans and the git it was planned on (a refresh with
/// another git is another plan); no message goes in it, as none goes in the real one's lines.
fn fake(edit: &Edit, repo: &Repo) -> Result<Plan, Refused> {
    let Edit::Plan(m) = edit else {
        return Err(Refused("not a plan".into()));
    };
    let verbs: Vec<&str> = m.rows.iter().map(|r| r.verb.word()).collect();
    let line = format!(
        "plan {} stash={} refs={} git {}",
        verbs.join(","),
        m.autostash,
        m.update_refs,
        repo.version.text
    );
    Ok(Plan {
        confirm: Some(Confirmation {
            title: "Rewrite main".into(),
            loses: Some("4 commits are rewritten".into()),
            changes: vec!["eeeeeeee fifth: rewritten".into()],
            backups: vec!["heads/main".into()],
            commands: vec![line],
            notes: Vec::new(),
        }),
        backups: Vec::new(),
        message: String::new(),
        steps: Vec::new(),
        changes_files: false,
    })
}

/// The real model's seams (`Model::apply`, `changed`, `problems`), with [`fake`]'s plan.
const SEAMS: Seams = Seams {
    plan: fake,
    ..planner::SEAMS
};

/// `main` at `e`, read by another git (a refresh's snapshot).
fn refreshed() -> Repo {
    let mut r = repo();
    r.version.minor = 46;
    r.version.text = "2.46.0".into();
    r
}

/// The repository with a staged change: a run without `--autostash` is refused.
fn staged() -> Repo {
    let mut r = repo();
    r.status.entries = vec![StatusEntry {
        path: "s.txt".into(),
        old_path: None,
        state: StatusState::Changed {
            staged: Some(Change::Modified),
            unstaged: None,
        },
        lossy: false,
    }];
    r
}

/// The planner over all of the grid's cells, its dialogs on the same grid, as the panel draws it.
struct Host {
    view: PlannerView,
    ends: Vec<PlannerEnd>,
    /// What the planner said its dialog covers, last pass (the panel blanks it).
    covers: Vec<Cells>,
    /// The planner has the keyboard (`false`: another dialog has it, over the planner's).
    active: bool,
    /// The typed text the planner's pass left in the input, for a dialog drawn after it.
    left: Vec<String>,
}

impl Host {
    fn new(view: PlannerView) -> Host {
        Host {
            view,
            ends: Vec::new(),
            covers: Vec::new(),
            active: true,
            left: Vec::new(),
        }
    }
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let grid = Grid::over(ui.max_rect());
                let mut buf = Buffer::empty(grid.all());
                let (view, active) = (&mut self.view, self.active);
                let end = planner::draw(ui, &grid, grid.all(), &mut buf, view, active, &SEAMS);
                let texts = ui.input(|i| {
                    (i.events.iter())
                        .filter_map(|e| match e {
                            Event::Text(t) => Some(t.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                });
                self.left.extend(texts);
                self.covers = self.view.covers(&grid);
                for c in &self.covers {
                    cells::fill(&mut buf, *c, Pen::new(PALETTE.text, PALETTE.back));
                }
                let painter = ui.painter_at(grid.rect(grid.all()));
                cells::paint(&painter, grid.origin, &buf, None);
                self.ends.extend(end);
            });
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
    /// The planner on `m` and `repo` in a 100×30-cell window, laid out twice.
    fn new(m: Model, repo: &Repo) -> Fx {
        let mut fx = Fx {
            drv: Driver::new(100.0 * CELL.x, 30.0 * CELL.y),
            host: Host::new(PlannerView::new(m, repo)),
        };
        fx.idle(2);
        fx
    }

    fn idle(&mut self, n: usize) {
        self.drv.idle(&mut self.host, n);
    }

    /// One pass with `events` and no other: what the owner saw is the pass before.
    fn raw(&mut self, events: Vec<Event>) {
        self.drv.frame(&mut self.host, events, Modifiers::NONE);
    }

    /// One pass with `events`, then one without.
    fn frame(&mut self, events: Vec<Event>) {
        self.raw(events);
        self.idle(1);
    }

    fn press(&mut self, key: Key) {
        self.drv.key(&mut self.host, key, Modifiers::NONE);
    }

    fn typed(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.into())]);
    }

    /// Cmd/Ctrl+Enter: the run's confirmation.
    fn run(&mut self) {
        self.drv
            .key(&mut self.host, Key::Enter, testkit::command(false));
    }

    fn up(&self) -> bool {
        self.host.view.dialog_up()
    }

    fn row(&self, k: usize) -> &Row {
        &self.host.view.model().rows[k]
    }

    fn focus(&mut self, on: egui::Id) {
        self.drv.ctx.memory_mut(|m| m.request_focus(on));
        self.idle(2);
    }

    fn focused(&self) -> Option<egui::Id> {
        self.drv.ctx.memory(|m| m.focused())
    }

    /// The run's confirmation's rows, one string.
    fn shown(&self) -> String {
        rows(&boxed(&self.drv, "Rewrite main").expect("the confirmation")).join("\n")
    }
}

fn key(k: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key: k,
        physical_key: Some(k),
        pressed,
        repeat: false,
        modifiers,
    }
}

fn down(k: Key) -> Event {
    key(k, true, Modifiers::NONE)
}

fn cmd(k: Key) -> Event {
    key(k, true, Modifiers::COMMAND)
}

fn text(t: &str) -> Event {
    Event::Text(t.into())
}

fn preedit(t: &str) -> Event {
    Event::Ime(egui::ImeEvent::Preedit {
        text: t.into(),
        active_range_chars: None,
    })
}

/// The message of [`model`]'s newest row, `e`'s.
const FIFTH: &str = "fifth\n\nbody of fifth\n";

/// The layer whose rows hold `title`: a dialog's box.
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

/// The foreground of the first cell of `text` in the layer `l`.
fn ink_of(l: &Arc<Layer>, text: &str) -> Option<egui::Color32> {
    let hits = cells::find(std::slice::from_ref(l), text);
    let at = hits.first()?.cells;
    let a = l.buf.area;
    l.seen()[usize::from(at.y - a.y)][usize::from(at.x - a.x)].fg
}

/// What a terminal shows of the last pass (`term::compose`) is every cell the window's: per cell
/// the dialog's where its box stands, else the screen's under it — the dialog's area over the
/// screen's whatever order the two were painted in (the pane paints its buffer after its dialog,
/// as the panel does) — the same character, foreground and background, and the same caret; no
/// hand-off box.
fn same_in_a_terminal(drv: &Driver) -> term::Screen {
    let layers = cells::layers(&drv.ctx);
    let r = drv.ctx.content_rect();
    let (cols, rows) = ((r.width() / CELL.x) as u16, (r.height() / CELL.y) as u16);
    let screen = term::compose(&drv.ctx, &layers, cols, rows);
    assert_eq!(screen.hand_offs, vec![], "every cell is the window's");
    let all = Cells::new(0, 0, cols, rows);
    let mut over = layers.clone();
    over.sort_by_key(|l| l.buf.area != all);
    for y in 0..rows {
        for x in 0..cols {
            let at = Position::new(x, y);
            let want = (over.iter().rev())
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
    let caret = over.iter().rev().find_map(|l| l.caret);
    assert_eq!(screen.caret, caret, "the caret's cell");
    screen
}

// ---------------------------------------------------------------------------------------------
// The dialogs in cells.

/// Enter on a reword row, `A`, Esc after a change and Cmd/Ctrl+Enter: each dialog is the kit's
/// box in cells, titled, with its parts — the message's field, OK, Cancel and its hint; the
/// author's three fields, its committer date and its buttons; the question; the run's lost line,
/// its options (`--update-refs` offered on git 2.45), the plan, Run and Cancel — the same cells in
/// a terminal (no hand-off box), and the planner says it covers its box and its shadow, nothing
/// once it is closed.
#[test]
fn x4p2_each_dialog_is_cells_in_both_backends() {
    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    let cases: [(&str, &str, &[&str]); 4] = [
        (
            "Enter",
            "eeeeeeee fifth: its message",
            &[
                "Message",
                "body of fifth",
                "[ OK ]  [ Cancel ]",
                "Enter keeps it · Shift+Enter a new line · Esc cancels",
            ],
        ),
        (
            "a",
            "eeeeeeee fifth: author and dates",
            &[
                "Name Ada",
                "Email ada@example.com",
                "Author date 2026-11-11 18:24:00 +0900",
                "Committer date",
                "(•) keep",
                "( ) now",
                "( ) same as the author date",
                "[ OK ]  [ The commit's own ]  [ Cancel ]",
                "Enter keeps it · Esc cancels",
            ],
        ),
        (
            "Esc",
            "Close the planner",
            &[
                "Close the planner and lose its changes?",
                "[ Close ]  [ Keep planning ]",
                "Enter closes · Esc keeps planning",
            ],
        ),
        (
            "Cmd+Enter",
            "Rewrite main",
            &[
                "4 commits are rewritten",
                "[ ] Stash my changes for it",
                "[ ] Move other branches along",
                "  plan reword,pick,pick,pick stash=false refs=false git 2.45.0",
                "[ Run ]  [ Cancel ]",
                "Enter runs · Esc cancels",
            ],
        ),
    ];
    for (opener, title, parts) in cases {
        match opener {
            "Enter" => fx.press(Key::Enter),
            "a" => fx.typed("a"),
            "Esc" => fx.press(Key::Escape),
            _ => fx.run(),
        }
        fx.idle(1);
        assert!(fx.up(), "{opener}");
        let l = boxed(&fx.drv, title).unwrap_or_else(|| panic!("{opener}: no box titled {title}"));
        let all = rows(&l).join("\n");
        for part in parts {
            assert!(all.contains(part), "{opener}: {part:?} in\n{all}");
        }
        let screen = same_in_a_terminal(&fx.drv);
        if opener == "Enter" || opener == "a" {
            assert!(screen.caret.is_some(), "{opener}: the field's caret");
        }
        let covers = fx.host.covers.clone();
        assert_eq!(covers.len(), 3, "{opener}: the box and its shadow");
        assert_eq!(covers[0].union(covers[1]).union(covers[2]), l.buf.area);
        fx.press(Key::Escape);
        assert!(!fx.up(), "{opener}: Esc closes it");
        assert!(
            fx.host.covers.is_empty(),
            "{opener}: a closed dialog covers nothing"
        );
    }
}

/// In the git panel: the planner's dialog is one of the panel's (`dialog_up`), which blanks the
/// cells it covers and stands over them in cells; while another dialog has the keyboard (another
/// panel's, `dialog_elsewhere`) it is drawn but reads no key — typed text and an Enter in that
/// pass neither type nor keep — and once it has the keyboard again Enter keeps what it showed.
#[test]
fn x4p2_a_planner_dialog_is_its_panels_and_reads_no_key_elsewhere() {
    let mut view = super::tests::fixture_view();
    view.set_repo(repo());
    view.open_planner(model());
    let mut fx = super::tests::Fx::new(1000.0, 500.0, view);
    fx.idle();
    fx.typed("r");
    fx.press(Key::Enter);
    assert!(fx.view().dialog_up(), "a dialog of the panel's");
    let grid = Grid::over(fx.host.rect);
    let covered = fx.view().covered(&grid);
    assert_eq!(covered.len(), 3, "the box and its shadow");
    let seen = super::tests_x4_graph::panel(&fx).seen();
    for r in &covered {
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                let s = &seen[usize::from(y)][usize::from(x)];
                let blank = (s.symbol.as_str(), s.bg);
                assert_eq!(blank, (" ", Some(PALETTE.back)), "({x}, {y})");
            }
        }
    }
    let stood = covered[0].union(covered[1]).union(covered[2]);
    let l = (cells::layers(&fx.drv.ctx).into_iter())
        .find(|l| l.buf.area == stood)
        .expect("the dialog in cells, where it said it stands");
    assert!(rows(&l).join("\n").contains("body of fifth"));
    fx.view_mut().set_dialog_elsewhere(true);
    let enter = [true, false].map(|p| key(Key::Enter, p, Modifiers::NONE));
    let mut events = vec![text("x")];
    events.extend(enter);
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert!(fx.view().dialog_up(), "no Enter reached it");
    fx.view_mut().set_dialog_elsewhere(false);
    fx.press(Key::Enter);
    assert!(!fx.view().dialog_up());
    let kept = fx.view().planner_model().map(|m| m.rows[0].message.clone());
    assert_eq!(kept, Some(Some(FIFTH.to_string())), "nothing typed");
}

/// Two git panels side by side, each with a planner (X4-menu's one dialog at a time): while the
/// left one's message dialog is up the right one is `dialog_elsewhere` (as the integration sets
/// it), so its pane opens no dialog — not by Cmd/Ctrl+Enter or `a`, even with the keyboard, nor by
/// a click on its `[ Run… ]` — and changes nothing; the two planners' ids are their own.
#[test]
fn x4p2_one_dialog_at_a_time_across_two_panels() {
    struct Two {
        views: [super::GitView; 2],
        active: [bool; 2],
    }
    impl Window for Two {
        fn ui(&mut self, ui: &mut egui::Ui) {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let grid = Grid::over(ui.max_rect());
                    let half = grid.cols / 2;
                    let sides = [
                        Cells::new(0, 0, half, grid.rows),
                        Cells::new(half, 0, grid.cols - half, grid.rows),
                    ];
                    for (i, side) in sides.into_iter().enumerate() {
                        let up = self.views[1 - i].dialog_up();
                        self.views[i].set_dialog_elsewhere(up);
                        let active = self.active[i];
                        super::draw(ui, &grid, side, &mut self.views[i], active, &[]);
                    }
                });
        }
        fn ready(&self) -> bool {
            true
        }
    }
    let view = || {
        let mut v = super::tests::fixture_view();
        v.set_repo(repo());
        v.open_planner(model());
        v
    };
    let mut two = Two {
        views: [view(), view()],
        active: [true, false],
    };
    let mut drv = Driver::new(1600.0, 500.0);
    drv.idle(&mut two, 2);
    let id = |two: &Two, i: usize| (two.views[i].planner.as_ref()).map(|p| p.part_id("ok"));
    assert_ne!(id(&two, 0), id(&two, 1), "each planner's own");
    drv.frame(&mut two, vec![text("r")], Modifiers::NONE);
    drv.key(&mut two, Key::Enter, Modifiers::NONE);
    assert!(two.views[0].dialog_up(), "the left one's message");
    // The right one has the keyboard too: still no dialog of its own.
    two.active = [true, true];
    let right = two.views[1].planner_model().cloned();
    drv.key(&mut two, Key::Enter, testkit::command(false));
    drv.frame(&mut two, vec![text("a")], Modifiers::NONE);
    drv.idle(&mut two, 1);
    assert!(!two.views[1].dialog_up(), "the right one opened none");
    assert_eq!(two.views[1].planner_model().cloned(), right, "nor changed");
    assert!(two.views[0].dialog_up(), "the left one's is still up");
    let runs = drv.texts_starting_with("[ Run… ]");
    let (at, _) = (runs.iter())
        .max_by(|a, b| a.0.left().total_cmp(&b.0.left()))
        .expect("the right pane's foot");
    drv.click(&mut two, at.center());
    drv.idle(&mut two, 1);
    assert!(!two.views[1].dialog_up(), "a click on its Run… neither");
}

// ---------------------------------------------------------------------------------------------
// What runs.

/// Only the plan the screen showed runs, each in raw passes: Cmd/Ctrl+Enter and two Enters in one
/// pass run nothing (the confirmation was never on screen); Cmd/Ctrl+Enter, Esc, Cmd/Ctrl+Enter
/// and Enter neither (the second opening shows from the next pass, the rest of that pass dropped);
/// once shown, Enter runs exactly the model shown and hands on the plan the box showed.
#[test]
fn x4p2_only_the_plan_the_screen_showed_runs() {
    let mut fx = Fx::new(model(), &repo());
    fx.raw(vec![cmd(Key::Enter), down(Key::Enter), down(Key::Enter)]);
    assert!(fx.up(), "the confirmation");
    assert!(
        fx.host.ends.is_empty(),
        "never on screen: {:?}",
        fx.host.ends
    );

    let mut fx = Fx::new(model(), &repo());
    fx.raw(vec![
        cmd(Key::Enter),
        down(Key::Escape),
        cmd(Key::Enter),
        down(Key::Enter),
    ]);
    assert!(fx.up(), "opened again");
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    // Its first pass lays it out unseen (egui's new modal), the next paints it.
    fx.idle(2);
    fx.raw(vec![down(Key::Enter)]);
    let edit = Edit::Plan(model());
    assert_eq!(fx.host.ends, [PlannerEnd::Run(edit.clone())]);
    let (shown, plan) = fx.host.view.take_shown().expect("the plan shown");
    assert_eq!(shown, edit);
    let line = "plan pick,pick,pick,pick stash=false refs=false git 2.45.0";
    let commands = plan.confirm.map(|c| c.commands).unwrap_or_default();
    assert_eq!(commands, [line], "the plan the box showed");
}

/// A refresh with another plan in the pass of the Enter, and an option chosen in the same pass as
/// the Enter, run nothing: the screen showed the plan before; the next Enter runs what shows.
#[test]
fn x4p2_a_refresh_or_an_option_in_the_enters_pass_runs_nothing() {
    let mut fx = Fx::new(model(), &repo());
    fx.run();
    fx.idle(1);
    fx.host.view.set_repo(refreshed());
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "refresh: {:?}", fx.host.ends);
    assert!(fx.shown().contains("git 2.46.0"), "the new plan shows");
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends.len(), 1, "the plan shown runs");

    let mut fx = Fx::new(model(), &repo());
    fx.run();
    let options = fx.host.view.run_ids().options();
    fx.focus(choice_id(options, 0));
    fx.raw(vec![down(Key::Space), text(" "), down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "an option: {:?}", fx.host.ends);
    assert!(
        fx.host.view.model().autostash,
        "the run's option, the model's"
    );
    // That pass painted the old plan beside the new choice; the next one paints the new plan.
    fx.idle(1);
    fx.raw(vec![down(Key::Enter)]);
    let mut m = model();
    m.autostash = true;
    assert_eq!(fx.host.ends, [PlannerEnd::Run(Edit::Plan(m))]);
}

/// A check that fails refuses the run: a squash with nothing below it is said in the box in the
/// error colour, `[ Run ]` is off and Enter runs nothing; over uncommitted changes the run waits
/// for `--autostash`, its check going once the option is on, and then runs with it.
#[test]
fn x4p2_a_check_that_fails_refuses_the_run() {
    let mut m = model();
    m.rows[3].verb = Verb::Squash;
    let why = (planner::SEAMS.problems)(&m, &repo());
    assert_eq!(why.len(), 1, "{why:?}");
    let mut fx = Fx::new(m, &repo());
    fx.run();
    fx.idle(1);
    let l = boxed(&fx.drv, "Rewrite main").expect("the box");
    assert_eq!(ink_of(&l, &why[0]), Some(PALETTE.dialog_error), "the check");
    assert_eq!(
        ink_of(&l, "[ Run ]"),
        Some(PALETTE.dialog_weak),
        "Run is off"
    );
    fx.press(Key::Enter);
    fx.press(Key::Enter);
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);

    let mut fx = Fx::new(model(), &staged());
    fx.run();
    fx.idle(1);
    let check = (planner::SEAMS.problems)(&model(), &staged());
    // Wrapped in the box: its words up to the first colon.
    let check = &check[0][..check[0].find(':').expect("a colon")];
    assert!(fx.shown().contains(check), "{}", fx.shown());
    fx.press(Key::Enter);
    assert!(fx.host.ends.is_empty());
    fx.drv
        .click_text_on_top(&mut fx.host, "[ ] Stash my changes for it");
    fx.idle(1);
    assert!(!fx.shown().contains(check), "{}", fx.shown());
    fx.press(Key::Enter);
    assert!(
        matches!(&fx.host.ends[..], [PlannerEnd::Run(Edit::Plan(m))] if m.autostash),
        "{:?}",
        fx.host.ends
    );
}

// ---------------------------------------------------------------------------------------------
// Through the terminal.

/// A terminal that sends its bursts of events back to back — each waiting while the next is
/// unread, so the backend prints only the last pass of a burst — one burst once the backend has
/// asked twice since the last, then ends the run (`tests_x4_dialogs.rs`' stand-in).
struct Bursts {
    bursts: std::collections::VecDeque<Vec<crossterm::event::Event>>,
    now: std::collections::VecDeque<crossterm::event::Event>,
    asked: usize,
    /// The planner's ends so far, as the host left them, when each burst was sent.
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

/// The terminal prints no pass while input waits, and a pass it never printed was never seen:
/// the run's confirmation on screen, then `[Tab, Space, a focus report, Enter]` in one read — the
/// Space turning `--autostash` on, the report's pass painting the new plan, neither printed —
/// runs nothing (a guard of the last pass painted, not printed, would run it); the next Enter,
/// once the new plan was printed, runs it with the option.
#[test]
fn x4p2_a_pass_the_terminal_never_printed_was_never_seen() {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
    let press = |code| CEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let settle = || vec![CEvent::FocusGained];
    let bursts = vec![
        settle(),
        settle(),
        vec![CEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::CONTROL,
        ))],
        settle(),
        settle(),
        vec![
            press(KeyCode::Tab),
            press(KeyCode::Char(' ')),
            CEvent::FocusGained,
            press(KeyCode::Enter),
        ],
        settle(),
        vec![press(KeyCode::Enter)],
    ];
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
        host: Host::new(PlannerView::new(model(), &repo())),
        ends,
    };
    let mut no_switch =
        |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, term::End::Signal(libc::SIGTERM));
    assert_eq!(
        *sent_at.borrow(),
        [0; 8],
        "nothing ran before the last Enter"
    );
    let mut m = model();
    m.autostash = true;
    assert_eq!(win.host.ends, [PlannerEnd::Run(Edit::Plan(m))]);
}

// ---------------------------------------------------------------------------------------------
// The keyboard and what is typed.

/// Messages are kept byte for byte (03 §3.3): a message of quotes, `\n` as text, `%`, `$`,
/// backquotes, a `#` line, Hangul, a tab, CR, ESC, a leading blank line and trailing blanks comes
/// back unchanged from the message dialog when nothing is typed. No pass copies a message: with a
/// mebibyte of message on a row, ten idle passes of the message dialog or of the run's
/// confirmation copy none of it — the confirmation's plan copies the model once, as it opens.
#[test]
fn x4p2_messages_are_kept_byte_for_byte_and_no_pass_copies_them() {
    let odd = "\nsubject \"q\" 'q' \\n % $ `x`\n# not a comment\n한글\ttab\rcr\x1b[0m  \n\n  \n";
    let mut m = model();
    m.rows[0].commit.message = odd.into();
    let mut fx = Fx::new(m, &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    assert!(fx.up());
    fx.press(Key::Enter);
    assert!(!fx.up());
    assert_eq!(fx.row(0).message.as_deref(), Some(odd));

    let mut m = model();
    m.rows[0].commit.message = "0123456789abcde\n".repeat(1 << 16);
    let bytes: usize = (m.rows.iter())
        .map(|r| r.commit.message.len() + r.message.as_ref().map_or(0, String::len))
        .sum();
    let mut fx = Fx::new(m, &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    assert!(fx.up(), "the message dialog");
    planner::copied();
    fx.idle(10);
    assert_eq!(
        planner::copied(),
        0,
        "ten idle passes of the message dialog"
    );
    fx.press(Key::Escape);
    planner::copied();
    fx.run();
    assert!(fx.up(), "the confirmation");
    assert_eq!(planner::copied(), bytes, "its plan's one copy, as it opens");
    fx.idle(10);
    assert_eq!(planner::copied(), 0, "ten idle passes of the confirmation");
}

/// Hangul composed through an input method: the Enter that commits the composition — while it
/// composes, or in its commit's pass — keeps nothing; the next keeps exactly what was committed.
#[test]
fn x4p2_an_enter_that_commits_a_composition_keeps_nothing() {
    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    for t in ["ㅎ", "하", "한"] {
        fx.frame(vec![preedit(t)]);
    }
    fx.press(Key::Enter);
    assert!(fx.up(), "while composing");
    let commit = Event::Ime(egui::ImeEvent::Commit("한".into()));
    fx.frame(vec![commit, down(Key::Enter)]);
    assert!(fx.up(), "the committing Enter");
    fx.press(Key::Enter);
    assert!(!fx.up());
    assert_eq!(fx.row(0).message, Some(format!("{FIFTH}한")));

    // The Enter before its commit in one pass, while composing or composed in that very pass:
    // the commit still reaches the field (no turn ends at that Enter), and the next Enter keeps it.
    let commit = |t: &str| Event::Ime(egui::ImeEvent::Commit(t.into()));
    for pass in [
        vec![down(Key::Enter), commit("한")],
        vec![preedit("한"), down(Key::Enter), commit("한")],
    ] {
        let mut fx = Fx::new(model(), &repo());
        fx.typed("r");
        fx.press(Key::Enter);
        fx.frame(vec![preedit("하")]);
        fx.frame(pass.clone());
        assert!(fx.up(), "{pass:?}: the committing Enter");
        fx.press(Key::Enter);
        assert!(!fx.up(), "{pass:?}: the next Enter keeps it");
        let kept = fx.row(0).message.clone();
        assert_eq!(kept, Some(format!("{FIFTH}한")), "{pass:?}");
    }
}

/// Shift+Enter is the message's new line and ends no turn: Enter, Shift+Enter, `x` and Enter in
/// one pass on a reword row keep the message with a new line and the `x`, the row a reword.
#[test]
fn x4p2_shift_enter_is_a_new_line_and_ends_no_turn() {
    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    fx.frame(vec![
        down(Key::Enter),
        key(Key::Enter, true, Modifiers::SHIFT),
        text("x"),
        down(Key::Enter),
    ]);
    assert!(!fx.up());
    assert_eq!(fx.row(0).message, Some(format!("{FIFTH}\nx")));
    assert_eq!(fx.row(0).verb, Verb::Reword);
}

/// Each Enter goes to what had the keyboard at its point of the pass: from the message, `[Tab,
/// Enter]` presses OK (it keeps) and `[Tab, Tab, Enter]` nothing (egui moves the keyboard once a
/// pass); in the question `[Tab, Tab, Enter]` likewise, and Enter once Keep planning has the
/// keyboard keeps planning.
#[test]
fn x4p2_enter_goes_to_what_has_the_keyboard() {
    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    fx.frame(vec![down(Key::Tab), down(Key::Enter)]);
    assert!(!fx.up(), "OK had it");
    assert_eq!(fx.row(0).message.as_deref(), Some(FIFTH));
    fx.press(Key::Enter);
    fx.frame(vec![down(Key::Tab), down(Key::Tab), down(Key::Enter)]);
    assert!(fx.up(), "an Enter egui cannot follow is dropped");
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    assert!(fx.up(), "the question");
    fx.frame(vec![down(Key::Tab), down(Key::Tab), down(Key::Enter)]);
    assert!(fx.up() && fx.host.ends.is_empty(), "dropped");
    let keep = fx.host.view.part_id("keep");
    if fx.focused() != Some(keep) {
        fx.press(Key::Tab);
    }
    assert_eq!(fx.focused(), Some(keep));
    fx.press(Key::Enter);
    assert!(!fx.up(), "Keep planning had it");
    assert!(fx.host.ends.is_empty(), "{:?}", fx.host.ends);
}

/// Esc's question loses the plan, so it closes the planner only once the screen showed it: Esc
/// and Enter in one pass close nothing; Enter after a pass that was not printed neither; the next
/// Enter closes.
#[test]
fn x4p2_the_question_closes_only_once_shown() {
    let mut fx = Fx::new(model(), &repo());
    fx.typed("d");
    fx.raw(vec![down(Key::Escape), down(Key::Enter)]);
    assert!(fx.up() && fx.host.ends.is_empty(), "{:?}", fx.host.ends);
    fx.raw(vec![down(Key::Enter)]);
    assert!(
        fx.host.ends.is_empty(),
        "its first pass was laid out unseen"
    );
    fx.idle(1);
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends, [PlannerEnd::Close]);

    let mut fx = Fx::new(model(), &repo());
    fx.typed("d");
    fx.raw(vec![down(Key::Escape)]);
    fx.raw(vec![]);
    cells::set_presented(&fx.drv.ctx, false);
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "not printed: {:?}", fx.host.ends);
    cells::set_presented(&fx.drv.ctx, true);
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends, [PlannerEnd::Close]);
}

/// Every field opens afresh — no selection, no undo, its caret at its end — whatever the last
/// dialog left under the same ids: Cmd/Ctrl+A in the author's name, Esc, `A` again and `x` typed
/// give `Adax`; `abc` typed in a message, Esc, Enter again and Cmd/Ctrl+Z keep it as it was; a
/// message dialog opened in the pass after the last closed, `x` typed in that very pass, is
/// typed at its end.
#[test]
fn x4p2_every_field_opens_afresh() {
    let all = testkit::command(false);
    let mut fx = Fx::new(model(), &repo());
    fx.typed("a");
    fx.drv.key(&mut fx.host, Key::A, all);
    fx.press(Key::Escape);
    fx.typed("a");
    fx.typed("x");
    fx.press(Key::Enter);
    let name = fx.row(0).author.as_ref().map(|a| a.name.clone());
    assert_eq!(name.as_deref(), Some("Adax"), "no selection kept");

    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    fx.typed("abc");
    fx.press(Key::Escape);
    fx.press(Key::Enter);
    fx.drv.key(&mut fx.host, Key::Z, all);
    fx.press(Key::Enter);
    assert_eq!(fx.row(0).message.as_deref(), Some(FIFTH), "nothing to undo");

    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    fx.press(Key::ArrowUp);
    fx.raw(vec![down(Key::Escape)]);
    fx.raw(vec![down(Key::Enter), text("x")]);
    fx.idle(1);
    fx.press(Key::Enter);
    let kept = fx.row(0).message.clone();
    assert_eq!(kept, Some(format!("{FIFTH}x")), "in the next pass");
}

/// A dialog's ids are its planner's own, not its cells': two planners never share them, nor does
/// a planner's run's confirmation share the quick edits' (the integration may open a quick edit's
/// confirmation while it is up); and a message field keeps its caret through a resize that moves
/// its box — `a` typed, the window made narrower, `b` typed after it.
#[test]
fn x4p2_ids_are_the_planners_own_and_survive_a_resize() {
    let one = PlannerView::new(model(), &repo());
    let two = PlannerView::new(model(), &repo());
    assert_ne!(one.part_id("message-field"), two.part_id("message-field"));
    assert_ne!(one.run_ids(), two.run_ids());
    assert_ne!(
        one.run_ids().run(),
        planner::quick_run_id(),
        "not a quick edit's"
    );
    assert_ne!(one.run_ids().cancel(), planner::quick_cancel_id());
    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    fx.typed("a");
    let before = boxed(&fx.drv, "its message").expect("the box").buf.area;
    fx.drv.resize(90.0 * CELL.x, 30.0 * CELL.y);
    fx.idle(2);
    let after = boxed(&fx.drv, "its message").expect("the box").buf.area;
    assert_ne!(before, after, "the box moved");
    fx.typed("b");
    fx.press(Key::Enter);
    assert_eq!(fx.row(0).message, Some(format!("{FIFTH}ab")));
}

// ---------------------------------------------------------------------------------------------
// Review round 1.

/// A planner dialog under another dialog — the panel's own result, another panel's (`active`
/// false) — is not what the screen shows: Esc's question, a pass under another panel's dialog,
/// then Enter closes nothing, since the last printed frame showed it covered; the next Enter, the
/// question shown in the pass before, closes the planner. The run's confirmation likewise runs
/// nothing after a covered pass, and the next Enter runs it.
#[test]
fn x4p2_a_covered_dialog_counts_as_unseen() {
    let mut view = super::tests::fixture_view();
    view.set_repo(repo());
    view.open_planner(model());
    let mut fx = super::tests::Fx::new(1000.0, 500.0, view);
    fx.idle();
    fx.typed("d");
    fx.press(Key::Escape);
    fx.idle();
    assert!(fx.view().dialog_up(), "the question");
    fx.view_mut().set_dialog_elsewhere(true);
    fx.drv.frame(&mut fx.host, vec![], Modifiers::NONE);
    fx.view_mut().set_dialog_elsewhere(false);
    fx.drv
        .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
    assert!(
        fx.view().planner_model().is_some(),
        "shown covered: it stays"
    );
    fx.drv
        .frame(&mut fx.host, vec![down(Key::Enter)], Modifiers::NONE);
    assert!(fx.view().planner_model().is_none(), "shown, it closes");

    let mut fx = Fx::new(model(), &repo());
    fx.run();
    fx.idle(1);
    fx.host.active = false;
    fx.raw(vec![]);
    fx.host.active = true;
    fx.raw(vec![down(Key::Enter)]);
    assert!(fx.host.ends.is_empty(), "shown covered: {:?}", fx.host.ends);
    fx.raw(vec![down(Key::Enter)]);
    assert_eq!(fx.host.ends, [PlannerEnd::Run(Edit::Plan(model()))]);
}

/// The panel in the terminal, the review's sequence: Esc's question up and a result over it (a
/// read's failure), then `[Enter, Enter]` in one read — the first closes the result in a pass the
/// terminal does not print, the second reaches the question, whose last printed frame was under
/// the result — closes nothing; the next Enter, once the question was printed, closes the planner.
#[test]
fn x4p2_an_enter_after_a_result_over_the_question_closes_nothing() {
    use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyModifiers};
    struct Panel {
        host: super::tests::Host,
        open: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl Window for Panel {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.host.ui(ui);
            let open = self.host.view.planner_model().is_some();
            self.open.set(usize::from(open));
        }
        fn ready(&self) -> bool {
            true
        }
    }
    // The question asked in a window's context, then the result over it.
    let mut view = super::tests::fixture_view();
    view.set_repo(repo());
    view.open_planner(model());
    let mut fx = super::tests::Fx::new(800.0, 480.0, view);
    fx.idle();
    fx.typed("d");
    fx.press(Key::Escape);
    assert!(fx.view().dialog_up(), "the question");
    fx.view_mut()
        .show_text("Fetch", "fatal: the remote hung up", None);
    let press = |code| CEvent::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let settle = || vec![CEvent::FocusGained];
    let bursts = vec![
        settle(),
        settle(),
        vec![press(KeyCode::Enter), press(KeyCode::Enter)],
        settle(),
        vec![press(KeyCode::Enter)],
    ];
    let open = std::rc::Rc::new(std::cell::Cell::new(1));
    let sent_at = std::rc::Rc::new(RefCell::new(Vec::new()));
    let tty = Bursts {
        bursts: bursts.into(),
        now: Default::default(),
        asked: 0,
        ends: open.clone(),
        sent_at: sent_at.clone(),
    };
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let mut b = term::Backend::new(tty, std::io::sink(), ctx, true);
    let mut win = Panel {
        host: super::tests::Host {
            view: std::mem::replace(&mut fx.host.view, super::tests::fixture_view()),
            active: true,
            actions: Vec::new(),
            rect: egui::Rect::NOTHING,
            under: Vec::new(),
            side: None,
        },
        open,
    };
    let mut no_switch =
        |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
    let end = b.run(&mut win, &mut no_switch).expect("the run");
    assert_eq!(end, term::End::Signal(libc::SIGTERM));
    assert_eq!(*sent_at.borrow(), [1; 5], "open until the last Enter");
    assert!(
        win.host.view.planner_model().is_none(),
        "the last Enter closed it"
    );
}

/// While another dialog has the keyboard the planner's dialog leaves the pass's typed text in the
/// input for it; with the keyboard the planner takes every typed character, none reaching the
/// git context's Find.
#[test]
fn x4p2_typed_text_is_left_for_the_dialog_with_the_keyboard() {
    let mut fx = Fx::new(model(), &repo());
    fx.typed("r");
    fx.press(Key::Enter);
    fx.host.left.clear();
    fx.host.active = false;
    fx.raw(vec![text("x")]);
    assert_eq!(fx.host.left, ["x"], "left for the other dialog");
    fx.host.active = true;
    fx.host.left.clear();
    fx.raw(vec![text("y")]);
    assert!(fx.host.left.is_empty(), "{:?}", fx.host.left);
    fx.idle(1);
    fx.press(Key::Enter);
    let kept = fx.row(0).message.clone();
    assert_eq!(kept, Some(format!("{FIFTH}y")), "y typed, x not");
}

/// `--update-refs` is offered from git 2.38 (03 §7.8.2): the run's confirmation over git 2.37
/// offers `--autostash` alone, over 2.38 both.
#[test]
fn x4p2_update_refs_is_offered_from_git_2_38() {
    for (minor, offered) in [(37, false), (38, true)] {
        let mut r = repo();
        r.version.minor = minor;
        r.version.text = format!("2.{minor}.0");
        let mut fx = Fx::new(model(), &r);
        fx.run();
        fx.idle(1);
        let all = fx.shown();
        assert!(
            all.contains("[ ] Stash my changes for it"),
            "2.{minor}: {all}"
        );
        assert_eq!(
            all.contains(planner::UPDATE_REFS),
            offered,
            "2.{minor}: {all}"
        );
    }
}
