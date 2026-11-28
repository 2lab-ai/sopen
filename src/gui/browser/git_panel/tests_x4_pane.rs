//! X4-pane's tests (10 §17): the details pane and the banner in cells (03 §6.1, §8.5) — the
//! summary, the files and the message wrapped by cells, the pane's bar and scroll bar; the
//! banner's rows and its `[ Continue ]`-style buttons, a click the step's key.

use super::in_progress::{BannerView, EditStop, Pressed, StopEnd, StopPressed};
use super::tests::{day, fixture_view, oid, Fx};
use super::*;
use crate::git::actions::{Action, Proceed};
use crate::git::rebase::Edit;
use crate::gui::browser::ops::dialog;
use crate::gui::cells::{Layer, Seen};
use crate::gui::chrome::git::LOOK;
use crate::gui::chrome::PALETTE;
use crate::gui::testkit;
use eframe::egui::{Event, Modifiers, PointerButton, Pos2, Vec2};
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Reading the panel's cells.

/// The panel's grid: the one its host laid over the window's central area.
fn grid_of(fx: &Fx) -> Grid {
    Grid::over(fx.host.rect)
}

/// The panel's layer of cells: the one over all of its side.
fn panel(fx: &Fx) -> Arc<Layer> {
    let grid = grid_of(fx);
    cells::layers(&fx.drv.ctx)
        .into_iter()
        .find(|l| l.buf.area == grid.all() && l.origin == grid.origin)
        .expect("the panel's cells over its side")
}

/// Row `y` of the panel from column `from` to `to` (exclusive), as text (a wide glyph's second
/// cell adds nothing).
fn text(fx: &Fx, y: u16, from: u16, to: u16) -> String {
    let rows = panel(fx).seen();
    rows[usize::from(y)][usize::from(from)..usize::from(to)]
        .iter()
        .map(|c| c.symbol.as_str())
        .collect()
}

/// What row `y` shows from column `x` over as many cells as `like` takes.
fn read(fx: &Fx, x: u16, y: u16, like: &str) -> String {
    text(fx, y, x, x + cells::width(like) as u16)
}

/// The cell at column `x` of row `y`.
fn cell(fx: &Fx, x: u16, y: u16) -> Seen {
    panel(fx).seen()[usize::from(y)][usize::from(x)].clone()
}

/// The centre of cell (`x`, `y`) of the panel's grid, in points.
fn centre(fx: &Fx, x: u16, y: u16) -> Pos2 {
    grid_of(fx).rect(Cells::new(x, y, 1, 1)).center()
}

/// A press and a release of the primary button at `p`, as one pass's events.
fn click_events(p: Pos2) -> Vec<Event> {
    let button_at = |pressed| Event::PointerButton {
        pos: p,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(p), button_at(true), button_at(false)]
}

/// The pane's cells in a panel over the whole grid with no banner: the rows under the graph's
/// rule (the graph keeping half the body, 8 rows at least), inside the frame, above the foot's
/// rule (`Layout::of`).
fn pane_cells(fx: &Fx) -> Cells {
    let g = grid_of(fx);
    let total = g.rows - 5;
    let graph = (total / 2).max(8).min(total - 2);
    let rule = 2 + graph;
    Cells::new(1, rule + 1, g.cols - 2, g.rows - 3 - (rule + 1))
}

