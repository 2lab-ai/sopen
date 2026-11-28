//! X4-menu's tests (10 §17): the object's menu and the filters in cells (03 §4.6, §7, §9),
//! through `ops::dialog` and the kit's list and choices.

use super::tests::{fixture_view, label, Fx};
use super::*;
use crate::git::actions::{Action, ActionId};
use crate::gui::browser::ops::dialog as kit;
use crate::gui::cells::Layer;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit::Driver;
use eframe::egui::{Event, Modifiers, PointerButton, Pos2};
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Driving and reading.

/// A press and a release of `key` with `mods`, as one pass's events.
fn key_events(key: Key, mods: Modifiers) -> Vec<Event> {
    [true, false]
        .map(|pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: mods,
        })
        .to_vec()
}

/// A press and a release of `button` at `p`, as one pass's events.
fn click_events(p: Pos2, button: PointerButton) -> Vec<Event> {
    let at = |pressed| Event::PointerButton {
        pos: p,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(p), at(true), at(false)]
}

/// One pass of the panel with `events` and no pass after it: what it asked for.
fn pass(fx: &mut Fx, events: Vec<Event>) -> Vec<GitAction> {
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.take()
}

/// The panel's grid, which its dialogs stand on.
fn grid_of(fx: &Fx) -> Grid {
    Grid::over(fx.host.rect)
}

/// The fixture's panel with the S3 repository behind it (so it has a menu), in a 1000×600 window.
fn panel() -> Fx {
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.view_mut().set_repo(super::tests_s3::repo());
    fx.idle();
    fx
}

/// The layer of cells over exactly `covers` (a dialog's box and its shadow).
fn layer_over(drv: &Driver, covers: &[Cells]) -> Arc<Layer> {
    let area = covers.iter().fold(covers[0], |a, c| a.union(*c));
    cells::layers(&drv.ctx)
        .into_iter()
        .rev()
        .find(|l| l.buf.area == area)
        .unwrap_or_else(|| panic!("no layer of cells over {area:?}"))
}

/// The open menu's box, as it covers the panels' grid: the frame first.
fn menu_covers(fx: &Fx) -> Vec<Cells> {
    let menu = fx.view().menu.as_ref().expect("the menu is open");
    menu.covers(&grid_of(fx))
}

/// The open filters' box.
fn filters_covers(fx: &Fx) -> Vec<Cells> {
    let d = fx
        .view()
        .filters_dialog
        .as_ref()
        .expect("the filters are open");
    d.covers(&grid_of(fx))
}

/// The rows of `frame` in `l`, as text.
fn rows_of(l: &Layer, frame: Cells) -> Vec<String> {
    (frame.top()..frame.bottom())
        .map(|y| {
            let (text, at) = cells::row_text(&l.buf, y);
            let inside = (text.chars().zip(at)).filter(|&(_, x)| frame.contains_x(x));
            inside.map(|(c, _)| c).collect()
        })
        .collect()
}

trait ContainsX {
    fn contains_x(&self, x: u16) -> bool;
}

impl ContainsX for Cells {
    fn contains_x(&self, x: u16) -> bool {
        (self.left()..self.right()).contains(&x)
    }
}

/// The open menu's rows inside its frame, the frame's lines included.
fn menu_rows(fx: &Fx) -> Vec<String> {
    let covers = menu_covers(fx);
    rows_of(&layer_over(&fx.drv, &covers), covers[0])
}

fn filters_rows(fx: &Fx) -> Vec<String> {
    let covers = filters_covers(fx);
    rows_of(&layer_over(&fx.drv, &covers), covers[0])
}

/// The row of `rows` that holds `text`.
fn row_with<'a>(rows: &'a [String], text: &str) -> &'a str {
    (rows.iter())
        .find(|r| r.contains(text))
        .unwrap_or_else(|| panic!("no row holds {text:?}: {rows:#?}"))
}

/// What a pass that chose a menu item asked for, as the item's id: the dialog it opened, or the
/// read or the run it asked the integration for.
fn chose(fx: &Fx, asked: &[GitAction]) -> Option<ActionId> {
    let asked = asked.iter().find_map(|a| match a {
        GitAction::Open { id, .. } => Some(*id),
        GitAction::Run(action) => Some(action.id()),
        _ => None,
    });
    asked.or_else(|| fx.view().draft().map(Action::id))
}

/// The point at the centre of row `y` of the panel's grid, column `x`.
fn centre(fx: &Fx, x: u16, y: u16) -> Pos2 {
    grid_of(fx).rect(Cells::new(x, y, 1, 1)).center()
}

/// Where the cells of `l` show `text` first, on screen.
fn place_of(l: &Layer, text: &str) -> Pos2 {
    let hit = cells::find(std::slice::from_ref(&Arc::new(l.clone())), text);
    let hit = hit
        .first()
        .unwrap_or_else(|| panic!("{text:?} is not in the box"));
    l.rect(hit.cells).center()
}

// ---------------------------------------------------------------------------------------------
// The menu (03 §7, §9: F10, Shift+F10, a right click).

/// F10 in the git panel: the object's menu is a box of cells on the panels' grid — the frame
/// titled with the commit, each item a row with its key at the right, the bar on the first — as
/// wide as its widest row by 10 §6's rule (24 cells at least).
#[test]
fn f10_opens_the_menu_in_cells_with_its_keys_at_the_right() {
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    let rows = menu_rows(&fx);
    assert!(
        rows[0].starts_with('╔') && rows[0].contains(" 09090909 Merge branch 'feature' "),
        "{rows:#?}"
    );
    // A blank, the label, the key at the right, a blank, inside the frame's margin.
    assert!(row_with(&rows, "Cherry-pick…").starts_with("║  Cherry-pick…"));
    assert!(row_with(&rows, "Cherry-pick…").ends_with(" F5  ║"));
    assert!(row_with(&rows, "Rebase the current").ends_with(" F6  ║"));
    assert!(row_with(&rows, "New branch…").ends_with(" F7  ║"));
    let copy = row_with(&rows, "Copy hash");
    assert!(
        copy.trim_end_matches('║').trim_end().ends_with("Copy hash"),
        "no key: {copy:?}"
    );
    assert_eq!(rows.last().map(|r| r.chars().next()), Some(Some('╚')));
    // The widest row: "Rebase the current branch onto this…" and F6, a blank either end, two
    // between — 42 cells inside, the frame 46.
    let frame = menu_covers(&fx)[0];
    assert_eq!(frame.width, 42 + 4, "{rows:#?}");
    // The bar on the first item, in the bar's colours.
    let covers = menu_covers(&fx);
    let l = layer_over(&fx.drv, &covers);
    let first = place_of(&l, "Tag…");
    let at = grid_of(&fx).at(first).expect("on the grid");
    let seen = &l.seen()[usize::from(at.y - l.buf.area.y)][usize::from(at.x - l.buf.area.x)];
    assert_eq!(seen.bg, Some(PALETTE.bar), "the bar on Tag…");
}

/// Shift+F10 and a right click on the row open the same menu as F10, cell for cell.
#[test]
fn shift_f10_and_the_right_click_open_the_same_menu() {
    let opened = |open: &dyn Fn(&mut Fx)| {
        let mut fx = panel();
        fx.press(Key::ArrowDown);
        open(&mut fx);
        fx.idle();
        menu_rows(&fx)
    };
    let f10 = opened(&|fx| {
        fx.press(Key::F10);
    });
    let shift = opened(&|fx| {
        fx.key(Key::F10, Modifiers::SHIFT);
    });
    let right = opened(&|fx| {
        let g = fx.view().geometry().expect("the graph drawn");
        let y = g.row(1).expect("row 1 drawn").y;
        let p = centre(fx, 60, y);
        pass(fx, click_events(p, PointerButton::Secondary));
    });
    assert!(f10.iter().any(|r| r.contains("Cherry-pick…")), "{f10:#?}");
    assert_eq!(shift, f10);
    assert_eq!(right, f10);
}

/// F10 and Enter in one pass: the menu opens and Enter chooses nothing, its items unseen; the
/// next Enter (the menu painted once) chooses the bar's item.
#[test]
fn f10_and_enter_in_one_pass_choose_nothing_unseen() {
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    let mut events = key_events(Key::F10, Modifiers::NONE);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let asked = pass(&mut fx, events);
    assert_eq!(chose(&fx, &asked), None, "{asked:?}");
    assert!(fx.view().menu.is_some(), "the menu is open");
    assert!(
        !fx.view().details_open(),
        "the Enter reached nothing under it"
    );
    let asked = fx.press(Key::Enter);
    assert!(fx.view().menu.is_none());
    assert_eq!(chose(&fx, &asked), Some(ActionId::Tag), "{asked:?}");
}

/// F-git-covered: a menu under another dialog that has the keyboard — another panel's, the list's
/// question, F12's (`set_dialog_elsewhere`) — is not what the screen shows: a printed pass under
/// one, then Enter in the pass after it chooses nothing; the next Enter, the menu printed with the
/// keyboard in the pass before, chooses.
#[test]
fn f_git_covered_a_menu_under_another_dialog_chooses_nothing() {
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    assert!(fx.view().menu.is_some(), "the menu is open");
    fx.view_mut().set_dialog_elsewhere(true);
    let asked = pass(&mut fx, vec![]);
    assert_eq!(chose(&fx, &asked), None, "{asked:?}");
    fx.view_mut().set_dialog_elsewhere(false);
    let asked = pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    assert_eq!(chose(&fx, &asked), None, "shown covered: {asked:?}");
    assert!(fx.view().menu.is_some(), "the menu stays");
    let asked = pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    assert!(fx.view().menu.is_none());
    assert_eq!(chose(&fx, &asked), Some(ActionId::Tag), "{asked:?}");
}

/// The menu's keys one at a time, in order, several in one pass: two ↓ and Enter choose the third
/// item.
#[test]
fn several_presses_in_one_pass_go_in_order() {
    let r = super::tests_s3::repo();
    let target = Target::Label(label("refs/heads/feature", 2, None));
    let mut fx = super::tests_s3::with_menu(target.clone(), &r);
    let mut events = key_events(Key::ArrowDown, Modifiers::NONE);
    events.extend(key_events(Key::ArrowDown, Modifiers::NONE));
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(
        fx.host.menu_ends,
        [menu::MenuEnd::Chose {
            id: ActionId::DeleteBranch,
            target
        }]
    );
}

/// `Repository…` turns the menu into the repository's: an Enter after it in the same pass is of
/// lines never shown, and chooses nothing; the next Enter chooses the bar's line there.
#[test]
fn an_enter_after_repository_in_one_pass_chooses_nothing() {
    let r = super::tests_s3::repo();
    let mut fx = super::tests_s3::with_menu(Target::Uncommitted, &r);
    let mut events = key_events(Key::End, Modifiers::NONE);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert!(fx.host.menu_ends.is_empty(), "{:?}", fx.host.menu_ends);
    fx.idle();
    fx.assert_shows("Fetch all remotes…");
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.menu_ends,
        [menu::MenuEnd::Chose {
            id: ActionId::Fetch,
            target: Target::Repository
        }]
    );
}

/// An Enter after a Shift+Tab in the same pass is dropped (egui moves the keyboard after the
/// pass), and reaches nothing under the menu; the next Enter chooses.
#[test]
fn an_enter_after_a_shift_tab_in_one_pass_is_dropped() {
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    let mut events = key_events(Key::Tab, Modifiers::SHIFT);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let asked = pass(&mut fx, events);
    assert_eq!(chose(&fx, &asked), None, "{asked:?}");
    assert!(fx.view().menu.is_some(), "the menu stays");
    assert!(!fx.view().details_open(), "nothing under it took the Enter");
    fx.idle();
    let asked = fx.press(Key::Enter);
    assert_eq!(chose(&fx, &asked), Some(ActionId::Tag), "{asked:?}");
}

