//! F-git-ids' tests (`.prd/xtools/loop.md`): two git panels side by side in one `ui` — a pin
//! beside the other side's own repository (X4-menu) — each take their own clicks on the graph's
//! rows, its paging line and the planner's rows and buttons, whose ids are the panel's own; Tab
//! reaches none of them (they sense clicks alone); and no click nor double click's pair — the
//! graph's, the details pane's, the Backups list's, the log's, the banner's — crosses from one
//! panel to another drawn in its place.

use super::tests::fixture_view;
use super::tests_s4::{model, repo};
use super::*;
use crate::git::actions::Proceed;
use crate::gui::cells::Grid;
use crate::gui::testkit::Driver;
use crate::gui::Window;
use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect};

/// Two git panels in one `ui`, as the browser lays a pin beside another repository's panel, each
/// with what it asked for. `alone`: only that one, on the left side — drawn where the other stood
/// (the other side's panel replaced by another repository's).
struct Two {
    views: [GitView; 2],
    actives: [bool; 2],
    asked: [Vec<GitAction>; 2],
    alone: Option<usize>,
    grid: Grid,
}

impl Two {
    /// `views` side by side in a 1600×500 window, both with the keyboard, drawn twice.
    fn new(drv: &mut Driver, views: [GitView; 2]) -> Two {
        let mut two = Two {
            views,
            actives: [true, true],
            asked: [Vec::new(), Vec::new()],
            alone: None,
            grid: Grid::over(Rect::NOTHING),
        };
        drv.idle(&mut two, 2);
        two
    }

    /// What each panel has asked for since the last call.
    fn take(&mut self) -> [Vec<GitAction>; 2] {
        std::mem::take(&mut self.asked)
    }

    /// The centre of panel `k`'s graph row `row`, where the last pass drew it.
    fn row(&self, k: usize, row: usize) -> Pos2 {
        let g = self.views[k].geometry().expect("the graph drawn");
        self.grid
            .rect(g.row(row).expect("the row on screen"))
            .center()
    }

    /// Each panel's graph bar.
    fn bars(&self) -> [usize; 2] {
        self.views.each_ref().map(|v| v.bar_place().0)
    }

    /// Each planner's bar, by its place in the model.
    fn planner_bars(&self) -> [Option<usize>; 2] {
        self.views.each_ref().map(|v| {
            let p = v.planner.as_ref()?;
            let bar = p.bar()?;
            p.model().rows.iter().position(|r| &r.commit.row.oid == bar)
        })
    }

    /// Whether each panel has a dialog up.
    fn up(&self) -> [bool; 2] {
        self.views.each_ref().map(GitView::dialog_up)
    }
}

impl Window for Two {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                self.grid = Grid::over(ui.max_rect());
                let sides = cells::halves(self.grid.all());
                for k in 0..2 {
                    let side = match self.alone {
                        Some(only) if only != k => continue,
                        Some(_) => sides[0],
                        None => sides[k],
                    };
                    // One git dialog at a time, as the integration says before each pass.
                    let up = self.alone.is_none() && self.views[1 - k].dialog_up();
                    self.views[k].set_dialog_elsewhere(up);
                    let active = self.actives[k];
                    let asked = draw(ui, &self.grid, side, &mut self.views[k], active, &[]);
                    self.asked[k].extend(asked);
                }
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// The window's middle: the left side's cells are left of it.
const HALF: f32 = 800.0;

/// The centre of where the last pass drew a text starting with `text` on the left side (`left`)
/// or the right.
fn on(drv: &Driver, text: &str, left: bool) -> Pos2 {
    let found = drv.texts_starting_with(text);
    (found.iter())
        .find(|(r, _)| (r.center().x < HALF) == left)
        .unwrap_or_else(|| panic!("{text}, left {left}: {found:?}"))
        .0
        .center()
}

/// A press and a release of `key`, as one pass's events.
fn key_events(key: Key) -> Vec<Event> {
    [true, false]
        .map(|pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        })
        .to_vec()
}

/// The fixture's panel with a failed read on its paging line: a click there always reads again
/// (03 §4.5), and the bar coming near the last row asks nothing.
fn failing() -> GitView {
    let mut v = fixture_view();
    v.set_paging(Paging::Error("the read failed".into()));
    v
}