/// The hand-off boxes a terminal would show over the panel's grid.
fn boxes(fx: &Fx) -> Vec<Cells> {
    let g = grid_of(fx);
    let layers = cells::layers(&fx.drv.ctx);
    let screen = crate::gui::term::compose(&fx.drv.ctx, &layers, g.cols, g.rows);
    (screen.hand_offs.into_iter())
        .filter(|r| r.y < g.rows)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The details read the tests hand the pane.

/// A changed file of a details read.
fn file(letter: char, path: &str, old: Option<&str>, lines: Option<(u64, u64)>) -> FileRow {
    FileRow {
        letter,
        path: path.into(),
        old_path: old.map(Into::into),
        lines,
        lossy: false,
    }
}

/// The merge's details (the fixture's row 1): a Hangul author and message, a committer of its
/// own, `In` answered, an added file, a renamed one with a Hangul name and a binary one.
fn merge_details(message: &str) -> Details {
    Details {
        subject: Subject::Row(RowKey::Commit(oid(9))),
        summary: Some(Summary {
            oid: oid(9),
            parents: vec![oid(7), oid(8)],
            author_name: "김철수".into(),
            author_email: "kim@example.com".into(),
            author_time: day(9),
            committer_name: "Dee".into(),
            committer_email: "dee@example.com".into(),
            commit_time: day(9) + 60,
            message: message.into(),
        }),
        contains: Some(vec!["main".into(), "origin/main".into()]),
        groups: vec![FileGroup {
            group: Group::Files,
            files: vec![
                file('A', "new.txt", None, Some((3, 0))),
                file('R', "docs/새 이름.md", Some("docs/old.md"), Some((1, 1))),
                file('M', "logo.png", None, None),
            ],
        }],
        error: None,
    }
}

const MERGE_MESSAGE: &str = "Merge branch 'feature'\n\n기능 브랜치를 합칩니다.\n";

/// The rename's row as the pane shows it.
const RENAMED: &str = "docs/old.md → docs/새 이름.md";

/// The fixture in a `w`×`h` window, the pane open on the merge with `details`.
fn with_pane(w: f32, h: f32, details: Details) -> Fx {
    let mut fx = Fx::new(w, h, fixture_view());
    fx.press(Key::ArrowDown);
    let asked = fx.press(Key::Enter);
    assert_eq!(asked, [GitAction::Details(details.subject.clone())]);
    fx.view_mut().set_details(details);
    fx.idle();
    fx
}

// ---------------------------------------------------------------------------------------------
// The pane (03 §6.1).

/// 03 §6.1: the pane is cells under the graph's rule — each field's name dim at its first cell
/// and its value at the 14th, the parents a line each, the message's lines, the group's head dim,
/// each file's change letter, its path (a rename `old → new`) and `+adds -dels` right-aligned at
/// the pane's end (blank for a binary file); the bar on the first file in the bar's colours; the
/// scroll bar on the frame's right border beside the pane. No hand-off box is left.
#[test]
fn the_pane_is_cells_under_the_rule() {
    let fx = with_pane(1000.0, 800.0, merge_details(MERGE_MESSAGE));
    let p = pane_cells(&fx);
    let (x, v) = (p.x, p.x + 13);
    let row = |k: u16| p.y + k;
    assert_eq!(read(&fx, x, row(0), "Commit"), "Commit");
    assert_eq!(
        cell(&fx, x, row(0)).fg,
        Some(LOOK.dim),
        "a field's name dim"
    );
    assert_eq!(read(&fx, v, row(0), oid(9).as_str()), oid(9).as_str());
    assert_eq!(cell(&fx, v, row(0)).fg, Some(LOOK.text));
    assert_eq!(text(&fx, row(1), x, v), "Parents      ");
    assert_eq!(read(&fx, v, row(1), oid(7).as_str()), oid(7).as_str());
    assert_eq!(
        text(&fx, row(2), x, v).trim(),
        "",
        "the second parent's line"
    );
    assert_eq!(read(&fx, v, row(2), oid(8).as_str()), oid(8).as_str());
    // A Hangul glyph is two cells: the value's cells, not its characters.
    let author = "김철수 <kim@example.com>";
    assert_eq!(text(&fx, row(3), x, v), "Author       ");
    assert_eq!(read(&fx, v, row(3), author), author);
    assert_eq!(read(&fx, x, row(4), "Author date"), "Author date");
    let date = super::tests::date_of(day(9));
    assert_eq!(read(&fx, v, row(4), &date), date);
    let committer = "Dee <dee@example.com>";
    assert_eq!(read(&fx, v, row(5), committer), committer);
    assert_eq!(read(&fx, x, row(6), "Commit date"), "Commit date");
    assert_eq!(read(&fx, x, row(7), "In"), "In");
    assert_eq!(
        read(&fx, v, row(7), "main, origin/main"),
        "main, origin/main"
    );
    assert_eq!(text(&fx, row(8), x, p.right()).trim(), "");
    let first = "Merge branch 'feature'";
    assert_eq!(read(&fx, x, row(9), first), first);
    let hangul = "기능 브랜치를 합칩니다.";
    assert_eq!(read(&fx, x, row(11), hangul), hangul);
    assert_eq!(read(&fx, x, row(13), "Files"), "Files");
    assert_eq!(
        cell(&fx, x, row(13)).fg,
        Some(LOOK.dim),
        "a group's head dim"
    );
    // The files: the letter at the third cell, the path at the fifth, the counts at the end.
    let end = p.right();
    assert_eq!(read(&fx, x, row(14), "  A new.txt"), "  A new.txt");
    assert_eq!(text(&fx, row(14), end - 5, end), "+3 -0");
    assert_eq!(read(&fx, x + 2, row(15), "R"), "R");
    assert_eq!(read(&fx, x + 4, row(15), RENAMED), RENAMED);
    assert_eq!(text(&fx, row(15), end - 5, end), "+1 -1");
    assert_eq!(read(&fx, x + 4, row(16), "logo.png"), "logo.png");
    assert_eq!(
        text(&fx, row(16), x + 12, end).trim(),
        "",
        "a binary file: no counts"
    );
    // The bar on the first file, the keyboard the pane's: the bar's colours across the row.
    for c in [x, x + 2, x + 4, end - 1] {
        let s = cell(&fx, c, row(14));
        assert_eq!((s.fg, s.bg), (Some(LOOK.on_bar), Some(PALETTE.bar)), "{c}");
    }
    assert_eq!(cell(&fx, x + 4, row(15)).bg, Some(PALETTE.back));
    // The scroll bar on the border beside the pane's rows; the rule above them stays.
    assert_eq!(cell(&fx, end, p.y).symbol, "↑");
    assert_eq!(cell(&fx, end, p.bottom() - 1).symbol, "↓");
    assert_eq!(
        cell(&fx, end, p.y + 1).bg,
        Some(PALETTE.thumb),
        "nothing to scroll: the thumb rests at the top"
    );
    assert_eq!(cell(&fx, end, p.y - 1).symbol, "╢");
    assert_eq!(cell(&fx, 0, p.y).symbol, "║");
    assert_eq!(boxes(&fx), Vec::<Cells>::new(), "no hand-off box");
}

/// 03 §6.1: while the read runs the pane says `reading…` (dim); a read that failed says why in
/// the error colour; the uncommitted row's groups have their heads and an untracked folder its
/// `►`.
#[test]
fn reading_an_error_the_groups_and_a_folder() {
    let mut fx = Fx::new(1000.0, 800.0, fixture_view());
    fx.press(Key::F3);
    let p = pane_cells(&fx);
    assert_eq!(read(&fx, p.x, p.y, "reading…"), "reading…");
    // Its line under the pane's bar while the pane has the keyboard; dim once the graph has it.
    assert_eq!(cell(&fx, p.x, p.y).bg, Some(PALETTE.bar));
    fx.press(Key::Escape);
    assert_eq!(cell(&fx, p.x, p.y).fg, Some(LOOK.dim));
    let row = |letter, path: &str| file(letter, path, None, None);
    fx.view_mut().set_details(Details {
        subject: Subject::Row(RowKey::Uncommitted),
        summary: None,
        contains: None,
        groups: vec![
            FileGroup {
                group: Group::Staged,
                files: vec![row('M', "a.txt")],
            },
            FileGroup {
                group: Group::Untracked,
                files: vec![row('?', "dir/")],
            },
        ],
        error: Some("git status failed: exit 128".into()),
    });
    fx.idle();
    let failed = "git status failed: exit 128";
    assert_eq!(read(&fx, p.x, p.y, failed), failed);
    assert_eq!(cell(&fx, p.x, p.y).fg, Some(LOOK.error));
    assert_eq!(read(&fx, p.x, p.y + 1, "Staged"), "Staged");
    assert_eq!(read(&fx, p.x, p.y + 3, "Untracked"), "Untracked");
    assert_eq!(read(&fx, p.x, p.y + 4, "► ? dir/"), "► ? dir/");
}

/// 03 §6.1: a message line longer than the pane wraps by cells at its last blank that fits — a
/// Hangul glyph two cells — as `dialog::wrap` breaks a dialog's text, never by the font's measure
/// (which lays 7 Hangul glyphs in 12 cells, 10 §10); no row runs past the pane.
#[test]
fn the_message_wraps_by_cells_at_a_blank() {
    let line = "한글 낱말과 English words 가 섞인 아주 긴 줄입니다 ".repeat(6);
    let line = line.trim_end();
    let mut fx = with_pane(400.0, 800.0, merge_details(&format!("Merge\n\n{line}\n")));
    fx.press(Key::Home);
    let p = pane_cells(&fx);
    let room = usize::from(p.width);
    let want: Vec<String> = (dialog::wrap(line, room).into_iter())
        .map(|r| r.trim_end().to_string())
        .collect();
    assert!(want.len() >= 4, "{want:?}");
    // The message's first line at row 9 (as above), a blank, then the long line's rows.
    let shown: Vec<String> = (0..want.len() as u16)
        .map(|k| {
            text(&fx, p.y + 11 + k, p.x, p.right())
                .trim_end()
                .to_string()
        })
        .collect();
    assert_eq!(shown, want);
    for r in &shown {
        assert!(cells::width(r) <= room, "{r:?}");
    }
}

/// 03 §6.1: a tab goes to the next eighth cell counted in cells as the row shows them, a
/// grapheme at a time — a Hangul glyph two, an emoji's sequence (a ZWJ sequence, a heart with its
/// presentation selector) the two cells it is laid in, an escaped control character its escape's
/// length.
#[test]
fn a_tab_after_hangul_stops_at_the_eighth_cell() {
    let message = "M\n\n한\tx\n\u{1}\ty\n👩\u{200d}💻\tx\n❤\u{fe0f}\tx\n";
    let fx = with_pane(1000.0, 800.0, merge_details(message));
    let p = pane_cells(&fx);
    assert_eq!(text(&fx, p.y + 11, p.x, p.x + 9), "한      x");
    assert_eq!(text(&fx, p.y + 12, p.x, p.x + 9), "\\u{1}   y");
    for (k, glyph) in [(13, "👩\u{200d}💻"), (14, "❤\u{fe0f}")] {
        assert_eq!(cell(&fx, p.x, p.y + k).symbol, glyph);
        assert_eq!(
            cells::width(glyph),
            2,
            "the control: {glyph:?} is two cells"
        );
        assert_eq!(cell(&fx, p.x + 8, p.y + k).symbol, "x", "{glyph:?}");
        assert_eq!(
            text(&fx, p.y + k, p.x + 2, p.x + 8),
            " ".repeat(6),
            "{glyph:?}"
        );
    }
}

/// The pane's bar is in the active bar's colours only while the panel and the pane have the
/// keyboard: idle (black, the text its own colour) when the panel has not, none once Esc gave the
/// keyboard back to the graph.
#[test]
fn the_pane_bar_follows_the_keyboard() {
    let mut fx = with_pane(1000.0, 800.0, merge_details(MERGE_MESSAGE));
    let p = pane_cells(&fx);
    let first = p.y + 14;
    fx.host.active = false;
    fx.idle();
    let s = cell(&fx, p.x + 4, first);
    assert_eq!((s.fg, s.bg), (Some(LOOK.text), Some(PALETTE.bar_idle)));
    fx.host.active = true;
    fx.press(Key::Escape);
    assert!(fx.view().details_open());
    assert_eq!(cell(&fx, p.x + 4, first).bg, Some(PALETTE.back), "no bar");
}

/// A long pane scrolls by cells: the wheel over it — a point delta a row each 16 points, a line
/// delta three rows, two in one pass adding up — moves its rows and the thumb, the bar staying on
/// its line; a key moving the bar off screen scrolls it back into view.
#[test]
fn the_wheel_and_the_keys_scroll_the_pane_by_cells() {
    let message: String = (0..60).map(|k| format!("line {k:02}\n")).collect();
    let mut details = merge_details(&message);
    if let Some(s) = details.summary.as_mut() {
        s.committer_name = s.author_name.clone();
        s.committer_email = s.author_email.clone();
        s.commit_time = s.author_time;
    }
    let mut fx = with_pane(1000.0, 800.0, details);
    let p = pane_cells(&fx);
    fx.press(Key::Home);
    assert_eq!(read(&fx, p.x, p.y, "Commit"), "Commit");
    // Commit, Parents (2), Author, Author date, In, a blank: the message from line 7.
    assert_eq!(read(&fx, p.x, p.y + 7, "line 00"), "line 00");
    // More lines than rows, scrolled to the top: the thumb at the top of the track.
    assert!(fx.view().pane.as_ref().unwrap().lines.len() > usize::from(p.height));
    let thumb_at = |fx: &Fx| {
        (p.y + 1..p.bottom() - 1).find(|&y| cell(fx, p.right(), y).bg == Some(PALETTE.thumb))
    };
    assert_eq!(thumb_at(&fx), Some(p.y + 1));
    let over = centre(&fx, 40, p.y + 3);
    let wheel = |unit, dy: f32| Event::MouseWheel {
        unit,
        delta: Vec2::new(0.0, dy),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    };
    let events = vec![
        Event::PointerMoved(over),
        wheel(egui::MouseWheelUnit::Point, -32.0),
        wheel(egui::MouseWheelUnit::Line, -1.0),
    ];
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(
        read(&fx, p.x, p.y + 2, "line 00"),
        "line 00",
        "2 rows and 3"
    );
    let bar = |fx: &Fx| fx.view().pane.as_ref().unwrap().scroll.bar;
    assert_eq!(bar(&fx), 0, "the bar stays");
    let moved = thumb_at(&fx).expect("a thumb");
    assert!(moved > p.y + 1, "the thumb moved down: {moved}");
    // The bar, off screen, scrolled back into view by a key.
    fx.press(Key::ArrowDown);
    assert_eq!(bar(&fx), 1);
    assert_eq!(read(&fx, p.x, p.y, "Parents"), "Parents");
    assert_eq!(
        cell(&fx, p.x, p.y).bg,
        Some(PALETTE.bar),
        "the bar on its row"
    );
    // At the end, the thumb at the track's foot.
    fx.press(Key::End);
    assert_eq!(thumb_at(&fx), Some(p.bottom() - 2));
}

/// Clicks by cells: a click on a line of the pane puts its bar there and gives it the keyboard;
/// two clicks in one pass, the line under the pointer at the pass's end (egui hit-tests the
/// pass's press there); a double click on a file right after a click on another line is Enter
/// there (its diff, the browser's `second_click`).
#[test]
fn clicks_and_double_clicks_on_the_pane_go_by_cells() {
    let mut fx = with_pane(1000.0, 800.0, merge_details(MERGE_MESSAGE));
    let p = pane_cells(&fx);
    fx.press(Key::Escape);
    let keyboard = |fx: &Fx| fx.view().pane.as_ref().unwrap().keyboard;
    let line = |fx: &Fx| fx.view().pane.as_ref().unwrap().scroll.bar;
    assert!(!keyboard(&fx), "the graph's keyboard");
    let at = centre(&fx, 40, p.y + 3);
    fx.drv
        .frame(&mut fx.host, click_events(at), Modifiers::NONE);
    fx.idle();
    assert!(keyboard(&fx), "the pane's keyboard");
    assert_eq!(line(&fx), 3);
    assert_eq!(cell(&fx, 40, p.y + 3).bg, Some(PALETTE.bar));
    let mut events = click_events(centre(&fx, 40, p.y + 5));
    events.extend(click_events(centre(&fx, 40, p.y + 7)));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(line(&fx), 7, "the line under the pointer at the pass's end");
    fx.take();
    // Past egui's double-click time since the clicks above.
    fx.drv.idle(&mut fx.host, 30);
    let new = centre(&fx, 40, p.y + 14);
    fx.drv.click(&mut fx.host, new);
    assert!(fx.take().is_empty(), "one click on new.txt");
    let renamed = centre(&fx, 40, p.y + 15);
    fx.drv.click(&mut fx.host, renamed);
    assert!(fx.take().is_empty(), "the control: one click on the rename");
    fx.drv.click(&mut fx.host, renamed);
    fx.idle();
    let asked = fx.take();
    assert!(
        matches!(&asked[..], [GitAction::Diff { file, .. }] if file.path == "docs/새 이름.md"),
        "{asked:?}"
    );
}

/// 10 §10, T38: in an ambiguous-wide terminal the pane's own `√` and a rename's `→` are §10's
/// stand-ins, at the source; a `→` in a path is content and stays.
#[test]
fn an_ambiguous_wide_terminal_gets_the_stand_ins_of_the_panes_own_glyphs() {
    let mut details = merge_details(MERGE_MESSAGE);
    details.groups[0].files[0] = file('M', "a→b.txt", None, Some((1, 0)));
    let mut fx = with_pane(1000.0, 800.0, details);
    let p = pane_cells(&fx);
    // Enter on the first file: its diff asked, the file viewed.
    fx.press(Key::Enter);
    fx.idle();
    assert_eq!(
        read(&fx, p.x, p.y + 14, "√ M a→b.txt"),
        "√ M a→b.txt",
        "the control"
    );
    // On the active bar the mark keeps its colour, as the graph's lanes and labels keep theirs.
    let mark = cell(&fx, p.x, p.y + 14);
    assert_eq!((mark.fg, mark.bg), (Some(LOOK.viewed), Some(PALETTE.bar)));
    let letter = cell(&fx, p.x + 2, p.y + 14);
    assert_eq!(
        (letter.fg, letter.bg),
        (Some(LOOK.on_bar), Some(PALETTE.bar))
    );
    assert_eq!(read(&fx, p.x + 4, p.y + 15, RENAMED), RENAMED);
    cells::set_ambiguous_wide(&fx.drv.ctx, true);
    fx.idle();
    assert_eq!(read(&fx, p.x, p.y + 14, ". M a→b.txt"), ". M a→b.txt");
    let stand_in = "docs/old.md > docs/새 이름.md";
    assert_eq!(read(&fx, p.x + 4, p.y + 15, stand_in), stand_in);
    cells::set_ambiguous_wide(&fx.drv.ctx, false);
}

/// The cells under a dialog of the list's are blank in the pane too (the panel blanks them over
/// every part's cells).
#[test]
fn the_pane_is_blank_under_a_dialog() {
    let mut fx = with_pane(1000.0, 800.0, merge_details(MERGE_MESSAGE));
    let p = pane_cells(&fx);
    assert_eq!(read(&fx, p.x, p.y, "Commit"), "Commit", "the control");
    fx.host.under = vec![Cells::new(p.x, p.y, 20, 3)];
    fx.idle();
    assert_eq!(text(&fx, p.y, p.x, p.x + 20), " ".repeat(20));
    assert_eq!(cell(&fx, p.x, p.y).bg, Some(PALETTE.back));
}

/// A value longer than the pane — `In` naming many branches — is cut by cells with `…` at the
/// pane's end, and a path longer than its room keeps its end after `…`.
#[test]
fn long_values_and_paths_are_cut_by_cells() {
    let mut details = merge_details(MERGE_MESSAGE);
    details.contains = Some((0..2_000).map(|k| format!("기능-{k}")).collect());
    let long = format!("{}/끝.txt", "가".repeat(80));
    details.groups[0].files[0] = file('M', &long, None, Some((12, 3)));
    let fx = with_pane(400.0, 800.0, details);
    let p = pane_cells(&fx);
    let shown = text(&fx, p.y + 7, p.x + 13, p.right());
    assert!(shown.starts_with("기능-0, 기능-1"), "{shown:?}");
    assert!(shown.trim_end().ends_with('…'), "{shown:?}");
    let path = text(&fx, p.y + 14, p.x + 4, p.right());
    assert!(path.starts_with('…'), "{path:?}");
    assert!(path.contains("/끝.txt"), "{path:?}");
    assert!(path.trim_end().ends_with("+12 -3"), "{path:?}");
}

/// The message's rows are `dialog::wrap`'s, made in one walk over the line: each grapheme is
/// measured about once — `dialog::wrap` measures what is left at each row, so a one-line message
/// near 03 §6.1's bound on the read (1 MiB) took minutes to wrap (more than 90 s for 1.5 MiB in a
/// debug build, the RED run).
#[test]
fn the_wrap_is_dialog_wraps_in_one_walk() {
    use unicode_segmentation::UnicodeSegmentation;
    let texts = [
        "",
        " ",
        "a",
        "ab cd",
        "  lead and  two   blanks  ",
        "longwordwithoutanyblank and more",
        "한글 낱말과 English 가 섞인 줄",
        "가나다라마바사아자차카타파하",
        "x y",
        "trailing   ",
        "a\u{3000}b c",
    ];
    for t in texts {
        for room in 0..20 {
            assert_eq!(
                details::wrap_cells(t, room),
                dialog::wrap(t, room),
                "{t:?} at {room}"
            );
        }
    }
    for (text, room) in [("단어 word ".repeat(1 << 12), 80), ("x".repeat(50_000), 7)] {
        let n = text.graphemes(true).count();
        details::WALKED.with(|c| c.set(0));
        let rows = details::wrap_cells(&text, room);
        let walked = details::WALKED.with(|c| c.get());
        assert!(rows.len() > n / (room + 1), "{}", rows.len());
        assert!(
            (n..=2 * n + rows.len()).contains(&walked),
            "{walked} graphemes measured for {n}"
        );
    }
}

/// A value cut by cells costs its room, however long it is: `In` naming a million-cell list of
/// refs measures the graphemes up to the first past the room, and shows what `cells::fit_start`
/// shows of the whole.
#[test]
fn a_long_value_costs_its_room_only() {
    let long = "기능, ".repeat(50_000);
    for room in [0, 1, 7, 30] {
        details::WALKED.with(|c| c.set(0));
        let shown = details::fitted(&long, room);
        let walked = details::WALKED.with(|c| c.get());
        assert_eq!(shown, cells::fit_start(&long, room), "{room}");
        assert!(
            walked <= room + 1,
            "{walked} graphemes measured for {room} cells"
        );
    }
    assert_eq!(details::fitted("short", 30), "short");
    assert_eq!(details::fitted("a\tb", 30), "a\\tb", "escaped");
}

// ---------------------------------------------------------------------------------------------
// The banner (03 §8.5).

/// 03 §8.5: the banner is the panel's top rows in cells, in the warning colour, its offered
/// steps `[ Continue ]`-style buttons right-aligned on its first row, a blank of the panel's back
/// apart, in the dialog's colours (as the hex view's buttons outside a dialog) — each its own
/// text; its second line (bisect) under it; the heads' row under the banner. No hand-off box is
/// left.
#[test]
fn the_banner_is_cells_with_its_buttons() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let right = grid_of(&fx).cols - 1;
    let state = "MERGE of feature · 1 conflict";
    fx.view_mut().banner = Some(BannerView::new(
        state,
        vec![Proceed::Continue, Proceed::Abort],
    ));
    fx.idle();
    assert_eq!(read(&fx, 1, 1, state), state);
    let s = cell(&fx, 1, 1);
    assert_eq!((s.fg, s.bg), (Some(PALETTE.warn), Some(PALETTE.back)));
    let buttons = "[ Continue ] [ Abort ]";
    let at = right - buttons.len() as u16;
    assert_eq!(text(&fx, 1, at, right), buttons);
    let b = cell(&fx, at, 1);
    assert_eq!(
        (b.fg, b.bg),
        (Some(PALETTE.dialog_text), Some(PALETTE.dialog_back))
    );
    assert_eq!(
        cell(&fx, at + 12, 1).bg,
        Some(PALETTE.back),
        "a blank apart"
    );
    assert!(
        fx.shows("[ Continue ]") && fx.shows("[ Abort ]"),
        "each its own text"
    );
    assert!(
        text(&fx, 2, 1, right).contains("Description"),
        "the heads under it"
    );
    assert_eq!(boxes(&fx), Vec::<Cells>::new(), "no hand-off box");
    // Two lines: bisect on its own; the buttons on the first.
    fx.view_mut().banner = Some(BannerView::new(
        "REBASE 3/7 · stopped at abcdef12 fix · 2 conflicts\nBISECT · Reset",
        vec![
            Proceed::Continue,
            Proceed::Skip,
            Proceed::Abort,
            Proceed::BisectReset,
        ],
    ));
    fx.idle();
    let all = "[ Continue ] [ Skip ] [ Abort ] [ Reset ]";
    assert_eq!(text(&fx, 1, right - all.len() as u16, right), all);
    assert_eq!(read(&fx, 1, 2, "BISECT · Reset"), "BISECT · Reset");
    assert_eq!(cell(&fx, 2, 2).fg, Some(PALETTE.warn));
    assert!(text(&fx, 3, 1, right).contains("Description"));
}