/// An item's key at the right still works: a click on it chooses its item, as on its label; it
/// is that item's key in the panel once the menu has closed; while the menu is open it does
/// nothing (as egui's menu did).
#[test]
fn an_items_key_at_the_right_still_works() {
    let r = super::tests_s3::repo();
    let target = Target::Label(label("refs/heads/feature", 2, None));
    for (key, id) in [("F8", ActionId::DeleteBranch), ("F2", ActionId::Rename)] {
        let mut fx = super::tests_s3::with_menu(target.clone(), &r);
        fx.click(key);
        assert_eq!(
            fx.host.menu_ends,
            [menu::MenuEnd::Chose {
                id,
                target: target.clone()
            }],
            "a click on {key}"
        );
    }
    // On the uncommitted row: Commit… F7.
    let mut fx = panel();
    fx.press(Key::F10);
    assert!(row_with(&menu_rows(&fx), "Commit…").ends_with(" F7  ║"));
    let asked = fx.press(Key::F7);
    assert_eq!(chose(&fx, &asked), None, "{asked:?}");
    assert!(
        fx.view().menu.is_some(),
        "F7 does nothing while the menu is open"
    );
    fx.press(Key::Escape);
    assert!(fx.view().menu.is_none());
    let asked = fx.press(Key::F7);
    assert_eq!(chose(&fx, &asked), Some(ActionId::Commit), "{asked:?}");
}

/// On a narrow grid the box is the grid less eight cells: a label is cut to leave its key whole
/// at the right, and the cut line is still its item.
#[test]
fn a_narrow_menu_cuts_the_label_and_keeps_its_key() {
    let r = super::tests_s3::repo();
    let mut fx = super::tests_s3::Fx {
        drv: Driver::new(320.0, 600.0),
        host: super::tests_s3::Host {
            menu: Some(menu::Menu::new(Target::Repository, &r)),
            ..super::tests_s3::Host::default()
        },
    };
    fx.idle();
    fx.idle();
    let menu = fx.host.menu.as_ref().expect("open");
    let grid = kit::grid(&fx.drv.ctx);
    assert_eq!(grid.cols, 40);
    let covers = menu.covers(&grid);
    assert_eq!(covers[0].width, 32 + 4, "the grid less eight");
    let rows = rows_of(&layer_over(&fx.drv, &covers), covers[0]);
    let fetch = row_with(&rows, "Ctrl/⌘+Shift+R");
    assert_eq!(fetch, "║  Fetch all rem…  Ctrl/⌘+Shift+R  ║", "{rows:#?}");
    fx.press(Key::Enter);
    assert_eq!(
        fx.host.menu_ends,
        [menu::MenuEnd::Chose {
            id: ActionId::Fetch,
            target: Target::Repository
        }]
    );
}

/// A box laid elsewhere than the pass before painted it (the window resized) takes no choice in
/// that pass: the click was aimed at the line on screen, and the list would find another under it
/// now. The next click on its new place chooses.
#[test]
fn a_box_laid_elsewhere_takes_no_choice_in_that_pass() {
    let r = super::tests_s3::repo();
    let target = Target::Label(label("refs/heads/feature", 2, None));
    let mut fx = super::tests_s3::with_menu(target.clone(), &r);
    let menu = fx.host.menu.as_ref().expect("open");
    let covers = menu.covers(&kit::grid(&fx.drv.ctx));
    let old = place_of(&layer_over(&fx.drv, &covers), "Delete…");
    // Shorter: the box re-centred three rows up, `Copy name` where `Delete…` stood.
    fx.drv.resize(1200.0, 700.0);
    fx.drv.frame(
        &mut fx.host,
        click_events(old, PointerButton::Primary),
        Modifiers::NONE,
    );
    fx.idle();
    assert!(fx.host.menu_ends.is_empty(), "{:?}", fx.host.menu_ends);
    fx.click("Delete…");
    assert_eq!(
        fx.host.menu_ends,
        [menu::MenuEnd::Chose {
            id: ActionId::DeleteBranch,
            target
        }]
    );
}

/// Space chooses nothing in the menu (egui's chose on Enter or a click; in the panel Space
/// marks, 03 §9.2), and reaches nothing under it; Enter chooses.
#[test]
fn space_chooses_nothing_in_the_menu() {
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    let mut events = key_events(Key::Space, Modifiers::NONE);
    events.insert(1, Event::Text(" ".into()));
    let asked = pass(&mut fx, events);
    fx.idle();
    assert_eq!(chose(&fx, &asked), None, "{asked:?}");
    assert!(fx.view().menu.is_some(), "the menu stays");
    assert!(fx.view().marks.is_empty(), "no row marked under it");
    let asked = fx.press(Key::Enter);
    assert_eq!(chose(&fx, &asked), Some(ActionId::Tag), "{asked:?}");
}

/// A menu with no item says why, wrapped at 40 cells as egui's was, and still offers
/// `Repository…`.
#[test]
fn a_menu_with_no_item_says_why_in_egui_s_width() {
    let r = super::tests_s3::repo();
    let mut lossy = label("refs/heads/b\u{fffd}d", 2, None);
    lossy.lossy = true;
    let fx = super::tests_s3::with_menu(Target::Label(lossy), &r);
    let menu = fx.host.menu.as_ref().expect("open");
    let covers = menu.covers(&kit::grid(&fx.drv.ctx));
    let rows = rows_of(&layer_over(&fx.drv, &covers), covers[0]);
    assert!(
        rows[2].starts_with("║ nothing to do here: a name that is not "),
        "{rows:#?}"
    );
    assert!(
        rows[3].starts_with("║ valid UTF-8 names nothing "),
        "{rows:#?}"
    );
    row_with(&rows, "  Repository…");
    assert_eq!(covers[0].width, 38 + 4, "its widest row, 38 cells");
}

/// On a short grid the list shows seven tenths of its rows (12 of 18) and scrolls with the bar,
/// ↑ ░ ↓ on the box's border; End shows `Repository…`, and a click on a line scrolled into view
/// chooses that line.
#[test]
fn a_short_grid_scrolls_the_lines_with_the_bar() {
    let r = super::tests_s3::repo();
    let target = Target::Commit {
        commit: super::tests::commit(5, &[1], "Ann", 0, "five"),
        marked: Vec::new(),
    };
    let mut fx = super::tests_s3::Fx {
        drv: Driver::new(1200.0, 18.0 * 16.0),
        host: super::tests_s3::Host {
            menu: Some(menu::Menu::new(target.clone(), &r)),
            ..super::tests_s3::Host::default()
        },
    };
    fx.idle();
    fx.idle();
    let grid = kit::grid(&fx.drv.ctx);
    assert_eq!(grid.rows, 18);
    let covers = fx.host.menu.as_ref().expect("open").covers(&grid);
    assert_eq!(covers[0].height, 12 + 4, "twelve lines of fourteen");
    let rows = rows_of(&layer_over(&fx.drv, &covers), covers[0]);
    assert!(
        rows[2].contains("Tag…") && rows[2].ends_with('↑'),
        "{rows:#?}"
    );
    assert!(rows[13].ends_with('↓'), "{rows:#?}");
    assert!(!rows.iter().any(|r| r.contains("Repository…")), "{rows:#?}");
    fx.press(Key::End);
    let rows = rows_of(&layer_over(&fx.drv, &covers), covers[0]);
    assert!(rows[13].contains("Repository…"), "{rows:#?}");
    assert!(
        !rows.iter().any(|r| r.contains("Tag…")),
        "scrolled: {rows:#?}"
    );
    fx.click("Move tag here…");
    assert_eq!(
        fx.host.menu_ends,
        [menu::MenuEnd::Chose {
            id: ActionId::MoveTag,
            target
        }]
    );
}

/// The panel blanks the cells the menu covers (its box and its shadow), as the menu answers.
#[test]
fn the_menu_covers_its_box_and_the_panel_blanks_it() {
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    let covers = menu_covers(&fx);
    assert_eq!(covers.len(), 3, "the box and its shadow's two strips");
    let [frame, right, under] = [covers[0], covers[1], covers[2]];
    assert_eq!(
        right,
        Cells::new(frame.right(), frame.y + 1, 2, frame.height)
    );
    assert_eq!(
        under,
        Cells::new(frame.x + 2, frame.bottom(), frame.width, 1)
    );
    let panel = super::tests_x4_graph::panel(&fx);
    let seen = panel.seen();
    for c in covers {
        for y in c.top()..c.bottom() {
            for x in c.left()..c.right() {
                let s = &seen[usize::from(y)][usize::from(x)];
                assert_eq!(s.symbol.trim(), "", "({x}, {y}) under the menu");
            }
        }
    }
}

/// A commit's title is its subject cut at 40 cells: a Hangul one at 19 characters and `…`.
#[test]
fn a_commits_menu_title_is_cut_by_cells() {
    let r = super::tests_s3::repo();
    let subject: String = "가나다라마바사아자차카타파하".repeat(3);
    let target = Target::Commit {
        commit: super::tests::commit(5, &[1], "Ann", 0, &subject),
        marked: Vec::new(),
    };
    let fx = super::tests_s3::with_menu(target, &r);
    let menu = fx.host.menu.as_ref().expect("open");
    let covers = menu.covers(&kit::grid(&fx.drv.ctx));
    let rows = rows_of(&layer_over(&fx.drv, &covers), covers[0]);
    let cut: String = subject.chars().take(19).collect();
    assert!(
        rows[0].contains(&format!(" 05050505 {cut}… ")),
        "{:?}",
        rows[0]
    );
}

// ---------------------------------------------------------------------------------------------
// The filters (03 §4.6: Cmd/Ctrl+K).

/// The fixture's view with two Hangul branches more.
fn hangul_view() -> GitView {
    let mut view = fixture_view();
    let mut refs = super::tests::refs();
    refs.insert(0, label("refs/heads/기능", 8, None));
    refs.insert(1, label("refs/heads/기록", 8, None));
    view.set_refs(refs, HeadRef::Branch("refs/heads/main".into()));
    view
}

fn filters_open(view: GitView) -> Fx {
    let mut fx = Fx::new(1000.0, 600.0, view);
    fx.key(Key::K, Modifiers::COMMAND);
    assert!(
        fx.view().filters_dialog.is_some(),
        "Cmd/Ctrl+K opened the filters"
    );
    fx
}

/// `filters`' id of a control of the dialog, as `filters.rs` names it.
fn filters_id(part: &str) -> egui::Id {
    egui::Id::unique("sopen-git-filters").with(part)
}

/// Cmd/Ctrl+K: the filters in cells — the branches' choice, the type-to-filter field, the
/// branches as the kit's list with the bar on the first, the remotes, the filters that are on or
/// off, the order, the buttons and the hint.
#[test]
fn cmd_k_opens_the_filters_in_cells() {
    let fx = filters_open(fixture_view());
    let rows = filters_rows(&fx);
    assert!(rows[0].contains(" Filters "), "{rows:#?}");
    for text in [
        "Branches [X] All branches",
        "Type to filter",
        "[X] feature",
        "[X] main",
        "[X] origin/feature",
        "[X] up/main",
        "Remotes [X] origin  [X] up",
        "[X] Tags  [X] Stashes  [ ] First parent only  [ ] Reflog commits",
        "Order (•) date  ( ) author date  ( ) topological",
        "[ Apply ]  [ Cancel ]",
        "Space turns on or off · Enter applies · Esc cancels",
    ] {
        row_with(&rows, text);
    }
    assert!(
        !rows.iter().any(|r| r.contains("origin/HEAD")),
        "a remote's own HEAD"
    );
    let covers = filters_covers(&fx);
    let l = layer_over(&fx.drv, &covers);
    let at = grid_of(&fx)
        .at(place_of(&l, "[X] feature"))
        .expect("on the grid");
    let s = &l.seen()[usize::from(at.y - l.buf.area.y)][usize::from(at.x - l.buf.area.x)];
    assert_eq!(s.bg, Some(PALETTE.bar), "the bar on the first branch");
    // The panel blanks what the box covers.
    let panel = super::tests_x4_graph::panel(&fx);
    let frame = covers[0];
    let s = &panel.seen()[usize::from(frame.y + 1)][usize::from(frame.x + 1)];
    assert_eq!(s.symbol.trim(), "");
}