/// The fixture's panel with the planner open over it on S4's model.
fn planning() -> GitView {
    let mut v = fixture_view();
    v.set_repo(repo());
    v.open_planner(model());
    v
}

/// Whether `asked` holds a details request (a double click's).
fn details(asked: &[GitAction]) -> bool {
    asked.iter().any(|a| matches!(a, GitAction::Details(_)))
}

/// A click on graph row 3 of panel `k` puts that panel's bar there and leaves the other's.
fn graph_row_click(k: usize) {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [fixture_view(), fixture_view()]);
    two.actives = [k == 0, k == 1];
    assert_eq!(two.bars(), [0, 0], "the control");
    let p = two.row(k, 3);
    drv.click(&mut two, p);
    drv.idle(&mut two, 1);
    let mut wanted = [0, 0];
    wanted[k] = 3;
    assert_eq!(two.bars(), wanted, "panel {k}'s row alone");
}

/// The right panel's graph rows are its own: a click there moves its bar alone (with one id for
/// both, egui kept the right panel's rects for both, and a click on the right was a click in
/// both panels).
#[test]
fn f_git_ids_the_right_panels_graph_row_is_its_own() {
    graph_row_click(1);
}

/// The left panel's graph rows are its own (with one id for both they were dead: egui kept the
/// right panel's rects).
#[test]
fn f_git_ids_the_left_panels_graph_row_is_its_own() {
    graph_row_click(0);
}

/// A double click's pair does not cross panels. Side by side egui counts no double click across
/// the frames between them (more than its 6 points, `max_click_dist`), so the pair crosses only
/// where one panel is drawn where the other stood between the two clicks: a click on the left
/// panel's uncommitted row, then the other repository's panel in its place and a click on its
/// uncommitted row is a click of its own there — not a double click opening its details (one
/// `RowKey` for both, as every repository's uncommitted row has).
#[test]
fn f_git_ids_a_double_clicks_pair_does_not_cross_panels() {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [fixture_view(), fixture_view()]);
    two.alone = Some(0);
    drv.idle(&mut two, 2);
    let p = two.row(0, 0);
    // The control: the same two clicks in one panel are a double click.
    drv.click(&mut two, p);
    drv.idle(&mut two, 1);
    drv.click(&mut two, p);
    let [left, _] = two.take();
    assert!(details(&left), "the control: {left:?}");
    drv.idle(&mut two, 30);
    // The other panel's bar off the row, so its click is seen to land.
    two.views[1].move_bar(3);
    assert_eq!(two.views[1].bar_place().0, 3, "the other's bar elsewhere");
    drv.click(&mut two, p);
    two.alone = Some(1);
    drv.idle(&mut two, 1);
    assert_eq!(two.row(1, 0), p, "the other panel where the first stood");
    drv.click(&mut two, p);
    let [left, right] = two.take();
    assert!(!details(&left), "{left:?}");
    assert!(!details(&right), "a click of its own: {right:?}");
    assert_eq!(
        two.views[1].bar_place().0,
        0,
        "its bar on the row: the click landed"
    );
}

/// A click on panel `k`'s paging line reads more there alone (03 §4.5).
fn paging_line_click(k: usize) {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [failing(), failing()]);
    assert_eq!(two.take(), [vec![], vec![]], "the control: nothing asked");
    let v = &two.views[0];
    let (foot, _) = graph_view::paging_text(&v.paging, v.commits, "commits");
    drv.click(&mut two, on(&drv, &foot, k == 0));
    let mut wanted = [vec![], vec![]];
    wanted[k] = vec![GitAction::MoreRows];
    assert_eq!(two.take(), wanted, "panel {k}'s alone");
}

#[test]
fn f_git_ids_the_right_panels_paging_line_is_its_own() {
    paging_line_click(1);
}

#[test]
fn f_git_ids_the_left_panels_paging_line_is_its_own() {
    paging_line_click(0);
}

/// A click on the row `third` of panel `k`'s planner puts that planner's bar there alone.
fn planner_row_click(k: usize) {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [planning(), planning()]);
    assert_eq!(two.planner_bars(), [Some(0), Some(0)], "the control");
    drv.click(&mut two, on(&drv, "third", k == 0));
    drv.idle(&mut two, 1);
    let mut wanted = [Some(0), Some(0)];
    wanted[k] = Some(2);
    assert_eq!(two.planner_bars(), wanted, "panel {k}'s row alone");
}