/// 03 §8.5: an edit stop's banner has `[ Amend ]` and `[ Split ]` before the banner's steps. The
/// buttons come first, as egui's did: on a narrow panel the state is cut with `…` a cell before
/// the first button, and only buttons that do not fit at all go, from the left.
#[test]
fn an_edit_stops_banner_has_amend_and_split_first() {
    let c = Oid::parse(&"c".repeat(40)).unwrap();
    let stopped = "REBASE 2/3 · stopped to edit cccccccc third";
    let view = || {
        let mut view = fixture_view();
        view.banner = Some(BannerView::new(
            stopped,
            vec![Proceed::Continue, Proceed::Abort],
        ));
        view.stop = Some(EditStop::new("cccccccc", Some(c.clone())));
        view
    };
    let fx = Fx::new(1000.0, 500.0, view());
    let right = grid_of(&fx).cols - 1;
    let all = "[ Amend ] [ Split ] [ Continue ] [ Abort ]";
    assert_eq!(text(&fx, 1, right - all.len() as u16, right), all);
    assert_eq!(read(&fx, 1, 1, stopped), stopped);
    // 60 cells inside (about a side of a 1000-point window): all four (42 cells) and the state
    // cut to the 17 cells before their blank.
    let fx = Fx::new(62.0 * 8.0, 500.0, view());
    let right = grid_of(&fx).cols - 1;
    let at = right - all.len() as u16;
    assert_eq!(text(&fx, 1, at, right), all);
    assert_eq!(text(&fx, 1, 1, at), "REBASE 2/3 · sto… ");
    // 30 cells inside: `[ Split ]` and `[ Amend ]` would start before the row and go; the state
    // keeps the 7 cells left.
    let fx = Fx::new(32.0 * 8.0, 500.0, view());
    let right = grid_of(&fx).cols - 1;
    let two = "[ Continue ] [ Abort ]";
    let at = right - two.len() as u16;
    assert_eq!(text(&fx, 1, at, right), two);
    assert_eq!(text(&fx, 1, 1, at), "REBASE… ");
}