/// Type-to-filter takes several letters in one pass, in order, Hangul too — also in the pass
/// Cmd/Ctrl+K opens the dialog in, after the key.
#[test]
fn typed_letters_narrow_the_branches_in_order_in_one_pass() {
    // Opened and typed into in one pass.
    let mut fx = Fx::new(1000.0, 600.0, hangul_view());
    let mut events = key_events(Key::K, Modifiers::COMMAND);
    events.extend([Event::Text("기".into()), Event::Text("능".into())]);
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert!(fx.view().filters_dialog.is_some());
    fx.idle();
    let rows = filters_rows(&fx);
    row_with(&rows, "Type to filter 기능");
    row_with(&rows, "[X] 기능");
    assert!(
        !rows
            .iter()
            .any(|r| r.contains("기록") || r.contains("feature")),
        "{rows:#?}"
    );
    // Typed into once open: three letters in one pass.
    let mut fx = filters_open(hangul_view());
    let events = ["f", "e", "a"].map(|t| Event::Text(t.into())).to_vec();
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    let rows = filters_rows(&fx);
    row_with(&rows, "Type to filter fea");
    row_with(&rows, "[X] feature");
    row_with(&rows, "[X] origin/feature");
    assert!(
        !rows.iter().any(|r| r.contains("main") && r.contains("[X]")),
        "{rows:#?}"
    );
    // And Hangul after them, one pass: nothing is left.
    fx.drv.frame(
        &mut fx.host,
        vec![Event::Text("기".into())],
        Modifiers::NONE,
    );
    fx.idle();
    row_with(&filters_rows(&fx), "Type to filter fea기");
}

/// The fixture's view with ten branches more, `fx-00` to `fx-09`, after its own.
fn many_view() -> GitView {
    let mut view = fixture_view();
    let mut refs = super::tests::refs();
    refs.extend((0..10).map(|k| label(&format!("refs/heads/fx-{k:02}"), 8, None)));
    view.set_refs(refs, HeadRef::Branch("refs/heads/main".into()));
    view
}

/// What Enter applies now, the dialog's filters as they stand.
fn applied(fx: &mut Fx) -> Filters {
    match &fx.press(Key::Enter)[..] {
        [GitAction::FiltersChanged(f)] => f.clone(),
        other => panic!("{other:?}"),
    }
}

/// Type-to-filter, then a click on a branch in the very next pass: the pass that typed painted
/// the rows it held before the text (`feature` first), so the click — the text has put `fx-00`
/// there, in a box of the same size — turns nothing; once the new rows are painted a click there
/// turns `fx-00`. (In the typing pass itself a click takes the keyboard from the field before it
/// types, egui's rule.)
#[test]
fn a_click_right_after_typing_turns_nothing_unseen() {
    let mut fx = filters_open(many_view());
    let before = filters_covers(&fx);
    let p = place_of(&layer_over(&fx.drv, &before), "[X] feature");
    pass(&mut fx, vec![Event::Text("fx".into())]);
    // That pass painted the box it laid out before the text: the rows it held, the text typed.
    let rows = rows_of(&layer_over(&fx.drv, &before), before[0]);
    row_with(&rows, "Type to filter fx");
    row_with(&rows, "[X] feature");
    pass(&mut fx, click_events(p, PointerButton::Primary));
    fx.idle();
    assert_eq!(filters_covers(&fx), before, "the box stands where it stood");
    let rows = filters_rows(&fx);
    row_with(&rows, "[X] fx-00");
    assert!(!rows.iter().any(|r| r.contains("[ ] fx-00")), "{rows:#?}");
    // The new rows painted: a click there turns `fx-00`.
    fx.drv.click(&mut fx.host, p);
    row_with(&filters_rows(&fx), "[ ] fx-00");
    let f = applied(&mut fx);
    assert!(
        matches!(&f.branches, BranchChoice::Only(c) if c.len() == 14 && !c.contains("refs/heads/fx-00"))
    );
    assert_eq!(
        f,
        Filters {
            branches: f.branches.clone(),
            ..Filters::default()
        }
    );
}

/// A box laid elsewhere than the pass before painted it takes no click on the list in that pass.
/// Typing `up` leaves one branch and the box is four rows shorter, re-centred: a click where
/// `feature` stood — egui gives it to the list, which stood there — would be the list's first row
/// now, `up/main`, and turns nothing. With the same rows, a window resized under the pointer: the
/// click where `main` stood would be another branch, and turns nothing.
#[test]
fn a_box_laid_elsewhere_takes_no_click_in_that_pass() {
    let mut fx = filters_open(fixture_view());
    let before = filters_covers(&fx);
    let p = place_of(&layer_over(&fx.drv, &before), "[X] feature");
    pass(&mut fx, vec![Event::Text("up".into())]);
    pass(&mut fx, click_events(p, PointerButton::Primary));
    fx.idle();
    let now = filters_covers(&fx);
    assert_eq!(now[0].height + 4, before[0].height, "four rows fewer");
    let rows = filters_rows(&fx);
    row_with(&rows, "[X] up/main");
    assert!(
        fx.view().filters_dialog.is_some(),
        "no click outside it either"
    );
    assert!(
        fx.press(Key::Enter).is_empty(),
        "nothing turned: nothing to reload"
    );
    assert!(fx.view().filters_dialog.is_none() && fx.view().filters().is_default());
    // The rows unchanged, the box elsewhere: the window two rows shorter.
    let mut fx = filters_open(fixture_view());
    let before = filters_covers(&fx);
    let p = place_of(&layer_over(&fx.drv, &before), "[X] main");
    fx.drv.resize(1000.0, 600.0 - 2.0 * 16.0);
    pass(&mut fx, click_events(p, PointerButton::Primary));
    fx.idle();
    assert_ne!(filters_covers(&fx), before, "the box moved");
    assert!(
        fx.press(Key::Enter).is_empty(),
        "nothing turned: nothing to reload"
    );
    assert!(fx.view().filters_dialog.is_none() && fx.view().filters().is_default());
}

/// Type-to-filter and Enter in one pass: the filters the screen showed are applied — the typed
/// text changes none of them.
#[test]
fn typed_text_and_enter_in_one_pass_apply_what_showed() {
    let mut fx = filters_open(fixture_view());
    fx.drv.click_text_on_top(&mut fx.host, "[X] Tags");
    fx.idle();
    let mut events = vec![Event::Text("fe".into())];
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let asked = pass(&mut fx, events);
    match &asked[..] {
        [GitAction::FiltersChanged(f)] => {
            assert!(!f.tags);
            assert_eq!(f.branches, BranchChoice::All);
        }
        other => panic!("{other:?}"),
    }
    assert!(fx.view().filters_dialog.is_none());
}

/// A choice turned and Enter in one pass: what Enter would apply never showed, so nothing is
/// applied and the box says the filters changed; the next Enter applies them.
#[test]
fn a_choice_turned_and_enter_in_one_pass_apply_nothing_unseen() {
    let mut fx = filters_open(fixture_view());
    let covers = filters_covers(&fx);
    let p = place_of(&layer_over(&fx.drv, &covers), "[X] Tags");
    let mut events = click_events(p, PointerButton::Primary);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let asked = pass(&mut fx, events);
    assert!(asked.is_empty(), "{asked:?}");
    assert!(fx.view().filters_dialog.is_some(), "the dialog stays");
    fx.idle();
    let rows = filters_rows(&fx);
    row_with(&rows, "[ ] Tags");
    row_with(&rows, filters::CHANGED);
    let asked = fx.press(Key::Enter);
    match &asked[..] {
        [GitAction::FiltersChanged(f)] => assert!(!f.tags),
        other => panic!("{other:?}"),
    }
}

/// Space on a branch and Enter in one pass: the turn shows first, then Enter applies it.
#[test]
fn space_on_a_branch_and_enter_in_one_pass_apply_nothing_unseen() {
    let mut fx = filters_open(fixture_view());
    fx.drv
        .ctx
        .memory_mut(|m| m.request_focus(filters_id("branches")));
    fx.idle();
    // As egui-winit sends a typing key: the press, its text, then the release.
    let mut events = key_events(Key::Space, Modifiers::NONE);
    events.insert(1, Event::Text(" ".into()));
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    assert!(pass(&mut fx, events).is_empty());
    fx.idle();
    row_with(&filters_rows(&fx), "[ ] feature");
    let asked = fx.press(Key::Enter);
    match &asked[..] {
        [GitAction::FiltersChanged(f)] => {
            assert!(
                matches!(&f.branches, BranchChoice::Only(c) if !c.contains("refs/heads/feature"))
            )
        }
        other => panic!("{other:?}"),
    }
}

/// Enter goes to what has the keyboard: on `[ Cancel ]` it cancels; after a Shift+Tab in the same
/// pass (egui moves the keyboard after it) it is dropped, and the next one goes where the
/// keyboard went.
#[test]
fn enter_goes_where_the_keyboard_is_and_is_dropped_after_a_late_move() {
    let mut fx = filters_open(fixture_view());
    fx.drv.click_text_on_top(&mut fx.host, "[X] Tags");
    fx.idle();
    fx.drv
        .ctx
        .memory_mut(|m| m.request_focus(filters_id("cancel")));
    fx.idle();
    let mut events = key_events(Key::Tab, Modifiers::SHIFT);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    fx.drv.frame(&mut fx.host, events, Modifiers::SHIFT);
    assert!(fx.take().is_empty());
    assert!(fx.view().filters_dialog.is_some(), "the Enter was dropped");
    fx.idle();
    let on_apply = fx.drv.ctx.memory(|m| m.has_focus(filters_id("apply")));
    assert!(on_apply, "Shift+Tab went to Apply");
    let asked = fx.press(Key::Enter);
    assert!(
        matches!(&asked[..], [GitAction::FiltersChanged(f)] if !f.tags),
        "{asked:?}"
    );
    // Enter on Cancel cancels.
    let mut fx = filters_open(fixture_view());
    fx.drv.click_text_on_top(&mut fx.host, "[X] Tags");
    fx.drv
        .ctx
        .memory_mut(|m| m.request_focus(filters_id("cancel")));
    fx.idle();
    assert!(fx.press(Key::Enter).is_empty());
    assert!(fx.view().filters_dialog.is_none(), "cancelled");
    assert!(fx.view().filters().tags);
}