#[test]
fn f_git_ids_the_right_planners_row_is_its_own() {
    planner_row_click(1);
}

#[test]
fn f_git_ids_the_left_planners_row_is_its_own() {
    planner_row_click(0);
}

/// A click on panel `k`'s `[ Run… ]` opens that planner's confirmation, not the other's.
fn planner_run_click(k: usize) {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [planning(), planning()]);
    assert_eq!(two.up(), [false, false], "the control");
    drv.click(&mut two, on(&drv, "[ Run… ]", k == 0));
    drv.idle(&mut two, 1);
    let mut wanted = [false, false];
    wanted[k] = true;
    assert_eq!(two.up(), wanted, "panel {k}'s confirmation alone");
}

/// The right planner's Run is its own (with one id for both, a click on it pressed the left
/// one's, which opened first and so kept the right one from opening: one dialog at a time).
#[test]
fn f_git_ids_the_right_planners_run_is_its_own() {
    planner_run_click(1);
}

#[test]
fn f_git_ids_the_left_planners_run_is_its_own() {
    planner_run_click(0);
}

/// A click on panel `k`'s `[ Close ]` closes that planner (nothing changed: no question) and
/// leaves the other open.
fn planner_close_click(k: usize) {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [planning(), planning()]);
    let open = |two: &Two| two.views.each_ref().map(|v| v.planner.is_some());
    assert_eq!(open(&two), [true, true], "the control");
    drv.click(&mut two, on(&drv, "[ Close ]", k == 0));
    drv.idle(&mut two, 1);
    let mut wanted = [true, true];
    wanted[k] = false;
    assert_eq!(open(&two), wanted, "panel {k}'s planner alone closed");
}

#[test]
fn f_git_ids_the_left_planners_close_is_its_own() {
    planner_close_click(0);
}

#[test]
fn f_git_ids_the_right_planners_close_is_its_own() {
    planner_close_click(1);
}

/// Tab, `then`, and the two in one pass, `n` times, the panels' parts drawn throughout: no part
/// takes egui's keyboard.
fn tabs_then(drv: &mut Driver, two: &mut Two, n: usize) {
    for then in [Key::Space, Key::Enter] {
        for _ in 0..n {
            drv.key(two, Key::Tab, Modifiers::NONE);
            assert_eq!(drv.ctx.memory(|m| m.focused()), None, "nothing has it");
            drv.key(two, then, Modifiers::NONE);
            let mut events = key_events(Key::Tab);
            events.extend(key_events(then));
            drv.frame(two, events, Modifiers::NONE);
            drv.idle(two, 1);
            assert_eq!(drv.ctx.memory(|m| m.focused()), None, "nor after {then:?}");
        }
    }
}

/// The graph's rows and its paging line sense clicks alone: Tab moves egui's keyboard to none of
/// them, so Tab then Space or Enter — each in its own pass, or both in one — presses none (egui
/// 0.37's `Sense::click()` is focusable, and a focused widget's Space or Enter is a click).
#[test]
fn f_git_ids_tab_reaches_no_graph_row_nor_paging_line() {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [failing(), failing()]);
    two.actives = [true, false];
    two.take();
    tabs_then(&mut drv, &mut two, 3);
    assert_eq!(two.take(), [vec![], vec![]], "nothing pressed");
    assert_eq!(two.bars(), [0, 0], "no bar moved");
}

/// The planner's rows and its `[ Run… ]` and `[ Close ]` sense clicks alone: its keys are its
/// own (Cmd/Ctrl+Enter runs, Esc closes, 03 §7.8.2) and Tab is the git panel's way back to the
/// list (03 §9), so Tab moves egui's keyboard to none of them, and Tab then Space or Enter opens
/// no confirmation, closes no planner and moves no bar.
#[test]
fn f_git_ids_tab_reaches_no_planner_row_nor_button() {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [planning(), planning()]);
    tabs_then(&mut drv, &mut two, 4);
    let open = two.views.each_ref().map(|v| v.planner.is_some());
    assert_eq!(open, [true, true], "no planner closed");
    assert_eq!(two.up(), [false, false], "no confirmation");
    assert_eq!(two.planner_bars(), [Some(0), Some(0)], "no bar moved");
}