/// 03 §8.5: a click on a button is its step's key: the banner's steps give S3-B's `Pressed`, an
/// edit stop's Amend and Split S4-B's; a key and a click in one pass give both, the key first
/// (it is read before the pass draws); two clicks in one pass, the button under the pointer at
/// the pass's end (egui hit-tests the pass's press there).
#[test]
fn a_click_on_a_button_is_its_steps_key() {
    use super::tests_s3::{Fx as S3, Host as S3Host};
    let mut fx = S3::new(S3Host {
        banner: Some(BannerView::new(
            "MERGE of feature · 1 conflict",
            vec![Proceed::Continue, Proceed::Abort],
        )),
        ..S3Host::default()
    });
    fx.click("[ Abort ]");
    assert_eq!(
        fx.host.pressed,
        [Pressed::Proceed(Action::Proceed(Proceed::Abort))]
    );
    fx.host.pressed.clear();
    let at = |fx: &S3, text: &str| fx.drv.find_text(text).expect(text).center();
    let continue_at = at(&fx, "[ Continue ]");
    let abort_at = at(&fx, "[ Abort ]");
    let key = |pressed| Event::Key {
        key: Key::A,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: testkit::command(true),
    };
    let mut events = vec![key(true), key(false)];
    events.extend(click_events(continue_at));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(
        fx.host.pressed,
        [
            Pressed::Proceed(Action::Proceed(Proceed::Abort)),
            Pressed::Proceed(Action::Proceed(Proceed::Continue)),
        ],
        "the key, then the click"
    );
    fx.host.pressed.clear();
    let mut events = click_events(continue_at);
    events.extend(click_events(abort_at));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(
        fx.host.pressed,
        [Pressed::Proceed(Action::Proceed(Proceed::Abort))],
        "the button under the pointer at the pass's end"
    );

    use super::tests_s4::{Fx as S4, Host as S4Host};
    let c = Oid::parse(&"c".repeat(40)).unwrap();
    let mut fx = S4::new(S4Host {
        banner: Some(BannerView::new(
            "REBASE 2/3 · stopped to edit cccccccc third",
            vec![Proceed::Continue, Proceed::Abort],
        )),
        stop: Some(EditStop::new("cccccccc", Some(c.clone()))),
        ..S4Host::default()
    });
    fx.click("[ Split ]");
    let split = StopPressed::Split(Edit::SplitStop { commit: c });
    assert_eq!(fx.host.stop_ends, [StopEnd::Stop(split)]);
    fx.host.stop_ends.clear();
    fx.click("[ Amend ]");
    assert_eq!(fx.host.stop_ends, [StopEnd::Stop(StopPressed::Amend)]);
    fx.host.stop_ends.clear();
    fx.click("[ Continue ]");
    assert_eq!(
        fx.host.stop_ends,
        [StopEnd::Banner(Pressed::Proceed(Action::Proceed(
            Proceed::Continue
        )))]
    );
}