/// Space on a focused `[ Cancel ]` cancels, on `[ Apply ]` applies: the buttons take the kit's
/// presses.
#[test]
fn space_on_a_focused_button_presses_it() {
    for (button, applies) in [("cancel", false), ("apply", true)] {
        let mut fx = filters_open(fixture_view());
        fx.drv.click_text_on_top(&mut fx.host, "[X] Tags");
        fx.drv
            .ctx
            .memory_mut(|m| m.request_focus(filters_id(button)));
        fx.idle();
        let mut events = key_events(Key::Space, Modifiers::NONE);
        events.insert(1, Event::Text(" ".into()));
        let asked = pass(&mut fx, events);
        assert!(fx.view().filters_dialog.is_none(), "{button} closed it");
        assert_eq!(
            matches!(&asked[..], [GitAction::FiltersChanged(f)] if !f.tags),
            applies,
            "{button}: {asked:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Round 1.

/// A dialog alone over the window — the menu or the filters — and what each pass left in its
/// input after the dialog's pass: whatever is under it would read those.
struct Alone {
    menu: Option<menu::Menu>,
    filters: Option<filters::Dialog>,
    ends: Vec<String>,
    left: Vec<Vec<Event>>,
}

impl Alone {
    fn menu(target: Target) -> Alone {
        Alone {
            menu: Some(menu::Menu::new(target, &super::tests_s3::repo())),
            filters: None,
            ends: Vec::new(),
            left: Vec::new(),
        }
    }

    fn filters() -> Alone {
        let refs = super::tests::refs();
        Alone {
            menu: None,
            filters: Some(filters::Dialog::new(&Filters::default(), &refs, &[])),
            ends: Vec::new(),
            left: Vec::new(),
        }
    }
}

impl crate::gui::Window for Alone {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let grid = kit::grid(&ctx);
        if let Some(m) = &mut self.menu {
            if let Some(end) = m.ui(&ctx, grid) {
                self.ends.push(format!("{end:?}"));
                self.menu = None;
            }
        }
        if let Some(d) = &mut self.filters {
            if let Some(end) = d.ui(&ctx, grid, true) {
                self.ends.push(format!("{end:?}"));
                self.filters = None;
            }
        }
        self.left.push(ctx.input(|i| i.events.clone()));
    }

    fn ready(&self) -> bool {
        true
    }
}

/// The presses and texts `events` hold, in order: `Enter`, `Space`, `text:fix`.
fn presses(events: &[Event]) -> Vec<String> {
    (events.iter())
        .filter_map(|e| match e {
            Event::Key {
                key, pressed: true, ..
            } => Some(format!("{key:?}")),
            Event::Text(t) => Some(format!("text:{t}")),
            _ => None,
        })
        .collect()
}

/// One pass of `host` with `events`, after two passes that show it: what it left.
fn left_after(host: &mut Alone, drv: &mut Driver, events: Vec<Event>) -> Vec<String> {
    drv.frame(host, events, Modifiers::NONE);
    presses(host.left.last().expect("a pass"))
}

/// The pass that closes the menu or the filters leaves no Enter and no Space (with its text) for
/// what is under it: Esc and Enter, F10 and Space in one pass.
#[test]
fn the_closing_pass_leaves_no_enter_or_space() {
    let space = || {
        let mut e = key_events(Key::Space, Modifiers::NONE);
        e.insert(1, Event::Text(" ".into()));
        e
    };
    for (close, then) in [(Key::Escape, Key::Enter), (Key::F10, Key::Space)] {
        let mut events = key_events(close, Modifiers::NONE);
        events.extend(match then {
            Key::Space => space(),
            _ => key_events(then, Modifiers::NONE),
        });
        let mut drv = Driver::new(1200.0, 900.0);
        let mut host = Alone::menu(Target::Repository);
        drv.idle(&mut host, 2);
        let left = left_after(&mut host, &mut drv, events.clone());
        assert_eq!(host.ends, ["Cancel"], "{close:?}");
        assert!(left.is_empty(), "the menu's {close:?} pass left {left:?}");
        if close == Key::Escape {
            let mut host = Alone::filters();
            let mut drv = Driver::new(1200.0, 900.0);
            drv.idle(&mut host, 2);
            let left = left_after(&mut host, &mut drv, events);
            assert_eq!(host.ends, ["Cancel"]);
            assert!(left.is_empty(), "the filters' Esc pass left {left:?}");
        }
    }
    // Nothing under the menu acted: in the panel, Esc and Enter close it and open no details.
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    let mut events = key_events(Key::Escape, Modifiers::NONE);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let asked = pass(&mut fx, events);
    fx.idle();
    assert!(
        fx.view().menu.is_none() && !fx.view().details_open(),
        "{asked:?}"
    );
}

/// The Enters the list does not take (after a Tab in the pass) are taken out of the pass, not
/// left for what is under the menu.
#[test]
fn an_enter_after_a_tab_is_not_left_in_the_pass() {
    let mut drv = Driver::new(1200.0, 900.0);
    let mut host = Alone::menu(Target::Repository);
    drv.idle(&mut host, 2);
    let mut events = key_events(Key::Tab, Modifiers::NONE);
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let left = left_after(&mut host, &mut drv, events);
    assert!(host.ends.is_empty(), "{:?}", host.ends);
    assert!(!left.iter().any(|k| k == "Enter"), "{left:?}");
}

/// What the pass holds after the Enter that chooses a line is the dialog's that line opens: the
/// menu hands it on in its order — no Enter, which would run a dialog never shown — and lets the
/// keyboard go; the dialog shows in that same pass, and an Esc there closes it.
#[test]
fn the_rest_of_the_choosing_pass_goes_to_the_dialog_it_opens() {
    let mut drv = Driver::new(1200.0, 900.0);
    let mut host = Alone::menu(Target::Repository);
    drv.idle(&mut host, 2);
    let mut events = key_events(Key::Enter, Modifiers::NONE);
    events.push(Event::Text("fix".into()));
    events.extend(key_events(Key::Space, Modifiers::NONE));
    events.insert(4, Event::Text(" ".into()));
    events.push(Event::Text("it".into()));
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let left = left_after(&mut host, &mut drv, events);
    assert_eq!(host.ends.len(), 1, "chose: {:?}", host.ends);
    assert_eq!(left, ["text:fix", "Space", "text: ", "text:it"], "in order");
    let list = egui::Id::unique("sopen-git-menu").with("list");
    assert!(
        !drv.ctx.memory(|m| m.has_focus(list)),
        "the keyboard let go"
    );
    // In the panel: an Esc after the Enter that chose Tag… is the Tag dialog's, run in that pass:
    // it closes the dialog, nothing run.
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    let mut events = key_events(Key::Enter, Modifiers::NONE);
    events.extend(key_events(Key::Escape, Modifiers::NONE));
    pass(&mut fx, events);
    assert!(
        fx.view().menu.is_none() && fx.view().draft().is_none(),
        "opened, then closed"
    );
}

/// A right click in a panel beside another panel's open dialog opens nothing and moves no bar
/// (one dialog at a time): the integration says the other's is up.
#[test]
fn a_panel_beside_an_open_dialog_opens_none() {
    let mut fx = panel();
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    let g = fx.view().geometry().expect("the graph drawn");
    let p = centre(&fx, 60, g.row(3).expect("row 3").y);
    let asked = pass(&mut fx, click_events(p, PointerButton::Secondary));
    fx.idle();
    assert!(fx.view().menu.is_none(), "{asked:?}");
    assert_eq!(fx.view().bar_place().0, 0, "the bar stays");
    // The other's closed: the right click opens this one's menu.
    fx.view_mut().set_dialog_elsewhere(false);
    fx.idle();
    pass(&mut fx, click_events(p, PointerButton::Secondary));
    fx.idle();
    assert!(fx.view().menu.is_some());
}

/// The S3 repository in an in-progress state, its status entries `entries`.
fn in_state(state: crate::git::model::InProgress) -> crate::git::actions::Repo {
    let mut r = super::tests_s3::repo();
    r.in_progress = state;
    r
}

/// The panel on `repo`, the repository's menu open through `Repository…`.
fn repository_menu(repo: &crate::git::actions::Repo) -> Fx {
    let mut fx = Fx::new(1000.0, 700.0, fixture_view());
    fx.view_mut().set_repo(repo.clone());
    fx.idle();
    fx.press(Key::F10);
    fx.press(Key::End);
    fx.press(Key::Enter);
    fx.idle();
    fx
}

/// A state of the repository's, its name and the key lines its menu holds (label, key letter).
type Case = (
    &'static str,
    crate::git::model::InProgress,
    &'static [(&'static str, char)],
);

/// T17: a terminal without the kitty protocol loses the Shift of the Cmd/Ctrl+Shift chords
/// (10 §9), so the repository's menu holds each of the panel's: the banner's steps the state
/// offers, an edit stop's Amend and Split, bisect's Reset, the panel over both sides — each with
/// its key at the right, each running what its key runs. A rebase stopped to edit, a merge with
/// a conflict, a cherry-pick, a bisect.
#[test]
fn the_repositorys_menu_holds_every_cmd_shift_action() {
    use crate::git::model::{InProgress, Operation, OperationHead, RebaseProgress};
    use crate::git::rebase::Edit;
    let a = "a".repeat(40);
    let stop = InProgress {
        operation: Some(Operation::Rebase),
        rebase: Some(RebaseProgress {
            msgnum: Some("2".into()),
            end: Some("4".into()),
            head_name: Some("refs/heads/main".into()),
            onto: Some("b".repeat(40)),
            stopped_sha: Some(a.clone()),
            done_last: Some(format!("edit {a} third")),
        }),
        ..InProgress::default()
    };
    let merge = InProgress {
        operation: Some(Operation::Merge),
        head: Some(OperationHead {
            commit: "b".repeat(40),
            merge_msg: Some("Merge branch 'feature'".into()),
        }),
        ..InProgress::default()
    };
    let pick = InProgress {
        operation: Some(Operation::CherryPick),
        head: Some(OperationHead {
            commit: "b".repeat(40),
            merge_msg: None,
        }),
        ..InProgress::default()
    };
    let bisect = InProgress {
        bisect: true,
        ..InProgress::default()
    };
    let cmd_shift = Mods::CommandShift.prefix();
    let wide = "The git panel over both sides, and back";
    let cases: [Case; 5] = [
        ("nothing in progress", InProgress::default(), &[]),
        (
            "edit stop",
            stop,
            &[
                ("Continue", 'C'),
                ("Abort", 'A'),
                ("Amend the stopped commit…", 'M'),
                ("Split the stopped commit…", 'P'),
            ],
        ),
        ("merge", merge, &[("Continue", 'C'), ("Abort", 'A')]),
        (
            "cherry-pick",
            pick,
            &[("Continue", 'C'), ("Skip", 'S'), ("Abort", 'A')],
        ),
        ("bisect", bisect, &[("Reset (bisect)", 'B')]),
    ];
    for (name, state, steps) in cases {
        let repo = in_state(state);
        let fx = repository_menu(&repo);
        let rows = menu_rows(&fx);
        // Its key lines first, in their keys' order, each with its key at the right; its first
        // item right after them (none with nothing in progress).
        for (k, (label, key)) in steps.iter().enumerate() {
            let row = &rows[2 + k];
            assert!(row.starts_with(&format!("║  {label}")), "{name}: {rows:#?}");
            assert!(
                row.ends_with(&format!(" {cmd_shift}{key}  ║")),
                "{name}: {rows:#?}"
            );
        }
        let first = &rows[2 + steps.len()];
        assert!(
            first.starts_with("║  Fetch all remotes…"),
            "{name}: the first item after the key lines: {rows:#?}"
        );
        // The status row counts every line: the key lines, the items, the panel over both sides.
        let lines = rows.len() - 4;
        let said = fx.view().status_line().text;
        assert!(said.contains(&format!(": {lines} items")), "{name}: {said}");
        assert!(
            row_with(&rows, wide).ends_with(&format!(" {cmd_shift}G  ║")),
            "{name}: {rows:#?}"
        );
        assert!(row_with(&rows, "Fetch all remotes…").contains("Ctrl/⌘+Shift+R"));
        // Each as its key runs it: the same dialog, the same draft.
        for &(label, key) in steps.iter() {
            let mut by_menu = repository_menu(&repo);
            by_menu.drv.click_text_on_top(&mut by_menu.host, label);
            let menu_asked = by_menu.take();
            let mut by_key = Fx::new(1000.0, 700.0, fixture_view());
            by_key.view_mut().set_repo(repo.clone());
            by_key.idle();
            let letter = Key::from_name(&key.to_string()).expect("a letter");
            let key_asked = by_key.key(letter, Modifiers::COMMAND | Modifiers::SHIFT);
            let seen = |fx: &Fx| (fx.view().draft().cloned(), fx.view().edit_draft().cloned());
            assert_eq!(seen(&by_menu), seen(&by_key), "{name}: {label}");
            assert_eq!(menu_asked, key_asked, "{name}: {label}");
            assert!(
                seen(&by_menu) != (None, None) || !menu_asked.is_empty(),
                "{name}: {label} opened its dialog or asked its reads"
            );
        }
    }
    // The stop's Split is `Edit::SplitStop` of the stopped commit, not the history row's split.
    let fx = {
        let mut fx = repository_menu(&in_state(InProgress {
            operation: Some(Operation::Rebase),
            rebase: Some(RebaseProgress {
                stopped_sha: Some(a.clone()),
                done_last: Some(format!("edit {a} third")),
                ..RebaseProgress::default()
            }),
            ..InProgress::default()
        }));
        fx.drv
            .click_text_on_top(&mut fx.host, "Split the stopped commit…");
        fx
    };
    assert!(
        matches!(fx.view().edit_draft(), Some(Edit::SplitStop { .. })),
        "{:?}",
        fx.view().edit_draft()
    );
    // The panel over both sides, as Cmd/Ctrl+Shift+G.
    let mut fx = repository_menu(&super::tests_s3::repo());
    fx.drv.click_text_on_top(&mut fx.host, wide);
    assert_eq!(fx.take(), [GitAction::Wide]);
}

/// An Enter in the pass right after a choice was turned — no pass between to show it — applies
/// nothing and says the filters changed; the next Enter applies them.
#[test]
fn an_enter_right_after_a_change_says_the_filters_changed() {
    let mut fx = filters_open(fixture_view());
    let covers = filters_covers(&fx);
    let p = place_of(&layer_over(&fx.drv, &covers), "[X] Tags");
    pass(&mut fx, click_events(p, PointerButton::Primary));
    let asked = pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    assert!(
        asked.is_empty() && fx.view().filters_dialog.is_some(),
        "{asked:?}"
    );
    fx.idle();
    row_with(&filters_rows(&fx), filters::CHANGED);
    assert!(!applied(&mut fx).tags);
}