// ---------------------------------------------------------------------------------------------
// Another repository's panel drawn where the first stood: the details pane's, the Backups list's,
// the log's and the banner's ids and double clicks' pairs are the panel's, not its column's.

/// Two panels made by `view`, each drawn alone on the left once (so each has painted its parts:
/// a guard acts on what was painted), the first last.
fn alone_on_the_left(drv: &mut Driver, view: fn() -> GitView) -> Two {
    let mut two = Two::new(drv, [view(), view()]);
    for k in [1, 0] {
        two.alone = Some(k);
        drv.idle(&mut two, 2);
    }
    two.take();
    two
}

/// The control — two clicks at `at` in the first panel are a double click there (`opened`:
/// whether panel `k` took one, given what it asked) — and then a click there, the other
/// repository's panel drawn in its place, and a click on its same line, which is a click of its
/// own: the pair is the panel's, not its column's. `landed`, when given, says the second click
/// put the other panel's bar on its line (it stood elsewhere before).
fn no_pair_across_a_swap(
    view: fn() -> GitView,
    at: &str,
    opened: fn(&Two, usize, &[GitAction]) -> bool,
    landed: Option<fn(&mut GitView) -> bool>,
) {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = alone_on_the_left(&mut drv, view);
    let p = on(&drv, at, true);
    drv.click(&mut two, p);
    drv.idle(&mut two, 1);
    drv.click(&mut two, p);
    let [left, _] = two.take();
    assert!(opened(&two, 0, &left), "the control: {left:?}");

    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = alone_on_the_left(&mut drv, view);
    drv.click(&mut two, p);
    two.alone = Some(1);
    drv.idle(&mut two, 1);
    assert_eq!(
        on(&drv, at, true),
        p,
        "the other panel's line where the first's stood"
    );
    drv.click(&mut two, p);
    let [left, right] = two.take();
    assert!(!opened(&two, 0, &left), "{left:?}");
    assert!(!opened(&two, 1, &right), "a click of its own: {right:?}");
    if let Some(landed) = landed {
        assert!(landed(&mut two.views[1]), "the click landed on its line");
    }
}

/// A press at `at` in the first panel and its release there with the other repository's panel
/// drawn in its place: egui gives the release to the id it pressed, so with one id for both the
/// other panel's control took the click. The control first: the press and the release in one
/// panel are its click. `took`: whether panel `k` took the click, given what it asked.
fn no_click_across_a_swap(
    view: fn() -> GitView,
    at: &str,
    took: fn(&mut Two, usize, &[GitAction]) -> bool,
) {
    for swap in [false, true] {
        let mut drv = Driver::new(1600.0, 500.0);
        let mut two = alone_on_the_left(&mut drv, view);
        let p = on(&drv, at, true);
        drv.move_to(&mut two, p);
        drv.press(&mut two, PointerButton::Primary);
        if swap {
            two.alone = Some(1);
        }
        drv.release(&mut two, PointerButton::Primary);
        drv.idle(&mut two, 2);
        let [left, right] = two.take();
        if swap {
            assert!(!took(&mut two, 1, &right), "the other panel's: {right:?}");
            assert!(!took(&mut two, 0, &left), "{left:?}");
        } else {
            assert!(took(&mut two, 0, &left), "the control: {left:?}");
        }
    }
}

/// The details of `subject`: six changed files, `f0.txt` to `f5.txt`.
fn six_files(subject: Subject) -> Details {
    let file = |k: usize| FileRow {
        letter: 'M',
        path: format!("f{k}.txt"),
        old_path: None,
        lines: None,
        lossy: false,
    };
    Details {
        subject,
        summary: None,
        contains: None,
        groups: vec![FileGroup {
            group: Group::NotStaged,
            files: (0..6).map(file).collect(),
        }],
        error: None,
    }
}