/// In the panel, a click on a banner's button goes through its step's dialog as its key does
/// (Abort's confirmation).
#[test]
fn the_panels_banner_button_opens_the_steps_dialog() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.view_mut().set_repo(super::tests_s3::repo());
    fx.view_mut().banner = Some(BannerView::new(
        "MERGE of feature · 1 conflict",
        vec![Proceed::Continue, Proceed::Abort],
    ));
    fx.idle();
    let abort = fx.drv.find_text("[ Abort ]").expect("the button").center();
    fx.drv.click(&mut fx.host, abort);
    fx.idle();
    fx.idle();
    assert!(fx.view().dialog_up(), "Abort's dialog");
}

// ---------------------------------------------------------------------------------------------
// Review round 1.

/// Two banners, or two panes, side by side in one `ui` — two repositories' panels, a pin beside
/// the other side's own (`browser.rs` `panels_ui`) — each at its side's cells, what each gave.
struct TwoSides {
    banners: Option<[BannerView; 2]>,
    panes: Option<[details::Pane; 2]>,
    pressed: Vec<(usize, Pressed)>,
    clicked: Vec<(usize, usize, bool)>,
}

impl crate::gui::Window for TwoSides {
    fn ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let grid = Grid::over(ui.max_rect());
                let mut buf = cells::Buffer::empty(grid.all());
                let sides = cells::halves(grid.all());
                if let Some(banners) = &self.banners {
                    for (k, (side, b)) in sides.iter().zip(banners).enumerate() {
                        let top = Cells::new(side.x, side.y, side.width, 1);
                        // Each side its own panel's key, as two panels' are.
                        let panel = egui::Id::unique(("sopen-git-test-panel", k));
                        if let Some(p) = in_progress::draw(ui, &grid, top, &mut buf, b, panel) {
                            self.pressed.push((k, p));
                        }
                    }
                }
                if let Some(panes) = &mut self.panes {
                    for (k, (side, pane)) in sides.iter().zip(panes).enumerate() {
                        // The border column right of the pane, for its scroll bar.
                        let area = Cells::new(side.x, side.y, side.width - 1, side.height);
                        let seen = |_: &FileRow| false;
                        if let Some((line, twice)) =
                            details::draw(ui, &grid, area, &mut buf, pane, &seen, true)
                        {
                            self.clicked.push((k, line, twice));
                        }
                    }
                }
                let painter = ui.painter_at(grid.rect(grid.all()));
                cells::paint(&painter, grid.origin, &buf, None);
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// The centre of the cell `x` cells from the left of row `y`, in a window of the kit's grid.
fn at_cell(x: u16, y: u16) -> Pos2 {
    Grid::over(egui::Rect::from_min_size(
        Pos2::ZERO,
        Vec2::new(800.0, 400.0),
    ))
    .rect(Cells::new(x, y, 1, 1))
    .center()
}

/// R1 MUST 1: two panels' banners in one `ui` are two sets of buttons — a click on the right
/// panel's `[ Abort ]` is that panel's Abort alone, a click on the left one's the left's (the
/// buttons' ids were the same in both, so egui kept the right ones' rects for both: the left
/// buttons were dead, and a click on the right Abort was Abort in both repositories).
#[test]
fn two_panels_banners_take_their_own_clicks() {
    let banner = || {
        BannerView::new(
            "MERGE of feature · 1 conflict",
            vec![Proceed::Continue, Proceed::Abort],
        )
    };
    let mut win = TwoSides {
        banners: Some([banner(), banner()]),
        panes: None,
        pressed: Vec::new(),
        clicked: Vec::new(),
    };
    let mut drv = crate::gui::testkit::Driver::new(800.0, 400.0);
    drv.idle(&mut win, 2);
    // 100 cells: the left side 0..50, the right 50..100; each `[ Abort ]` its side's last 9.
    let abort = Pressed::Proceed(Action::Proceed(Proceed::Abort));
    drv.click(&mut win, at_cell(100 - 5, 0));
    assert_eq!(win.pressed, [(1, abort.clone())], "the right panel's alone");
    win.pressed.clear();
    drv.idle(&mut win, 30);
    drv.click(&mut win, at_cell(50 - 5, 0));
    assert_eq!(win.pressed, [(0, abort)], "the left panel's alone");
}

/// R1 MUST 1: two panels' details panes in one `ui` take their own clicks — a line's click, and a
/// double click's pair (both panes show the uncommitted row: one subject).
#[test]
fn two_panes_take_their_own_clicks() {
    let pane = || {
        let subject = Subject::Row(RowKey::Uncommitted);
        let mut pane = details::Pane::new(subject.clone(), true, Vec::new());
        pane.set(Details {
            subject,
            summary: None,
            contains: None,
            groups: vec![FileGroup {
                group: Group::NotStaged,
                files: (0..6)
                    .map(|k| file('M', &format!("f{k}.txt"), None, None))
                    .collect(),
            }],
            error: None,
        });
        pane
    };
    let mut win = TwoSides {
        banners: None,
        panes: Some([pane(), pane()]),
        pressed: Vec::new(),
        clicked: Vec::new(),
    };
    let mut drv = crate::gui::testkit::Driver::new(800.0, 400.0);
    drv.idle(&mut win, 2);
    drv.click(&mut win, at_cell(70, 3));
    assert_eq!(win.clicked, [(1, 3, false)], "the right pane's line alone");
    win.clicked.clear();
    // The same line on the left right after: a click of its own — a control, since egui counts
    // no double click across more than 6 points (`max_click_dist`), whatever the pair's key.
    drv.click(&mut win, at_cell(20, 3));
    assert_eq!(win.clicked, [(0, 3, false)], "the left pane's line, once");
    win.clicked.clear();
    drv.click(&mut win, at_cell(20, 3));
    assert_eq!(
        win.clicked,
        [(0, 3, true)],
        "the control: a double click on the left"
    );
}

/// R1 MUST 2: the banner's buttons never take the keyboard — Tab does not reach them, so Space
/// and Enter press nothing (egui's `Sense::click()` is focusable, and a focused widget's Space or
/// Enter is a click).
#[test]
fn the_banner_buttons_never_take_the_keyboard() {
    use super::tests_s3::{Fx as S3, Host as S3Host};
    let mut fx = S3::new(S3Host {
        banner: Some(BannerView::new(
            "MERGE of feature · 1 conflict",
            vec![Proceed::Continue, Proceed::Abort],
        )),
        ..S3Host::default()
    });
    for _ in 0..3 {
        fx.press(Key::Tab);
        assert_eq!(
            fx.drv.ctx.memory(|m| m.focused()),
            None,
            "nothing has the focus"
        );
    }
    fx.press(Key::Space);
    fx.press(Key::Enter);
    assert_eq!(fx.host.pressed, Vec::<Pressed>::new(), "no step pressed");
    fx.click("[ Abort ]");
    assert_eq!(
        fx.host.pressed,
        [Pressed::Proceed(Action::Proceed(Proceed::Abort))],
        "the control: a click"
    );
}

/// R1 S3: a click on the blank between two buttons presses neither — in a host with nothing
/// behind the banner too, where egui's slop would give it to the nearer button.
#[test]
fn a_click_between_two_buttons_presses_none() {
    use super::tests_s3::{Fx as S3, Host as S3Host};
    let mut fx = S3::new(S3Host {
        banner: Some(BannerView::new(
            "MERGE of feature · 1 conflict",
            vec![Proceed::Continue, Proceed::Abort],
        )),
        ..S3Host::default()
    });
    let abort = fx.drv.find_text("[ Abort ]").expect("the button");
    // The blank cell left of `[ Abort ]`, at its centre.
    let gap = Pos2::new(abort.left() - chrome::CELL.x / 2.0, abort.center().y);
    fx.drv.click(&mut fx.host, gap);
    fx.idle();
    assert_eq!(
        fx.host.pressed,
        Vec::<Pressed>::new(),
        "the blank presses nothing"
    );
    // On the state's text: nothing either — a control, the state being far past egui's 5-point
    // slop from any button.
    let state = fx.drv.find_text("MERGE of feature · 1 conflict").unwrap();
    fx.drv.idle(&mut fx.host, 30);
    fx.drv.click(&mut fx.host, state.center());
    assert_eq!(
        fx.host.pressed,
        Vec::<Pressed>::new(),
        "the state presses nothing"
    );
}

/// Fix check: a click on a pane's line pressed before the banner grows a row and released after
/// is that line's — the line's id holds the pane's first column alone, so the pane moving down
/// under the taller banner (`Layout::of`) keeps its ids, and egui gives the release to the id it
/// pressed (`interaction.rs:118-123, 174-181`; with its row in the id the click was dropped).
#[test]
fn a_pane_click_survives_the_banner_growing() {
    let mut view = fixture_view();
    let merge = |text: &str| BannerView::new(text, vec![Proceed::Continue, Proceed::Abort]);
    view.banner = Some(merge("MERGE of feature · 1 conflict"));
    let mut fx = Fx::new(1000.0, 800.0, view);
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    fx.view_mut().set_details(merge_details(MERGE_MESSAGE));
    fx.idle();
    fx.press(Key::Escape);
    let pane = |fx: &Fx| {
        let p = fx.view().pane.as_ref().unwrap();
        (p.keyboard, p.scroll.bar)
    };
    assert!(!pane(&fx).0, "the graph's keyboard");
    let at = fx.drv.find_text("Author date").expect("the line").center();
    fx.drv.move_to(&mut fx.host, at);
    fx.drv.press(&mut fx.host, PointerButton::Primary);
    fx.view_mut().banner = Some(merge("MERGE of feature · 1 conflict\nBISECT · Reset"));
    fx.drv.release(&mut fx.host, PointerButton::Primary);
    fx.idle();
    assert_ne!(
        fx.drv.find_text("Author date").unwrap().center(),
        at,
        "the control: the pane moved under the taller banner"
    );
    assert_eq!(pane(&fx), (true, 4), "the click is `Author date`'s");
}

/// Fix check (asked as the banner's case): a click on `[ Abort ]` pressed before the banner grows
/// a row and released after is one Abort. A control in the panel: the banner stands at the
/// frame's top whatever its rows (`Layout::of`), so its row never moved; its ids hold its first
/// column alone all the same.
#[test]
fn a_banner_click_survives_its_rows_changing() {
    use super::tests_s3::{Fx as S3, Host as S3Host};
    let merge = |text: &str| BannerView::new(text, vec![Proceed::Continue, Proceed::Abort]);
    let mut fx = S3::new(S3Host {
        banner: Some(merge("MERGE of feature · 1 conflict")),
        ..S3Host::default()
    });
    let at = fx.drv.find_text("[ Abort ]").expect("the button").center();
    fx.drv.move_to(&mut fx.host, at);
    fx.drv.press(&mut fx.host, PointerButton::Primary);
    fx.host.banner = Some(merge("MERGE of feature · 1 conflict\nBISECT · Reset"));
    fx.drv.release(&mut fx.host, PointerButton::Primary);
    fx.idle();
    assert_eq!(
        fx.host.pressed,
        [Pressed::Proceed(Action::Proceed(Proceed::Abort))]
    );
}

/// Fix check: the pane's lines never take the keyboard either — Tab reaches none of them, so
/// Space and Enter click none (the pane's keys are the panel's).
#[test]
fn the_pane_lines_never_take_the_keyboard() {
    let pane = {
        let subject = Subject::Row(RowKey::Uncommitted);
        let mut pane = details::Pane::new(subject.clone(), true, Vec::new());
        pane.set(Details {
            subject,
            summary: None,
            contains: None,
            groups: vec![FileGroup {
                group: Group::NotStaged,
                files: vec![file('M', "a.txt", None, None)],
            }],
            error: None,
        });
        pane
    };
    // The other side's pane its own panel's key, as two panels' are.
    let other = (pane.clone()).in_panel(egui::Id::unique(("sopen-git-test-panel", 1)));
    let mut win = TwoSides {
        banners: None,
        panes: Some([pane, other]),
        pressed: Vec::new(),
        clicked: Vec::new(),
    };
    let mut drv = crate::gui::testkit::Driver::new(800.0, 400.0);
    drv.idle(&mut win, 2);
    for _ in 0..3 {
        drv.key(&mut win, Key::Tab, Modifiers::NONE);
        assert_eq!(
            drv.ctx.memory(|m| m.focused()),
            None,
            "nothing has the focus"
        );
    }
    drv.key(&mut win, Key::Space, Modifiers::NONE);
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    assert_eq!(
        win.clicked,
        Vec::<(usize, usize, bool)>::new(),
        "no line clicked"
    );
}

/// R1 S2: a double click's two clicks are on one file, by its group and path — a refresh between
/// them that moves the files (`In` answered, the status read again) makes the second click a
/// click of its own on the file now there, never a double click opening it.
#[test]
fn a_double_click_pairs_a_file_by_its_path() {
    let with_files = |names: &[&str]| {
        let mut d = merge_details(MERGE_MESSAGE);
        d.groups[0].files = (names.iter())
            .map(|n| file('M', n, None, Some((1, 1))))
            .collect();
        d
    };
    let mut fx = with_pane(1000.0, 800.0, with_files(&["a.txt", "b.txt"]));
    let p = pane_cells(&fx);
    fx.take();
    fx.drv.idle(&mut fx.host, 30);
    let first = centre(&fx, 40, p.y + 14);
    fx.drv.click(&mut fx.host, first);
    assert_eq!(read(&fx, p.x + 4, p.y + 14, "a.txt"), "a.txt");
    // A file comes before it: line 14 is `new.txt` now.
    fx.view_mut()
        .set_details(with_files(&["new.txt", "a.txt", "b.txt"]));
    fx.drv.click(&mut fx.host, first);
    assert_eq!(read(&fx, p.x + 4, p.y + 14, "new.txt"), "new.txt");
    assert_eq!(
        fx.take(),
        Vec::<GitAction>::new(),
        "two clicks on two files: no diff"
    );
    // The control: two clicks on one file are its diff.
    fx.drv.idle(&mut fx.host, 30);
    fx.drv.click(&mut fx.host, first);
    fx.drv.click(&mut fx.host, first);
    let asked = fx.take();
    assert!(
        matches!(&asked[..], [GitAction::Diff { file, .. }] if file.path == "new.txt"),
        "{asked:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// TS-4 (10 §14): the pane and the banner in both backends.

mod ts4 {
    use super::super::tests::{history, refs, rows_of, Host};
    use super::*;
    use crate::gui::chrome::CELL;
    use crate::gui::term::{self, Backend, End, Probed, Receipt, Tty};
    use crate::gui::testkit::Driver;
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

    /// The fixture with the pane open on the merge (a Hangul message and author, a rename with a
    /// Hangul name), or with an edit stop's banner (`stop`).
    fn host(stop: bool) -> Host {
        let mut view = GitView::new("box:/srv/repo");
        view.set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
        view.set_rows(rows_of(history(), true), false, 2);
        view.set_paging(Paging::AllLoaded);
        if stop {
            view.banner = Some(BannerView::new(
                "REBASE 2/3 · stopped to edit cccccccc 세 번째",
                vec![Proceed::Continue, Proceed::Abort],
            ));
            let c = Oid::parse(&"c".repeat(40)).unwrap();
            view.stop = Some(EditStop::new("cccccccc", Some(c)));
        } else {
            assert!(view.select(RowKey::Commit(oid(9))));
            view.open_details();
            view.set_details(merge_details(MERGE_MESSAGE));
        }
        Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: Rect::NOTHING,
            under: Vec::new(),
            side: None,
        }
    }

    /// TS-4's pane and banner: the terminal's receipt and the window's are the same bytes — the
    /// pane on the merge at 100×40 and at 60×30, the banner at an edit stop at 100×30.
    #[test]
    fn ts4_the_pane_and_the_banner_are_the_same_cells_in_both_backends() {
        for (stop, cols, rows) in [(false, 100, 40), (false, 60, 30), (true, 100, 30)] {
            let mut tui = host(stop);
            let (terminal, when) = terminal_run(&mut tui, cols, rows);
            let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
            let mut win = host(stop);
            drv.idle(&mut win, 2);
            let in_window = window_receipt(&drv, &mut win, cols, rows, when);
            assert_eq!(terminal, in_window, "{stop} {cols}x{rows}");
            // At 60×30 the pane's 11 rows end with its bar's line, the first file: `Author
            // date` to `new.txt`.
            let shown: &[&str] = match (stop, cols) {
                (true, _) => &["stopped to edit cccccccc 세 번째", "[ Amend ] [ Split ]"],
                (false, 100) => &["기능 브랜치를 합칩니다.", "→ docs/새", "김철수 <kim@"],
                (false, _) => &["기능 브랜치를 합칩니다.", "A new.txt", "Dee <dee@"],
            };
            for s in shown {
                assert!(
                    terminal.contains(s),
                    "{stop} {cols}x{rows}: {s}\n{terminal}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The release frame cost.

/// The panel's release frame cost at 1000×700 with the pane open — a merge with 300 files and a
/// Hangul message of 200 lines over the fixture's graph — run alone: `cargo test --release --lib
/// --no-default-features -- --ignored --exact
/// gui::browser::git_panel::tests_x4_pane::frame_cost_at_1000_by_700 --nocapture`. Printed, no
/// budget set.
#[test]
#[ignore]
fn frame_cost_at_1000_by_700() {
    use std::time::Instant;
    let line = "한글 메시지의 한 줄과 an English part of the same line, 길게 이어집니다";
    let message: String = (0..200).map(|k| format!("{k:03} {line}\n")).collect();
    let mut details = merge_details(&message);
    details.groups[0].files = (0..300u64)
        .map(|k| {
            let path = format!("src/모듈-{k}/파일-{k}.rs");
            file('M', &path, None, Some((k, 2 * k)))
        })
        .collect();
    let mut fx = Fx::new(1000.0, 700.0, fixture_view());
    fx.press(Key::ArrowDown);
    fx.press(Key::Enter);
    fx.view_mut().set_details(details);
    fx.drv.idle(&mut fx.host, 20);
    let n = 400;
    let started = Instant::now();
    for _ in 0..n {
        fx.drv.frame(&mut fx.host, Vec::new(), Modifiers::NONE);
    }
    println!(
        "the git panel's frame at 1000x700 with the pane: {:?}",
        started.elapsed() / n
    );
}