/// egui moves the keyboard once a pass: after a second move in a pass, an Enter goes nowhere
/// the owner meant, and is dropped. A Space after a late move is dropped only when the pass did
/// not begin in the field — the field typed it.
#[test]
fn a_second_move_drops_the_enter_and_the_field_keeps_its_space() {
    let mut fx = filters_open(fixture_view());
    let mut events = key_events(Key::Tab, Modifiers::NONE);
    events.extend(key_events(Key::Tab, Modifiers::NONE));
    events.extend(key_events(Key::Enter, Modifiers::NONE));
    let asked = pass(&mut fx, events);
    fx.idle();
    assert!(asked.is_empty(), "{asked:?}");
    assert!(fx.view().filters_dialog.is_some(), "the Enter was dropped");
    // In the field: Shift+Tab, a, Space, b — the field types `a b`.
    let mut fx = filters_open(fixture_view());
    let mut events = key_events(Key::Tab, Modifiers::SHIFT);
    events.push(Event::Text("a".into()));
    let mut space = key_events(Key::Space, Modifiers::NONE);
    space.insert(1, Event::Text(" ".into()));
    events.extend(space);
    events.push(Event::Text("b".into()));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    row_with(&filters_rows(&fx), "Type to filter a b");
}

/// The filters' buttons take the kit's presses: Space then Tab on `[ Apply ]` applies — egui's
/// own click would let the Tab hand the Space to `[ Cancel ]`.
#[test]
fn space_then_tab_on_apply_applies() {
    let mut fx = filters_open(fixture_view());
    fx.drv.click_text_on_top(&mut fx.host, "[X] Tags");
    fx.drv
        .ctx
        .memory_mut(|m| m.request_focus(filters_id("apply")));
    fx.idle();
    let mut events = key_events(Key::Space, Modifiers::NONE);
    events.insert(1, Event::Text(" ".into()));
    events.extend(key_events(Key::Tab, Modifiers::NONE));
    let asked = pass(&mut fx, events);
    assert!(
        matches!(&asked[..], [GitAction::FiltersChanged(f)] if !f.tags),
        "{asked:?}"
    );
}

/// While another dialog has the keyboard — another panel's, the list's question
/// (`set_dialog_elsewhere`) — an open menu reads no key: Esc and Enter are left for that dialog.
#[test]
fn an_open_menu_reads_no_key_while_another_dialog_has_it() {
    let mut fx = panel();
    fx.press(Key::ArrowDown);
    fx.press(Key::F10);
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    let asked = fx.press(Key::Enter);
    assert_eq!(chose(&fx, &asked), None, "{asked:?}");
    fx.press(Key::Escape);
    assert!(fx.view().menu.is_some(), "Esc was not the menu's");
    fx.view_mut().set_dialog_elsewhere(false);
    fx.idle();
    fx.press(Key::Escape);
    assert!(fx.view().menu.is_none(), "the control: Esc closes it");
}

/// A double click at `at`: two clicks a pass apart; what they asked for.
fn twice(fx: &mut Fx, at: Pos2) -> Vec<GitAction> {
    fx.drv.click(&mut fx.host, at);
    fx.drv.click(&mut fx.host, at);
    fx.take()
}

/// A click on the details pane, a command log row or the planner acts on nothing while another
/// panel's dialog is up — not only where egui's modal lies over it. (The pane and the planner
/// are cells, read by where their texts stand.)
#[test]
fn clicks_on_the_pane_the_log_and_the_planner_wait_for_the_other_dialog() {
    use super::tests::{day, oid};
    // The details pane, the keyboard back on the graph: a double click on a file row.
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    let subject = Subject::Row(RowKey::Commit(oid(9)));
    fx.view_mut().set_details(Details {
        subject,
        summary: Some(Summary {
            oid: oid(9),
            parents: vec![oid(7), oid(8)],
            author_name: "Ann".into(),
            author_email: "ann@example.com".into(),
            author_time: day(9),
            committer_name: "Ann".into(),
            committer_email: "ann@example.com".into(),
            commit_time: day(9),
            message: "Merge\n".into(),
        }),
        contains: None,
        groups: vec![FileGroup {
            group: Group::Files,
            files: vec![FileRow {
                letter: 'M',
                path: "src/lib.rs".into(),
                old_path: None,
                lines: Some((1, 1)),
                lossy: false,
            }],
        }],
        error: None,
    });
    fx.idle();
    fx.press(Key::Escape);
    let at = fx
        .drv
        .texts_starting_with("src/lib.rs")
        .first()
        .expect("the file row")
        .0
        .center();
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    let asked = twice(&mut fx, at);
    fx.idle();
    assert!(asked.is_empty(), "{asked:?}");
    assert!(
        !fx.view().pane.as_ref().is_some_and(|p| p.keyboard),
        "no keyboard"
    );
    fx.view_mut().set_dialog_elsewhere(false);
    // Past egui's double-click time since the clicks above.
    fx.drv.idle(&mut fx.host, 30);
    let asked = twice(&mut fx, at);
    assert!(
        asked.iter().any(|a| matches!(a, GitAction::Diff { .. })),
        "the control: {asked:?}"
    );
    // The command log: a double click shows an entry whole.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.view_mut().open_log(vec![super::tests_s3::entry(
        "git fetch origin",
        crate::git::runner::Ending::Exit(0),
        "",
        0,
    )]);
    fx.idle();
    let at = (fx.drv.texts_starting_with("").into_iter())
        .find(|(_, t)| t.contains("git fetch origin"))
        .expect("the log's row")
        .0
        .center();
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    twice(&mut fx, at);
    fx.idle();
    assert!(!fx.view().dialog_up(), "no entry shown whole");
    fx.view_mut().set_dialog_elsewhere(false);
    fx.drv.idle(&mut fx.host, 30);
    twice(&mut fx, at);
    fx.idle();
    assert!(fx.view().dialog_up(), "the control: the entry whole");
    // The planner: a click on a row moves its bar, Run… opens its confirmation.
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.view_mut().set_repo(super::tests_s4::repo());
    fx.view_mut().open_planner(super::tests_s4::model());
    fx.idle();
    fx.idle();
    let bar = |fx: &Fx| fx.view().planner.as_ref().and_then(|p| p.bar().cloned());
    let before = bar(&fx);
    let row = fx
        .drv
        .texts_starting_with("third")
        .first()
        .expect("the planner's row")
        .0
        .center();
    let run = fx.drv.find_text("[ Run… ]").expect("Run…").center();
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    pass(&mut fx, click_events(row, PointerButton::Primary));
    fx.idle();
    assert_eq!(bar(&fx), before, "the bar stays");
    pass(&mut fx, click_events(run, PointerButton::Primary));
    fx.idle();
    assert!(!fx.view().dialog_up(), "no run's confirmation");
    // Its keys wait too.
    fx.press(Key::ArrowDown);
    assert_eq!(bar(&fx), before, "no key moved it");
    fx.view_mut().set_dialog_elsewhere(false);
    fx.idle();
    pass(&mut fx, click_events(row, PointerButton::Primary));
    fx.idle();
    assert_ne!(bar(&fx), before, "the control: the click moves it");
}

/// While the filters are open the status row says their keys, not the panel's.
#[test]
fn the_status_row_says_the_filters_keys() {
    let fx = filters_open(fixture_view());
    assert_eq!(fx.view().status_line().text, filters::HINT);
}

// ---------------------------------------------------------------------------------------------
// TS-4 (10 §14): the menu and the filters, the same cells in both backends.

mod ts4 {
    use super::super::tests::{history, refs, rows_of, Host};
    use super::*;
    use crate::gui::chrome::CELL;
    use crate::gui::term::{self, Backend, End, Probed, Receipt, Tty};
    use crate::gui::Window;
    use eframe::egui::Rect;
    use std::io;
    use std::time::Duration;

    /// A terminal that sends nothing, `cols` × `rows`, never ambiguous-wide: a receipt run's.
    struct Still((u16, u16));

    impl Tty for Still {
        fn raw(&mut self, _on: bool) -> io::Result<()> {
            Ok(())
        }
        fn size(&mut self) -> io::Result<(u16, u16)> {
            Ok(self.0)
        }
        fn probe(&mut self) -> Probed {
            Probed {
                kitty: false,
                ambiguous: false,
            }
        }
        fn drop_input(&mut self) {}
        fn next(&mut self, wait: Duration) -> io::Result<Option<crossterm::event::Event>> {
            std::thread::sleep(wait.min(Duration::from_millis(1)));
            Ok(None)
        }
        fn pending(&mut self) -> io::Result<bool> {
            Ok(false)
        }
        fn stopped(&self) -> Option<i32> {
            None
        }
    }