/// The fixture's panel with the details pane open on the uncommitted row — as Enter opens it, so
/// the pane is the panel's (`in_panel(find_key)` in `mod.rs`) — and six changed files, the
/// keyboard in the pane or not (following the bar): every repository's uncommitted row is one
/// subject.
fn pane_view(keyboard: bool) -> GitView {
    let mut v = fixture_view();
    let asked = v.details_of_bar();
    let subject = Subject::Row(RowKey::Uncommitted);
    assert_eq!(asked, [GitAction::Details(subject.clone())]);
    v.set_details(six_files(subject));
    v.pane.as_mut().expect("the pane").keyboard = keyboard;
    v
}

fn with_pane() -> GitView {
    pane_view(true)
}

/// Whether the pane's bar is on `f3.txt`'s line.
fn pane_on_f3(v: &mut GitView) -> bool {
    let Some(pane) = &v.pane else {
        return false;
    };
    match pane.line() {
        Some(details::Line::File { group, file }) => {
            (pane.file(*group, *file)).is_some_and(|(_, f)| f.path == "f3.txt")
        }
        _ => false,
    }
}

/// The details pane's double click (a file's diff) does not cross panels; the other pane's bar,
/// on its first line before, is on the clicked one after.
#[test]
fn f_git_ids_the_panes_pair_does_not_cross_panels() {
    let opened: fn(&Two, usize, &[GitAction]) -> bool = |_, _, asked| !asked.is_empty();
    let mut v = with_pane();
    assert!(!pane_on_f3(&mut v), "the bar elsewhere first");
    no_pair_across_a_swap(with_pane, "f3.txt", opened, Some(pane_on_f3));
}

/// A press on a line of the details pane and its release over the other panel's pane: no click
/// there (a click gives the pane the keyboard).
#[test]
fn f_git_ids_a_panes_press_is_not_the_other_panels_click() {
    no_click_across_a_swap(
        || pane_view(false),
        "f3.txt",
        |two, k, _| (two.views[k].pane.as_ref()).is_some_and(|p| p.keyboard),
    );
}

/// The pane's ids are its panel's, not its own: a press on a line of the pane while it follows the
/// bar, the bar moved before the release — the pane made again for the new row, with the same
/// files — and the release there is a click on the pane (it takes the keyboard). With a key of
/// each pane's own (no `in_panel` where `mod.rs` makes one) the release went to an id no longer
/// drawn and the click was lost.
#[test]
fn f_git_ids_a_panes_press_lands_across_its_remaking() {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = alone_on_the_left(&mut drv, || pane_view(false));
    drv.move_to(&mut two, on(&drv, "f3.txt", true));
    drv.press(&mut two, PointerButton::Primary);
    let v = &mut two.views[0];
    let asked = v.move_bar(1);
    let subject = v.pane.as_ref().expect("the pane").subject.clone();
    assert_ne!(subject, Subject::Row(RowKey::Uncommitted), "made again");
    assert_eq!(asked, Some(GitAction::Details(subject.clone())));
    v.set_details(six_files(subject));
    drv.release(&mut two, PointerButton::Primary);
    drv.idle(&mut two, 1);
    let pane = two.views[0].pane.as_ref().expect("the pane");
    assert!(pane.keyboard, "the click landed: the pane has the keyboard");
    assert!(pane_on_f3(&mut two.views[0]), "on the line pressed");
}

/// The fixture's panel with the Backups list open on S3's records.
fn with_backups() -> GitView {
    let mut v = fixture_view();
    v.set_repo(super::tests_s3::repo());
    v.open_backups(super::tests_s3::records());
    v
}

/// Whether `asked` holds the Backups list's Show.
fn shows_a_backup(asked: &[GitAction]) -> bool {
    asked.iter().any(|a| matches!(a, GitAction::ShowBackup(_)))
}

/// The Backups list's double click (Show) does not cross panels: on the first group's line, where
/// both bars stand — a double click Shows only the line the bar was painted on (`as_painted`), so
/// on another line the guard alone would hide a pair that crossed.
#[test]
fn f_git_ids_the_backups_pair_does_not_cross_panels() {
    no_pair_across_a_swap(
        with_backups,
        "sopen: reset main to aaaaaaaa",
        |_, _, asked| shows_a_backup(asked),
        None,
    );
}