    /// The terminal's `SOPEN_CELLS` receipt of `win` on a `cols` × `rows` terminal, and the time
    /// of its last pass.
    fn terminal_run<W: Window>(win: &mut W, cols: u16, rows: u16) -> (String, f64) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cells.txt");
        let ctx = egui::Context::default();
        chrome::install(&ctx);
        let mut b = Backend::new(Still((cols, rows)), io::sink(), ctx.clone(), true);
        b.offset = 1_000.0;
        b.receipt = Some(Receipt::new(path.clone(), Duration::from_secs(30)));
        let mut no_switch = |_: &mut Backend<Still, io::Sink>| -> io::Result<Option<String>> {
            panic!("no switch in this test")
        };
        let end = b.run(win, &mut no_switch).expect("the receipt");
        assert_eq!(end, End::Closed);
        let when = ctx.input(|i| i.time);
        (std::fs::read_to_string(&path).unwrap(), when)
    }

    /// The window's `SOPEN_CELLS` receipt of one more pass of `win` in `drv`'s context at `time`
    /// seconds, as the shell writes it (`term::window_receipt`).
    fn window_receipt<W: Window>(
        drv: &Driver,
        win: &mut W,
        cols: u16,
        rows: u16,
        time: f64,
    ) -> String {
        let size = egui::vec2(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(time),
            max_texture_side: Some(8192),
            focused: true,
            ..Default::default()
        };
        let mut pass = None;
        let mut out = drv.ctx.run_ui(raw, |ui| {
            cells::clear(ui.ctx());
            win.ui(ui);
            pass = Some(term::keep_window_pass(ui.ctx()));
        });
        out.textures_delta.clear();
        term::window_receipt(&drv.ctx, &pass.expect("the pass ran"))
    }

    /// The panel with a Hangul author, the bar on the merge, and `open` (the menu, the filters).
    fn host(open: fn(&mut GitView)) -> Host {
        let mut commits = history();
        commits[1].author_name = "김철수".into();
        let mut view = GitView::new("box:/srv/repo");
        view.set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
        view.set_rows(rows_of(commits, true), false, 2);
        view.set_paging(Paging::AllLoaded);
        view.set_repo(super::super::tests_s3::repo());
        let _ = view.move_bar(1);
        open(&mut view);
        Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: Rect::NOTHING,
            under: Vec::new(),
            side: None,
        }
    }

    /// A dialog opened over the panel: its name, how it opens, texts its receipt shows.
    /// A dialog opened over the panel: its name, how it opens, the terminal's size, texts its
    /// receipt shows.
    type Opens = (
        &'static str,
        fn(&mut GitView),
        (u16, u16),
        &'static [&'static str],
    );

    /// TS-4's menu and filters: the terminal's receipt and the window's are the same bytes, with
    /// each open over the graph — the menu also on a grid short enough to scroll its lines, the
    /// scroll bar on its border.
    #[test]
    fn ts4_the_menu_and_the_filters_are_the_same_cells_in_both_backends() {
        let opens: [Opens; 3] = [
            (
                "menu",
                GitView::open_menu,
                (100, 30),
                &["Cherry-pick…", "F5", "Repository…"],
            ),
            ("short menu", GitView::open_menu, (100, 18), &["Tag…", "░"]),
            (
                "filters",
                GitView::open_filters,
                (100, 30),
                &["[X] All branches", "(•) date", "[ Apply ]"],
            ),
        ];
        for (name, open, (cols, rows), texts) in opens {
            let mut tui = host(open);
            let (terminal, when) = terminal_run(&mut tui, cols, rows);
            let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
            let mut win = host(open);
            drv.idle(&mut win, 2);
            let in_window = window_receipt(&drv, &mut win, cols, rows, when);
            assert_eq!(terminal, in_window, "{name}");
            for text in texts {
                assert!(terminal.contains(text), "{name}: {text}\n{terminal}");
            }
            if name == "short menu" {
                assert!(!terminal.contains("Repository…"), "scrolled: {terminal}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What reached the screen (X4-dialogs' `cells::OnScreen`): the terminal prints no pass while
// input waits, so a pass it never printed was never seen.

mod on_screen {
    use super::super::tests::Host;
    use super::*;
    use crate::gui::term;
    use crate::gui::Window;
    use crossterm::event::{
        Event as CEvent, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// What the panel showed as each burst was sent: its menu open, a dialog's draft, the
    /// filters open, the filters asked for so far.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    struct State {
        menu: bool,
        draft: bool,
        filters: bool,
        applied: usize,
    }

    /// A terminal that sends its bursts of events back to back — each waiting while the next is
    /// unread, so the backend prints only the last pass of a burst — one burst once the backend
    /// has asked three times since the last (its passes done and printed), then ends the run
    /// (X4-dialogs' `Bursts`).
    struct Bursts {
        bursts: VecDeque<Vec<CEvent>>,
        now: VecDeque<CEvent>,
        asked: usize,
        state: Rc<Cell<State>>,
        sent_at: Rc<RefCell<Vec<State>>>,
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
        fn next(&mut self, _wait: std::time::Duration) -> std::io::Result<Option<CEvent>> {
            if let Some(ev) = self.now.pop_front() {
                return Ok(Some(ev));
            }
            self.asked += 1;
            assert!(self.asked < 20_000, "the script ran out without an end");
            if self.asked >= 3 {
                if let Some(burst) = self.bursts.pop_front() {
                    self.sent_at.borrow_mut().push(self.state.get());
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
            (self.bursts.is_empty() && self.now.is_empty() && self.asked >= 3)
                .then_some(libc::SIGTERM)
        }
    }

    /// The panel in the terminal, saying what it shows.
    struct InTerminal {
        host: Host,
        state: Rc<Cell<State>>,
    }

    impl Window for InTerminal {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.host.ui(ui);
            let v = &self.host.view;
            let applied = (self.host.actions.iter())
                .filter(|a| matches!(a, GitAction::FiltersChanged(_)))
                .count();
            self.state.set(State {
                menu: v.menu.is_some(),
                draft: v.draft().is_some(),
                filters: v.filters_dialog.is_some(),
                applied,
            });
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// The fixture's panel with the S3 repository, the bar on the merge.
    fn host() -> Host {
        host_of(fixture_view())
    }

    /// `view` with the S3 repository, the bar on the merge, as the panel's host.
    fn host_of(mut view: GitView) -> Host {
        view.set_repo(super::super::tests_s3::repo());
        let _ = view.move_bar(1);
        Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: egui::Rect::NOTHING,
            under: Vec::new(),
            side: None,
        }
    }

    fn press(code: KeyCode) -> CEvent {
        CEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// A left press and release at cell (`column`, `row`), each an event.
    fn click(column: u16, row: u16) -> [CEvent; 2] {
        let at = |kind| {
            CEvent::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        [
            at(MouseEventKind::Down(MouseButton::Left)),
            at(MouseEventKind::Up(MouseButton::Left)),
        ]
    }

    /// The cell where the window, laid out as the terminal lays it (100×30), shows `text` with
    /// the filters open: their layout is a function of the grid alone.
    fn cell_of(text: &str) -> (u16, u16) {
        cell_in(host(), text)
    }

    /// [`cell_of`] in `win`.
    fn cell_in(mut win: Host, text: &str) -> (u16, u16) {
        win.view.open_filters();
        let mut drv = Driver::new(800.0, 480.0);
        drv.idle(&mut win, 3);
        let at = drv
            .find_text(text)
            .unwrap_or_else(|| panic!("{text:?}"))
            .center();
        ((at.x / 8.0) as u16, (at.y / 16.0) as u16)
    }

    /// `win` run in the terminal over `bursts`: what showed as each burst was sent.
    fn run(win: Host, bursts: Vec<Vec<CEvent>>) -> (InTerminal, Vec<State>) {
        let state = Rc::new(Cell::new(State::default()));
        let sent_at = Rc::new(RefCell::new(Vec::new()));
        let mut all = vec![vec![CEvent::FocusGained], vec![CEvent::FocusGained]];
        all.extend(bursts);
        let tty = Bursts {
            bursts: all.into(),
            now: VecDeque::new(),
            asked: 0,
            state: state.clone(),
            sent_at: sent_at.clone(),
        };
        let ctx = egui::Context::default();
        chrome::install(&ctx);
        let mut b = term::Backend::new(tty, std::io::sink(), ctx, true);
        let mut win = InTerminal { host: win, state };
        let mut no_switch =
            |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
        let end = b.run(&mut win, &mut no_switch).expect("the run");
        assert_eq!(end, term::End::Signal(libc::SIGTERM));
        let sent = sent_at.borrow().clone();
        (win, sent)
    }

    /// F10 and Enter in one burst: F10's pass, which opened the menu, was never printed, so the
    /// Enter chooses nothing; the next Enter, the menu on screen, chooses its first item.
    #[test]
    fn f10_and_enter_in_one_burst_choose_nothing() {
        let (win, sent) = run(
            host(),
            vec![
                vec![press(KeyCode::F(10)), press(KeyCode::Enter)],
                vec![press(KeyCode::Enter)],
            ],
        );
        let open = State {
            menu: true,
            ..State::default()
        };
        assert_eq!(
            sent.last(),
            Some(&open),
            "nothing chosen before the last Enter"
        );
        assert_eq!(win.host.view.draft().map(Action::id), Some(ActionId::Tag));
    }

    /// F10 printed, then Enter: the menu was on screen, and Enter chooses its first item.
    #[test]
    fn f10_printed_then_enter_chooses() {
        let (win, sent) = run(
            host(),
            vec![vec![press(KeyCode::F(10))], vec![press(KeyCode::Enter)]],
        );
        assert!(sent.last().is_some_and(|s| s.menu && !s.draft), "{sent:?}");
        assert_eq!(win.host.view.draft().map(Action::id), Some(ActionId::Tag));
    }

    /// A filter turned and Enter in one burst — a pass that changes nothing between them — apply
    /// nothing: the screen still showed Tags on. The next Enter, the change on screen, applies it.
    #[test]
    fn a_filter_turned_and_enter_in_one_burst_apply_nothing() {
        let (x, y) = cell_of("[X] Tags");
        let mut win = host();
        win.view.open_filters();
        let mut turn = click(x, y).to_vec();
        turn.extend([CEvent::FocusGained, press(KeyCode::Enter)]);
        let (win, sent) = run(win, vec![turn, vec![press(KeyCode::Enter)]]);
        let last = sent.last().copied().unwrap_or_default();
        assert!(
            last.filters && last.applied == 0,
            "nothing applied: {sent:?}"
        );
        let applied: Vec<&GitAction> = win.host.actions.iter().collect();
        assert!(
            matches!(&applied[..], [GitAction::FiltersChanged(f)] if !f.tags),
            "{applied:?}"
        );
    }

    /// A click on `[ Apply ]` whose press and release are read together — the press's pass not
    /// printed — applies what shows: that pass changed nothing on screen.
    #[test]
    fn a_click_on_apply_read_together_applies_what_shows() {
        let tags = cell_of("[X] Tags");
        let apply = cell_of("[ Apply ]");
        let mut win = host();
        win.view.open_filters();
        let (win, _) = run(
            win,
            vec![
                click(tags.0, tags.1).to_vec(),
                click(apply.0, apply.1).to_vec(),
            ],
        );
        let applied: Vec<&GitAction> = win.host.actions.iter().collect();
        assert!(
            matches!(&applied[..], [GitAction::FiltersChanged(f)] if !f.tags),
            "{applied:?}"
        );
        assert!(win.host.view.filters_dialog.is_none(), "applied and closed");
    }

    /// Typed text and a click on a branch in one burst, a pass that changes nothing between
    /// them: the typing narrowed the rows after its pass, and no pass since reached the screen,
    /// so the click — where `feature` showed, `fx-00` now — turns nothing; Enter applies nothing.
    #[test]
    fn typed_text_and_a_click_in_one_burst_turn_nothing_unseen() {
        let feature = cell_in(host_of(super::many_view()), "[X] feature");
        let mut win = host_of(super::many_view());
        win.view.open_filters();
        let mut burst = vec![press(KeyCode::Char('f')), press(KeyCode::Char('x'))];
        burst.push(CEvent::FocusGained);
        burst.extend(click(feature.0, feature.1));
        let (win, _) = run(win, vec![burst, vec![press(KeyCode::Enter)]]);
        assert!(win.host.actions.is_empty(), "{:?}", win.host.actions);
        assert!(win.host.view.filters().is_default());
    }

    /// The panel in the terminal, keeping what it held under the dialog a menu choice opened in
    /// the first pass that painted that dialog (F-menu-own): the cells with a glyph under its box
    /// and its shadow, in its own painting and as the pass left them (`Under`).
    struct Watched {
        host: Host,
        first: Option<Under>,
    }

    impl Window for Watched {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.host.ui(ui);
            if self.first.is_some() {
                return;
            }
            let grid = Grid::over(self.host.rect);
            let ctx = ui.ctx().clone();
            let covers = opened_on(&self.host.view, &grid).filter(|c| painted_over(&ctx, c));
            if let Some(covers) = covers {
                let (own, left) = panel_cells(&ctx, &grid);
                self.first = Some((left_under(&own, &covers), left_under(&left, &covers)));
            }
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// F-menu-own under `term::Backend`, recording on: F10 printed, then Enter on Tag…: in the
    /// first pass that paints the Tag dialog, the panel's recorded cells — what the backend's
    /// screen is composed from — are blank under its box and its shadow. The screen itself
    /// (`term::compose`) shows the dialog's cells in its box whatever lies under them, and these
    /// graph rows hold no wide glyph for its edge to cut, so the record is what this proves.
    #[test]
    fn f_menu_own_the_recorded_panel_is_blank_under_the_dialog_in_the_backend() {
        let mut all = vec![vec![CEvent::FocusGained], vec![CEvent::FocusGained]];
        all.extend([vec![press(KeyCode::F(10))], vec![press(KeyCode::Enter)]]);
        let tty = Bursts {
            bursts: all.into(),
            now: VecDeque::new(),
            asked: 0,
            state: Rc::new(Cell::new(State::default())),
            sent_at: Rc::new(RefCell::new(Vec::new())),
        };
        let ctx = egui::Context::default();
        chrome::install(&ctx);
        let mut b = term::Backend::new(tty, std::io::sink(), ctx, true);
        let mut win = Watched {
            host: host(),
            first: None,
        };
        let mut no_switch =
            |_: &mut term::Backend<Bursts, std::io::Sink>| panic!("no switch in this test");
        let end = b.run(&mut win, &mut no_switch).expect("the run");
        assert_eq!(end, term::End::Signal(libc::SIGTERM));
        assert_eq!(win.host.view.draft().map(Action::id), Some(ActionId::Tag));
        let (painted, left) = win.first.expect("the Tag dialog painted");
        assert!(
            !painted.is_empty(),
            "the control: graph text under the box in the panel's own painting"
        );
        assert_eq!(left, [], "left under the Tag dialog");
    }
}

// ---------------------------------------------------------------------------------------------
// F-git-listq: while another dialog has the keyboard — another panel's, the list's question, the
// tree (`set_dialog_elsewhere`) — none of the panel's dialogs reads a key, those it draws in its
// body (the planner's own, the command log's entry shown whole) too.

/// An Enter and an Esc in one pass, the keys a dialog runs and closes by.
fn enter_and_esc() -> Vec<Event> {
    let mut events = key_events(Key::Enter, Modifiers::NONE);
    events.extend(key_events(Key::Escape, Modifiers::NONE));
    events
}

/// The planner's run confirmation (Run…) reads neither key of the pass while another dialog has
/// the keyboard: it neither runs nor closes. The control: Esc closes it once the keyboard is back.
#[test]
fn f_git_listq_the_planners_confirmation_reads_no_key_while_another_dialog_has_it() {
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.view_mut().set_repo(super::tests_s4::repo());
    fx.view_mut().open_planner(super::tests_s4::model());
    fx.idle();
    fx.idle();
    let run = fx.drv.find_text("[ Run… ]").expect("Run…").center();
    pass(&mut fx, click_events(run, PointerButton::Primary));
    fx.idle();
    assert!(fx.view().dialog_up(), "the run's confirmation");
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    let asked = pass(&mut fx, enter_and_esc());
    fx.idle();
    assert!(asked.is_empty(), "{asked:?}");
    assert!(fx.view().dialog_up(), "neither key was the confirmation's");
    fx.view_mut().set_dialog_elsewhere(false);
    fx.idle();
    fx.press(Key::Escape);
    assert!(!fx.view().dialog_up(), "the control: Esc closes it");
}

/// The command log's entry shown whole reads neither key of the pass while another dialog has
/// the keyboard. The control: Esc closes it once the keyboard is back.
#[test]
fn f_git_listq_the_logs_entry_reads_no_key_while_another_dialog_has_it() {
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    fx.view_mut().open_log(vec![super::tests_s3::entry(
        "git fetch origin",
        crate::git::runner::Ending::Exit(0),
        "",
        0,
    )]);
    fx.idle();
    let at = (fx.drv.texts_starting_with("").into_iter())
        .find(|(_, t)| t.contains("git fetch origin"))
        .expect("the log's row")
        .0
        .center();
    twice(&mut fx, at);
    fx.idle();
    assert!(fx.view().dialog_up(), "the entry whole");
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    pass(&mut fx, enter_and_esc());
    fx.idle();
    assert!(fx.view().dialog_up(), "neither key was the entry's");
    fx.view_mut().set_dialog_elsewhere(false);
    fx.idle();
    fx.press(Key::Escape);
    assert!(!fx.view().dialog_up(), "the control: Esc closes it");
}

/// Guards existing behaviour (its keys half held on ee259ab: the quick dialog is drawn after
/// `take_keys`): a quick edit's dialog (Reword) reads no typed text and neither key of the pass
/// while another dialog has the keyboard, so it never closes on an Enter meant for that dialog.
/// The control: once the keyboard is back, typing reaches its message and Esc closes it. (Its plan
/// waits for the integration's reads here, so no Enter runs it.)
#[test]
fn f_git_listq_a_quick_dialog_reads_no_key_while_another_dialog_has_it() {
    let mut view = fixture_view();
    view.set_repo(super::tests_s4::repo());
    let mut fx = Fx::new(1000.0, 600.0, view);
    let c = crate::git::model::Oid::parse(&"c".repeat(40)).unwrap();
    let model = super::tests_s4::model();
    let asked = fx
        .view_mut()
        .open_edit(crate::git::rebase::EditId::Reword, &c, &model);
    assert!(asked.is_empty(), "{asked:?}");
    fx.idle();
    fx.idle();
    assert!(fx.drv.find_text("third").is_some(), "its message");
    fx.view_mut().set_dialog_elsewhere(true);
    fx.idle();
    let mut events = vec![Event::Text("x".into())];
    events.extend(enter_and_esc());
    let asked = pass(&mut fx, events);
    fx.idle();
    assert!(asked.is_empty(), "{asked:?}");
    assert!(fx.view().dialog_up(), "neither key was the dialog's");
    assert!(fx.drv.find_text("xthird").is_none(), "nor the typed text");
    fx.view_mut().set_dialog_elsewhere(false);
    fx.idle();
    fx.typed("!");
    fx.idle();
    assert!(
        fx.drv.find_text("!third").is_some(),
        "the control: typed into its message"
    );
    fx.press(Key::Escape);
    assert!(!fx.view().dialog_up(), "the control: Esc closes it");
}

// ---------------------------------------------------------------------------------------------
// F-git-covered: a dialog of the panel's that does not have the keyboard reads no key and
// records no painting.

/// The filters under a result of their own panel's (`modals` draws it over them): they read no
/// key, the pass's are the result's, and record no painting. Enter closes the result and applies
/// nothing under it; once a printed pass shows the filters with the keys, Enter applies them.
#[test]
fn f_git_covered_the_filters_under_their_panels_result_read_no_key() {
    let mut fx = filters_open(fixture_view());
    fx.idle();
    fx.view_mut()
        .show_text("Fetch", "fatal: the remote hung up", None);
    fx.idle();
    fx.idle();
    let asked = pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    assert!(fx.view().result.is_none(), "the Enter closed the result");
    assert!(
        fx.view().filters_dialog.is_some(),
        "and applied nothing under it: {asked:?}"
    );
    fx.idle();
    pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    assert!(
        fx.view().filters_dialog.is_none(),
        "printed with the keys, the filters apply"
    );
}

/// Another dialog has the keyboard (`set_dialog_elsewhere`) in a printed pass, then closes in a
/// pass the terminal does not print: an Enter read with that close acts on nothing of the panel's
/// dialog; the next Enter, the dialog printed with the keys, does.
fn covered_then_enter(fx: &mut Fx) -> (Vec<GitAction>, Vec<GitAction>) {
    fx.idle();
    fx.idle();
    fx.view_mut().set_dialog_elsewhere(true);
    pass(fx, vec![]);
    fx.view_mut().set_dialog_elsewhere(false);
    pass(fx, vec![]);
    cells::set_presented(&fx.drv.ctx, false);
    let first = pass(fx, key_events(Key::Enter, Modifiers::NONE));
    cells::set_presented(&fx.drv.ctx, true);
    let up = fx.view().dialog_up();
    let second = pass(fx, key_events(Key::Enter, Modifiers::NONE));
    assert!(up, "the dialog stayed up after the first Enter: {first:?}");
    (first, second)
}

/// "Stop git?" under another dialog that has the keyboard (another panel's, the list's question,
/// F12's): `[close the other, Enter]` read together stops nothing; the next Enter stops the run.
#[test]
fn f_git_covered_stop_git_under_another_dialog_stops_nothing() {
    let mut fx = panel();
    fx.view_mut().ask_stop("git fetch".to_string(), 7);
    let (first, second) = covered_then_enter(&mut fx);
    assert_eq!(first, [], "shown covered");
    assert_eq!(second, [GitAction::StopWrite(7)]);
    assert!(!fx.view().dialog_up());
}

/// A result under another dialog that has the keyboard: `[close the other, Enter]` read together
/// closes nothing; the next Enter closes it.
#[test]
fn f_git_covered_a_result_under_another_dialog_closes_on_no_enter() {
    let mut fx = panel();
    fx.view_mut()
        .show_text("Fetch", "fatal: the remote hung up", None);
    covered_then_enter(&mut fx);
    assert!(fx.view().result.is_none(), "the second Enter closed it");
}

/// `text` typed a character a pass, in the passes right after an action's dialog opened over
/// another of the panel's, with no pass between: the dialog's draft after them.
pub(super) fn typed_from_its_first_passes(fx: &mut Fx, text: &str) -> Option<Action> {
    for c in text.chars() {
        let typed = vec![Event::Text(c.to_string())];
        fx.drv.frame(&mut fx.host, typed, Modifiers::NONE);
    }
    fx.idle();
    fx.view().draft().cloned()
}

/// F-git-covered review 1: an action's dialog with a field over the filters — the Stash dialog
/// the integration opened, as the Commit after a `merge --squash` its prefill opens — has the
/// keyboard: the filters, under it, take none for their own field, so every character typed from
/// its first passes on goes into its field, and none into the filters'.
#[test]
fn f_git_covered_text_typed_over_the_filters_goes_into_the_dialog() {
    let mut fx = panel();
    fx.key(Key::K, Modifiers::COMMAND);
    assert!(fx.view().filters_dialog.is_some(), "the filters");
    fx.idle();
    let asked =
        (fx.view_mut()).open_action(ActionId::Stash, Target::Uncommitted, Default::default());
    assert!(asked.is_empty(), "{asked:?}");
    let draft = typed_from_its_first_passes(&mut fx, "qzwv");
    assert!(
        matches!(&draft, Some(Action::Stash { message, .. }) if message == "qzwv"),
        "the dialog's field: {draft:?}"
    );
    let rows = filters_rows(&fx);
    assert!(
        rows.iter().all(|r| !r.contains('q') && !r.contains('z')),
        "the filters' field stays empty: {rows:#?}"
    );
}

/// F-git-covered fix-check: the keyboard comes back to the filters' field once the dialog over
/// them closes — Esc on the Stash dialog, a pass — so what is typed then narrows the filters.
#[test]
fn f_git_covered_the_filters_take_the_keyboard_back() {
    let mut fx = panel();
    fx.key(Key::K, Modifiers::COMMAND);
    fx.idle();
    let asked =
        (fx.view_mut()).open_action(ActionId::Stash, Target::Uncommitted, Default::default());
    assert!(asked.is_empty(), "{asked:?}");
    fx.idle();
    fx.idle();
    fx.press(Key::Escape);
    assert!(fx.view().draft().is_none(), "Esc closed the Stash dialog");
    assert!(fx.view().filters_dialog.is_some(), "and left the filters");
    fx.idle();
    fx.typed("qz");
    let rows = filters_rows(&fx);
    assert!(
        rows.iter().any(|r| r.contains("qz")),
        "typed into the filters' field: {rows:#?}"
    );
}

/// F-git-covered review 1: the cover under a result is what the filters painted, not only which
/// keys they read: a result over them in a printed pass, Esc closing it in a pass the terminal
/// does not print, then Enter applies nothing — the screen last showed the filters under the
/// result; after a printed pass Enter applies them.
#[test]
fn f_git_covered_the_filters_shown_under_a_result_apply_on_no_enter() {
    let mut fx = filters_open(fixture_view());
    fx.idle();
    fx.view_mut()
        .show_text("Fetch", "fatal: the remote hung up", None);
    fx.idle();
    fx.idle();
    pass(&mut fx, key_events(Key::Escape, Modifiers::NONE));
    cells::set_presented(&fx.drv.ctx, false);
    assert!(fx.view().result.is_none(), "the Esc closed the result");
    let asked = pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    cells::set_presented(&fx.drv.ctx, true);
    assert!(
        fx.view().filters_dialog.is_some(),
        "shown under the result: nothing applied ({asked:?})"
    );
    fx.idle();
    pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    assert!(fx.view().filters_dialog.is_none(), "shown, they apply");
}

/// The panel under a dialog in front of it with a field — the list's, F12's, another panel's —
/// which takes the keyboard while nothing has it, as the git dialogs do, and reads what is typed
/// while it has it.
struct Front {
    host: super::tests::Host,
    typed: String,
}

impl Front {
    fn field() -> egui::Id {
        egui::Id::unique("f-git-covered-front-field")
    }
}

impl crate::gui::Window for Front {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.host.ui(ui);
        let ctx = ui.ctx().clone();
        if ctx.memory(|m| m.focused()).is_none() {
            ctx.memory_mut(|m| m.request_focus(Front::field()));
        }
        if ctx.memory(|m| m.has_focus(Front::field())) {
            ctx.input(|i| {
                for e in &i.events {
                    if let Event::Text(t) = e {
                        self.typed.push_str(t);
                    }
                }
            });
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

/// `view` in a 1000×600 window, `open` opening its git dialog, drawn twice; then the front dialog
/// comes up over it (`set_dialog_elsewhere`), its first request for the keyboard dropped (a new
/// box's: nothing holds the keyboard as the next pass begins), and `qzwv` is typed a character a
/// pass: what the front dialog read, and the git dialog's draft.
fn typed_under_a_front_dialog(
    view: GitView,
    open: impl FnOnce(&mut Driver, &mut Front),
) -> (String, Option<Action>) {
    let mut win = Front {
        host: super::tests::Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: egui::Rect::NOTHING,
            under: Vec::new(),
            side: None,
        },
        typed: String::new(),
    };
    let mut drv = Driver::new(1000.0, 600.0);
    drv.idle(&mut win, 1);
    open(&mut drv, &mut win);
    drv.idle(&mut win, 2);
    assert!(win.host.view.dialog_up(), "the git dialog");
    assert_ne!(
        drv.ctx.memory(|m| m.focused()),
        Some(Front::field()),
        "the git dialog has the keyboard"
    );
    win.host.view.set_dialog_elsewhere(true);
    drv.ctx.memory_mut(|m| {
        if let Some(f) = m.focused() {
            m.surrender_focus(f);
        }
    });
    for c in "qzwv".chars() {
        drv.frame(&mut win, vec![Event::Text(c.to_string())], Modifiers::NONE);
    }
    (win.typed, win.host.view.draft().cloned())
}

/// F-git-covered: a covered action dialog takes no keyboard, even while nothing has it: every
/// character typed goes into the front dialog's field, none into the Stash dialog's behind it.
#[test]
fn f_git_covered_a_covered_action_dialog_takes_no_keyboard() {
    let mut view = fixture_view();
    view.set_repo(super::tests_s3::repo());
    let (front, draft) = typed_under_a_front_dialog(view, |_, win| {
        let asked =
            (win.host.view).open_action(ActionId::Stash, Target::Uncommitted, Default::default());
        assert!(asked.is_empty(), "{asked:?}");
    });
    assert_eq!(front, "qzwv", "the front dialog's field");
    assert!(
        matches!(&draft, Some(Action::Stash { message, .. }) if message.is_empty()),
        "{draft:?}"
    );
}

/// F-git-covered: a covered quick edit's dialog takes no keyboard either.
#[test]
fn f_git_covered_a_covered_quick_edit_takes_no_keyboard() {
    let mut view = fixture_view();
    view.set_repo(super::tests_s4::repo());
    let (front, _) = typed_under_a_front_dialog(view, |_, win| {
        let c = crate::git::model::Oid::parse(&"c".repeat(40)).unwrap();
        let model = super::tests_s4::model();
        let asked = win.host.view.open_edit(EditId::Reword, &c, &model);
        assert!(asked.is_empty(), "{asked:?}");
    });
    assert_eq!(front, "qzwv", "the front dialog's field");
}

/// F-git-covered: a covered menu takes the keyboard back no more.
#[test]
fn f_git_covered_a_covered_menu_takes_no_keyboard() {
    let mut view = fixture_view();
    view.set_repo(super::tests_s3::repo());
    let (front, _) = typed_under_a_front_dialog(view, |drv, win| {
        drv.key(win, Key::ArrowDown, Modifiers::NONE);
        drv.key(win, Key::F10, Modifiers::NONE);
        assert!(win.host.view.menu.is_some(), "the menu");
    });
    assert_eq!(front, "qzwv", "the front dialog's field");
}

// ---------------------------------------------------------------------------------------------
// F-menu-own (X4-dialogs2's review of X4-menu): a menu choice that opens an action's dialog or a
// quick edit's draws it in the pass that chose it (`GitView::modals`), after the panel took its
// covers for the menu. In the pass that dialog first shows, the panel is blank under its box and
// its shadow, as under every dialog of its own (cells.rs, one buffer over another).

/// The box and the shadow of the dialog a choice of `modals`' opened — a quick edit's or an
/// action's — as it stands on the panels' grid now.
fn opened_covers(fx: &Fx) -> Option<Vec<Cells>> {
    opened_on(fx.view(), &grid_of(fx))
}

/// [`opened_covers`] of `view` on `grid`.
fn opened_on(view: &GitView, grid: &Grid) -> Option<Vec<Cells>> {
    (view.quick.as_ref().map(|d| d.covers(grid)))
        .or_else(|| view.dialog.as_ref().map(|d| d.covers(grid)))
}

/// The panel's own layer of cells in the pass just run in `ctx`, as it painted it, and that layer
/// as the pass left it ([`cells::with_later`]; the terminal's run has no `Fx` for
/// `tests_x4_graph::panel`).
fn panel_cells(ctx: &egui::Context, grid: &Grid) -> (Arc<Layer>, Layer) {
    let layers = cells::layers(ctx);
    let at = (layers.iter())
        .position(|l| l.buf.area == grid.all() && l.origin == grid.origin)
        .expect("the panel's cells over its side");
    (layers[at].clone(), cells::with_later(&layers, at))
}

/// The cells of `panel` under `covers` that hold a glyph, each as (x, y, glyph).
fn left_under(panel: &Layer, covers: &[Cells]) -> Vec<(u16, u16, String)> {
    let seen = panel.seen();
    let a = panel.buf.area;
    let mut left = Vec::new();
    for c in covers.iter().map(|c| c.intersection(a)) {
        for y in c.top()..c.bottom() {
            for x in c.left()..c.right() {
                let s = &seen[usize::from(y - a.y)][usize::from(x - a.x)];
                if !s.symbol.trim().is_empty() {
                    left.push((x, y, s.symbol.clone()));
                }
            }
        }
    }
    left
}

/// Whether the pass just run in `ctx` painted a layer of cells over exactly `covers`.
fn painted_over(ctx: &egui::Context, covers: &[Cells]) -> bool {
    let area = covers.iter().fold(covers[0], |a, c| a.union(*c));
    cells::layers(ctx).iter().any(|l| l.buf.area == area)
}

/// The box and the shadow of the last dialog the pass just run in `ctx` painted — a modal's layer
/// of cells (egui's `Foreground` order) — but the one over `except` (the menu's box and shadow),
/// from that layer: as it stood on screen, whatever the pass typed into it since it was laid out.
fn painted_box(ctx: &egui::Context, except: &[Cells]) -> Option<Vec<Cells>> {
    let menu = except.iter().fold(except[0], |a, c| a.union(*c));
    let layers = cells::layers(ctx);
    let l = (layers.iter().rev())
        .find(|l| l.layer.order == egui::Order::Foreground && l.buf.area != menu)?;
    let a = l.buf.area;
    let frame = Cells::new(a.x, a.y, a.width.checked_sub(2)?, a.height.checked_sub(1)?);
    let [right, under] = kit::shadow_of(frame);
    Some(vec![frame, right, under])
}

/// The cells with a glyph under a dialog's box and shadow: in the panel's own painting of the
/// pass (the premise — graph text there to blank), and as the pass left them.
type Under = (Vec<(u16, u16, String)>, Vec<(u16, u16, String)>);

/// `events` as one pass with the menu open, then passes with nothing, one at a time, until the
/// pass the dialog its choice opened first shows in: that pass's number (0, the choosing one) and
/// what the panel held under that dialog's box and shadow there ([`Under`]).
fn under_its_first_pass(fx: &mut Fx, events: Vec<Event>) -> (usize, Under) {
    let menu = menu_covers(fx);
    let mut events = events;
    for k in 0..4 {
        let now = std::mem::take(&mut events);
        fx.drv.frame(&mut fx.host, now, Modifiers::NONE);
        let shown = opened_covers(fx).and(painted_box(&fx.drv.ctx, &menu));
        if let Some(covers) = shown {
            let (own, _) = panel_cells(&fx.drv.ctx, &grid_of(fx));
            let left = left_under(&super::tests_x4_graph::panel(fx), &covers);
            return (k, (left_under(&own, &covers), left));
        }
    }
    panic!("no dialog showed: {:?}", fx.take())
}

/// A screen of Hangul in cells, painted, then `covers` blanked over it as the panel blanks under
/// a dialog a choice opened ([`super::blank_over`]).
struct Patched {
    covers: Vec<Cells>,
}

impl crate::gui::Window for Patched {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let grid = Grid::over(ui.ctx().content_rect());
        let body = Pen::new(PALETTE.text, PALETTE.back);
        let mut buf = cells::Buffer::empty(grid.all());
        cells::fill(&mut buf, grid.all(), body);
        for y in 0..grid.rows {
            cells::put(&mut buf, 0, y, &"한글".repeat(40), 80, body);
        }
        let painter = ui.painter().clone();
        cells::paint(&painter, grid.origin, &buf, None);
        super::blank_over(&painter, &grid, &mut buf, &self.covers, body);
    }

    fn ready(&self) -> bool {
        true
    }
}

/// What the blanking paints over the panel's painting, at a box's edges in a Hangul row: the
/// glyph whose second cell the box's left column is goes whole, and so does one whose first cell
/// is the box's last column — its second cell, outside the box, gets the body under it too, so
/// no half of a glyph painted before shows in the window (a terminal erases it whole).
#[test]
fn f_menu_own_the_blanking_takes_whole_glyphs_at_the_edges() {
    // Each glyph two cells, from column 0: column 5 a second cell, column 10 a first.
    let covers = vec![Cells::new(5, 2, 6, 3)];
    let mut drv = Driver::new(80.0 * chrome::CELL.x, 8.0 * chrome::CELL.y);
    let mut win = Patched { covers };
    drv.idle(&mut win, 1);
    let grid = Grid::over(drv.ctx.content_rect());
    let patch = (cells::layers(&drv.ctx).into_iter())
        .find(|l| l.buf.area != grid.all())
        .expect("the blanked cells, a layer of their own");
    assert_eq!(patch.buf.area, Cells::new(4, 2, 8, 3), "columns 4 to 11");
    let (_, panel) = panel_cells(&drv.ctx, &grid);
    let rows = |y: u16| cells::row_text(&panel.buf, y).0;
    assert_eq!(rows(1), "한글".repeat(20), "the row above as it was");
    for y in 2..5 {
        let blanked = format!("한글{}{}", " ".repeat(8), "한글".repeat(17));
        assert_eq!(rows(y), blanked, "row {y}");
    }
    for y in 2..5 {
        let run = grid.rect(Cells::new(4, y, 8, 1));
        assert!(
            (drv.rects().iter()).any(|&(r, fill, _)| r == run && fill == PALETTE.back),
            "the body under columns 4 to 11 of row {y}"
        );
    }
}

/// F10's menu on a commit, then Enter on its first item, Tag…, in one pass — alone, and with
/// text typed after it: the Tag dialog shows in that pass (X4-menu), no graph row stands under
/// its box or its shadow in the panel's cells there, and the text is the dialog's.
#[test]
fn f_menu_own_an_action_dialog_a_choice_opens_stands_over_blank_cells() {
    for typed in ["", "v9"] {
        let mut fx = Fx::new(1000.0, 400.0, fixture_view());
        fx.view_mut().set_repo(super::tests_s3::repo());
        fx.idle();
        fx.press(Key::ArrowDown);
        fx.press(Key::F10);
        let mut events = key_events(Key::Enter, Modifiers::NONE);
        if !typed.is_empty() {
            events.push(Event::Text(typed.into()));
        }
        let (pass, (painted, left)) = under_its_first_pass(&mut fx, events);
        assert!(
            !painted.is_empty(),
            "{typed:?}: the control: graph text under the box in the panel's own painting"
        );
        assert!(
            matches!(fx.view().draft(), Some(Action::Tag { name, .. }) if name == typed),
            "{typed:?}: {:?}",
            fx.view().draft()
        );
        assert_eq!(pass, 0, "{typed:?}: shown in the pass that chose it");
        assert_eq!(left, [], "{typed:?}: left under the Tag dialog");
    }
}

/// The repository's menu at an edit stop, then Enter on its key line "Split the stopped
/// commit…" in one pass: its quick edit's confirmation opens and shows in that pass
/// (`confirm_edit`, not the integration's reads), the panel blank under its box and its shadow.
#[test]
fn f_menu_own_a_quick_edit_a_choice_opens_stands_over_blank_cells() {
    use crate::git::model::{InProgress, Operation, RebaseProgress};
    use crate::git::rebase::Edit;
    let a = "a".repeat(40);
    let repo = in_state(InProgress {
        operation: Some(Operation::Rebase),
        rebase: Some(RebaseProgress {
            stopped_sha: Some(a.clone()),
            done_last: Some(format!("edit {a} third")),
            ..RebaseProgress::default()
        }),
        ..InProgress::default()
    });
    let mut fx = Fx::new(1000.0, 400.0, fixture_view());
    fx.view_mut().set_repo(repo);
    fx.idle();
    fx.press(Key::F10);
    fx.press(Key::End);
    fx.press(Key::Enter);
    fx.idle();
    let rows = menu_rows(&fx);
    let lines: Vec<&String> = rows.iter().skip(2).collect();
    let at = (lines.iter())
        .position(|r| r.contains("Split the stopped commit…"))
        .unwrap_or_else(|| panic!("no Split line: {rows:#?}"));
    for _ in 0..at {
        fx.press(Key::ArrowDown);
    }
    let (pass, (painted, left)) =
        under_its_first_pass(&mut fx, key_events(Key::Enter, Modifiers::NONE));
    assert!(
        !painted.is_empty(),
        "the control: graph text under the box in the panel's own painting"
    );
    assert!(
        matches!(fx.view().edit_draft(), Some(Edit::SplitStop { .. })),
        "{:?}",
        fx.view().edit_draft()
    );
    assert!(fx.view().quick.is_some(), "a quick edit's dialog");
    assert_eq!(pass, 0, "shown in the pass that chose it");
    assert_eq!(left, [], "left under the Split dialog");
}