/// A press on a line of the Backups list and its release over the other panel's list: no click
/// there (its bar stays on the first group). On a line, not a button: a button acts only as the
/// last pass on screen painted the bar's line (`as_painted`), which the other panel's list, not
/// drawn the pass before, never did — that guard alone would hide a click that crossed.
#[test]
fn f_git_ids_a_backups_press_is_not_the_other_panels_click() {
    no_click_across_a_swap(with_backups, "sopen: drop stash@{0}", |two, k, _| {
        let shows = two.views[k].backups.as_ref().and_then(|b| b.show());
        matches!(shows, Some(backups::BackupsEnd::Show(refs))
            if refs.iter().any(|r| r.kind == crate::git::model::BackupKind::Stash))
    });
}

/// The fixture's panel with the command log open on three entries, one of them failed.
fn with_log() -> GitView {
    use crate::git::runner::Ending;
    let mut v = fixture_view();
    let entry = super::tests_s3::entry;
    v.open_log(vec![
        entry("git fetch --progress origin b0", Ending::Exit(0), "", 3),
        entry(
            "git fetch --progress origin b1",
            Ending::Exit(128),
            "fatal",
            2,
        ),
        entry("git fetch --progress origin b2", Ending::Exit(0), "", 1),
    ]);
    v
}

/// Whether the log's bar is on the oldest entry, `b2` (it starts on the newest): Enter's
/// `Details` shows the bar's entry whole.
fn log_on_b2(v: &mut GitView) -> bool {
    let Some(log) = v.log.as_mut() else {
        return false;
    };
    if log.whole().is_none() {
        log.run(Command::Details);
    }
    log.whole().is_some_and(|e| e.line.ends_with("b2"))
}

/// The log's double click (the entry shown whole) does not cross panels; the other log's bar,
/// on the newest entry before, is on the clicked one after.
#[test]
fn f_git_ids_the_logs_pair_does_not_cross_panels() {
    no_pair_across_a_swap(
        with_log,
        "git fetch --progress origin b2",
        |two, k, _| (two.views[k].log.as_ref()).is_some_and(|l| l.whole().is_some()),
        Some(log_on_b2),
    );
}

/// A press on a row of the log and its release over the other panel's log: no click there (its
/// bar stays on the newest entry).
#[test]
fn f_git_ids_a_logs_press_is_not_the_other_panels_click() {
    no_click_across_a_swap(with_log, "git fetch --progress origin b2", |two, k, _| {
        log_on_b2(&mut two.views[k])
    });
}

/// The fixture's panel with S3's repository and a merge's banner offering Continue and Abort.
fn with_banner() -> GitView {
    let mut v = fixture_view();
    v.set_repo(super::tests_s3::repo());
    v.banner = Some(in_progress::BannerView::new(
        "MERGE of feature · 1 conflict",
        vec![Proceed::Continue, Proceed::Abort],
    ));
    v
}

/// A press on the banner's `[ Abort ]` and its release over the other panel's: no Abort there
/// (its confirmation opens on a click).
#[test]
fn f_git_ids_a_banners_press_is_not_the_other_panels_click() {
    no_click_across_a_swap(with_banner, "[ Abort ]", |two, k, asked| {
        !asked.is_empty() || two.views[k].dialog_up()
    });
}

/// The log's entry shown whole is the panel's own dialog, whatever its column: with the keyboard
/// on its `[ OK ]`, a width change — which moves the right side's column — keeps it there.
#[test]
fn f_git_ids_the_logs_entry_keeps_its_focus_across_a_width_change() {
    let mut drv = Driver::new(1600.0, 500.0);
    let mut two = Two::new(&mut drv, [fixture_view(), with_log()]);
    two.actives = [false, true];
    two.views[1]
        .log
        .as_mut()
        .expect("the log")
        .run(Command::Details);
    drv.idle(&mut two, 2);
    drv.key(&mut two, Key::Tab, Modifiers::NONE);
    let focused = drv.ctx.memory(|m| m.focused());
    assert!(focused.is_some(), "OK has the keyboard");
    let at = cells::halves(two.grid.all())[1].x;
    drv.resize(1400.0, 500.0);
    drv.idle(&mut two, 2);
    assert_ne!(cells::halves(two.grid.all())[1].x, at, "the column moved");
    let whole = two.views[1].log.as_ref().and_then(|l| l.whole());
    assert!(whole.is_some(), "the entry still shown whole");
    assert_eq!(drv.ctx.memory(|m| m.focused()), focused, "still its OK's");
}
