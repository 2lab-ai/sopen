//! X1b's dialogs in cells (10 §4–§6, §10; XW-dialogs): F2's field in its row, F5 / F6's Copy and
//! Move, F7's make folder, F8's delete, `+` / `-`'s select group, the name in the way and the
//! report are each cells on the window's grid — the frame in box glyphs with the title in its
//! top line and the shadow two cells right and one down, every text and `[ button ]` a whole
//! text of its row that no galley draws, the field black cells holding its text (a Hangul
//! glyph two cells) with the caret's cell — and the dialog and its shadow inside the window at
//! the smallest one (480×320). Each with its negative control: a text the dialog does not show
//! there (an error before the refusal, another dialog's button) and a part of a text it shows
//! never pass.

use super::*;
use crate::gui::cells;
use crate::gui::chrome::cga::{BLACK, WHITE};

const CELL: Vec2 = chrome::CELL;

/// `text` stands on screen as cells: a whole text of its row (the kit's exact rule), drawn by no
/// galley (the cell painter draws a glyph a galley).
fn in_cells(drv: &Driver, text: &str) -> bool {
    drv.find_text(text).is_some() && !drv.galleys().iter().any(|(_, g)| g.text() == text)
}

/// Every text of `texts` in cells; the ones that are not, said.
fn all_in_cells(drv: &Driver, texts: &[&str]) {
    let missing: Vec<&&str> = texts.iter().filter(|t| !in_cells(drv, t)).collect();
    assert!(missing.is_empty(), "not cells on screen: {missing:?}");
}

/// What the cell whose top-left corner is `at` shows on top (the last layer painted there).
fn cell_at(drv: &Driver, at: Pos2) -> Option<String> {
    let one = Rect::from_min_size(at, CELL);
    drv.column(one).pop().map(|(_, t)| t)
}

/// The dialog titled `title` in cells: a row `╔═…═ title ═…═╗`, rows `║ … ║` under it (the
/// right border may be a scroll bar's `↑ ░ ↓` and its thumb, a blank) and `╚═…═╝` — its rect on
/// screen. The borders are read a cell at a time: a wide glyph in a row adds one character for
/// its two cells.
fn frame_of(drv: &Driver, title: &str) -> Option<Rect> {
    let screen = Rect::from_min_size(Pos2::ZERO, drv.ctx.content_rect().size());
    let head = format!("═ {title} ═");
    for (top, text) in drv.column(screen) {
        let line = text.trim();
        if !(line.starts_with('╔') && line.ends_with('╗') && line.contains(&head)) {
            continue;
        }
        let lead = text.chars().take_while(|c| *c == ' ').count() as f32;
        let left = top.left() + lead * CELL.x;
        let right = left + cells::width(line) as f32 * CELL.x - CELL.x;
        let mut y = top.top() + CELL.y;
        while y + CELL.y <= screen.bottom() {
            let ends = (
                cell_at(drv, Pos2::new(left, y)),
                cell_at(drv, Pos2::new(right, y)),
            );
            match (ends.0.as_deref(), ends.1.as_deref()) {
                (Some("╚"), Some("╝")) => {
                    let max = Pos2::new(right + CELL.x, y + CELL.y);
                    return Some(Rect::from_min_max(Pos2::new(left, top.top()), max));
                }
                (Some("║"), Some("║" | "↑" | "░" | "↓" | " ")) => y += CELL.y,
                _ => break,
            }
        }
    }
    None
}

/// The frame titled `title`, else the test fails naming what the cells show.
fn framed(drv: &Driver, title: &str) -> Rect {
    frame_of(drv, title).unwrap_or_else(|| {
        let screen = Rect::from_min_size(Pos2::ZERO, drv.ctx.content_rect().size());
        let rows: Vec<String> = drv.column(screen).into_iter().map(|(_, t)| t).collect();
        panic!("no frame titled {title:?} in cells: {rows:#?}")
    })
}

/// A rect of `fill` covering `r` was painted (a run of cell backgrounds).
fn filled(drv: &Driver, r: Rect, fill: Color32) -> bool {
    (drv.rects().iter()).any(|&(at, c, _)| c == fill && at.expand(0.25).contains_rect(r))
}

/// The DOS shadow (10 §6): black cells two wide right of `frame` from its second row to one row
/// under it, and one row under it from its third column.
fn shadowed(drv: &Driver, frame: Rect) -> bool {
    let right = Rect::from_min_max(
        Pos2::new(frame.right(), frame.top() + CELL.y),
        Pos2::new(frame.right() + 2.0 * CELL.x, frame.bottom() + CELL.y),
    );
    let under = Rect::from_min_max(
        Pos2::new(frame.left() + 2.0 * CELL.x, frame.bottom()),
        Pos2::new(frame.right() + 2.0 * CELL.x, frame.bottom() + CELL.y),
    );
    let rows = |r: Rect| {
        let n = (r.height() / CELL.y) as usize;
        (0..n).all(|k| {
            let row = Rect::from_min_size(
                Pos2::new(r.left(), r.top() + k as f32 * CELL.y),
                Vec2::new(r.width(), CELL.y),
            );
            filled(drv, row, BLACK)
        })
    };
    rows(right) && rows(under)
}

/// The field holding `text` in cells, in white on black — on the selection's colour when
/// `selected` (a dialog selects its field's text as it opens): its rect.
fn field_text(drv: &Driver, text: &str, selected: bool) -> Rect {
    assert!(in_cells(drv, text), "the field's {text:?} is not cells");
    let at = drv.find_text(text).unwrap();
    let back = if selected { chrome::PALETTE.bar } else { BLACK };
    assert!(filled(drv, at, back), "{text:?} on {back:?}");
    assert_eq!(drv.colors_of(text).unwrap(), [WHITE], "{text:?} in white");
    at
}

/// The caret's two scan lines at the foot of the cell at `cell` (its top-left corner).
fn caret_at(drv: &Driver, cell: Pos2) -> bool {
    let foot = Rect::from_min_size(cell + Vec2::new(0.0, CELL.y - 2.0), Vec2::new(CELL.x, 2.0));
    (drv.rects().iter()).any(|&(r, _, _)| r == foot)
}

/// The dialog `frame` and its shadow lie inside the `w`×`h` window, on whole cells.
fn inside(frame: Rect, w: f32, h: f32) -> bool {
    let all = frame.union(frame.translate(Vec2::new(2.0 * CELL.x, CELL.y)));
    let on_grid = |v: f32, cell: f32| (v / cell).fract() == 0.0;
    Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h)).contains_rect(all)
        && on_grid(frame.left(), CELL.x)
        && on_grid(frame.top(), CELL.y)
}

/// F2: the bar's name becomes black cells in its row, the stem in the selection's colours, the
/// caret after it; typed into, the field holds what was typed — a Hangul syllable two cells,
/// the caret after it. The control: a part of the name and a name not typed are not there.
#[test]
fn x1b_the_rename_field_is_cells_in_its_row() {
    let (mut h, _) = host(&[]);
    let mut drv = Driver::new(800.0, 500.0);
    at(&mut h.panel, "main.rs");
    h.ops.open_rename(&h.panel, None);
    drv.idle(&mut h, 2);
    let row = row_rect(h.panel.highlight);
    assert!(in_cells(&drv, "main.rs"), "the field's text in cells");
    let name = drv.find_text("main.rs").unwrap();
    assert_eq!(name.min, row.min, "where the name was");
    let p = chrome::PALETTE;
    assert!(
        filled(&drv, drv.find_text("main").unwrap(), p.bar),
        "`main` selected"
    );
    assert!(filled(&drv, drv.find_text(".rs").unwrap(), BLACK));
    assert!(
        caret_at(&drv, row.min + Vec2::new(4.0 * CELL.x, 0.0)),
        "after the stem"
    );
    type_text(&mut drv, &mut h, "새");
    let typed = field_text(&drv, "새.rs", false);
    assert_eq!(typed.width(), 5.0 * CELL.x, "새 two cells, .rs three");
    assert!(
        caret_at(&drv, row.min + Vec2::new(2.0 * CELL.x, 0.0)),
        "after 새"
    );
    assert!(
        !in_cells(&drv, "새.r") && !in_cells(&drv, "main.rs"),
        "the control"
    );
}

/// F5 and F6: framed `Copy` / `Move`, what and the target field in cells, the hint, the
/// buttons; a target that does not read says why in cells. The control: the error before Enter,
/// the other kind's button and a part of a label are not there.
#[test]
fn x1b_the_copy_and_move_dialogs_are_cells() {
    let (mut h, _) = host(&[]);
    let mut drv = Driver::new(900.0, 500.0);
    h.panel.select_group("*.rs", true);
    let other = on("me@box", "/srv/다른 폴더");
    h.ops
        .open_copy(Transfer::Copy, &h.panel, Some("me@box"), &other);
    drv.idle(&mut h, 2);
    let frame = framed(&drv, "Copy");
    assert!(shadowed(&drv, frame), "the shadow");
    all_in_cells(
        &drv,
        &[
            "Copy 2 items",
            "to",
            "the other panel's folder",
            "[ Copy ]",
            "[ Cancel ]",
        ],
    );
    let shown = "me@box:/srv/다른 폴더";
    let target = field_text(&drv, shown, true);
    // 17 characters, four of them Hangul syllables of two cells each: 21 cells.
    assert_eq!(shown.chars().count(), 17);
    assert_eq!(
        target.width(),
        (17.0 + 4.0) * CELL.x,
        "two cells a syllable"
    );
    assert!(frame.contains_rect(target));
    let bad = "box:relative: give the folder on box as an absolute path (box:/…)";
    assert!(!in_cells(&drv, bad), "the control: no error yet");
    assert!(!in_cells(&drv, "[ Move ]") && !in_cells(&drv, "Copy 2"));
    type_text(&mut drv, &mut h, "box:relative");
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    all_in_cells(
        &drv,
        &[
            bad,
            "host:/abs on a remote · /abs here · a name: inside this folder",
        ],
    );
    assert!(drv
        .colors_of(bad)
        .unwrap()
        .contains(&chrome::PALETTE.dialog_error));
    drv.key(&mut h, Key::Escape, Modifiers::NONE);
    assert!(frame_of(&drv, "Copy").is_none(), "closed");

    at(&mut h.panel, "사진.png");
    h.panel.clear_selection();
    h.ops
        .open_copy(Transfer::Move, &h.panel, None, &here("/tmp/x"));
    drv.idle(&mut h, 2);
    framed(&drv, "Move");
    all_in_cells(&drv, &["Move 사진.png", "[ Move ]", "[ Cancel ]"]);
    field_text(&drv, "/tmp/x", true);
    assert!(!in_cells(&drv, "[ Copy ]"), "the control");
}

/// F7: framed `Make folder`, the folder, the label, the rule and the buttons in cells; `새 폴더
/// 2026` typed is twelve cells of the field, the caret on the thirteenth; Enter on nothing says
/// why in cells. The control: that reason before Enter.
#[test]
fn x1b_the_make_folder_dialog_is_cells() {
    let (mut h, _) = host(&[]);
    let mut drv = Driver::new(900.0, 500.0);
    h.ops.open_make_folder(&h.panel, None);
    drv.idle(&mut h, 2);
    let frame = framed(&drv, "Make folder");
    assert!(shadowed(&drv, frame));
    all_in_cells(
        &drv,
        &[
            "Make a folder in /srv/src",
            "name",
            "one level: no / in the name · a name that exists is an error",
            "[ Make ]",
            "[ Cancel ]",
        ],
    );
    let empty = "a name cannot be empty";
    assert!(!in_cells(&drv, empty), "the control: no error yet");
    type_text(&mut drv, &mut h, "새 폴더 2026");
    let typed = field_text(&drv, "새 폴더 2026", false);
    assert_eq!(
        typed.width(),
        12.0 * CELL.x,
        "three syllables of two cells, six of one"
    );
    assert!(
        caret_at(&drv, typed.right_top()),
        "the caret's cell after it"
    );
    drv.key(&mut h, Key::A, crate::gui::testkit::command(false));
    drv.key(&mut h, Key::Backspace, Modifiers::NONE);
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    assert!(in_cells(&drv, empty), "the reason in cells");
    assert_eq!(
        drv.colors_of(empty).unwrap(),
        [chrome::PALETTE.dialog_error]
    );
}

/// F8: framed `Delete`, the question, where it goes (permanent in the error colour), the
/// buttons and the hint in cells; while the Trash is checked, the line in the weak colour and
/// `[ Delete ]` off. The control: the link's line without a link.
#[test]
fn x1b_the_delete_dialog_is_cells() {
    let (mut h, _) = host(&[]);
    let mut drv = Driver::new(1000.0, 500.0);
    at(&mut h.panel, "main.rs");
    h.ops.open_delete(&h.panel, None, Removal::Trash, true);
    drv.idle(&mut h, 2);
    let frame = framed(&drv, "Delete");
    assert!(shadowed(&drv, frame));
    let p = chrome::PALETTE;
    all_in_cells(
        &drv,
        &[
            "Delete main.rs?",
            "to the Trash",
            "[ Delete ]",
            "[ Cancel ]",
            "Enter deletes · Esc cancels",
        ],
    );
    assert_eq!(drv.colors_of("to the Trash").unwrap(), [p.dialog_text]);
    let link = "a link goes as a link, never what it points at";
    assert!(!in_cells(&drv, link), "the control: no link");
    drv.key(&mut h, Key::Escape, Modifiers::NONE);
    h.ops
        .open_delete(&h.panel, Some("me@box"), Removal::Trash, true);
    drv.idle(&mut h, 2);
    let gone = "permanent — no Trash on me@box";
    assert!(in_cells(&drv, gone));
    assert_eq!(drv.colors_of(gone).unwrap(), [p.dialog_error]);
    drv.key(&mut h, Key::Escape, Modifiers::NONE);
    h.ops.open_delete_checking(&h.panel).unwrap();
    drv.idle(&mut h, 2);
    let checking = "checking the Trash…";
    assert!(in_cells(&drv, checking));
    assert_eq!(drv.colors_of(checking).unwrap(), [p.dialog_weak]);
    assert_eq!(drv.colors_of("[ Delete ]").unwrap(), [p.dialog_weak], "off");
    assert_eq!(drv.colors_of("[ Cancel ]").unwrap(), [p.dialog_text]);
}

/// `+` and `-`: framed `Select` / `Unselect`, the line, the pattern field, the hint and the
/// buttons in cells. The control: the other one's button.
#[test]
fn x1b_the_group_dialogs_are_cells() {
    let (mut h, _) = host(&[]);
    let mut drv = Driver::new(800.0, 500.0);
    h.ops.open_group(true);
    drv.idle(&mut h, 2);
    let frame = framed(&drv, "Select");
    assert!(shadowed(&drv, frame));
    all_in_cells(
        &drv,
        &[
            "Select the names that match",
            "* any characters · ? one · any case",
            "[ Select ]",
            "[ Cancel ]",
        ],
    );
    type_text(&mut drv, &mut h, "*.RS");
    field_text(&drv, "*.RS", false);
    assert!(!in_cells(&drv, "[ Unselect ]"), "the control");
    drv.key(&mut h, Key::Escape, Modifiers::NONE);
    h.ops.open_group(false);
    drv.idle(&mut h, 2);
    framed(&drv, "Unselect");
    all_in_cells(&drv, &["Unselect the names that match", "[ Unselect ]"]);
    assert!(!in_cells(&drv, "[ Select ]"), "the control");
}

/// The name in the way, then the report: each framed, every line, the field, the check box and
/// the buttons in cells; the check box clicked shows `[X]`; the report's rows in their colours.
/// The control: `[ Overwrite ]`, never offered, and `[X]` before the click.
#[test]
fn x1b_the_name_in_the_way_and_the_report_are_cells() {
    let (mut h, _) = host(&["a.txt"]);
    let mut drv = Driver::new(900.0, 600.0);
    let entries = ["a.txt", "b.txt"]
        .iter()
        .map(|n| entry(n, EntryKind::File, 1))
        .collect();
    h.panel.fill("/srv/src".to_string(), entries, None, None);
    h.panel.select_all();
    h.ops
        .open_copy(Transfer::Copy, &h.panel, None, &here("/srv/dst"));
    drv.idle(&mut h, 2);
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.until(&mut h, 5, |h| {
        h.ops.running.as_ref().is_some_and(|r| r.asking())
    });
    drv.idle(&mut h, 2);
    let frame = framed(&drv, "Name exists");
    assert!(shadowed(&drv, frame));
    all_in_cells(
        &drv,
        &[
            "a.txt is already in /srv/dst.",
            "Nothing is ever written over it.",
            "Rename to",
            "[ ] the same for the rest",
            "[ Skip ]",
            "[ Rename ]",
            "[ Cancel ]",
            "Enter renames · Esc cancels the rest",
        ],
    );
    field_text(&drv, ".txt", false);
    assert!(in_cells(&drv, "a-1"), "the stem selected");
    assert!(!in_cells(&drv, "[ Overwrite ]") && !in_cells(&drv, "[X] the same for the rest"));
    drv.click_text(&mut h, "[ ] the same for the rest");
    assert!(in_cells(&drv, "[X] the same for the rest"), "checked");
    drv.click_text(&mut h, "[ Skip ]");
    drv.until(&mut h, 5, |h| !h.changed.is_empty());
    drv.idle(&mut h, 2);
    let frame = framed(&drv, "Copy report");
    assert!(shadowed(&drv, frame));
    all_in_cells(
        &drv,
        &[
            "Copy to /srv/dst: 1 done · 1 skipped",
            "skipped     a.txt",
            "done        b.txt",
            "[ Close ]",
        ],
    );
    assert!(!in_cells(&drv, "skipped     b.txt"), "the control");
}

/// The report of many items on the smallest window: as many rows as fit, a scroll bar on the
/// frame's right border, the wheel brings the last row in; the dialog and its shadow inside the
/// window. The control: the last row before the wheel.
#[test]
fn x1b_a_long_report_scrolls_in_its_frame_on_the_smallest_window() {
    let names: Vec<String> = (1..=40).map(|i| format!("file-{i:02}.txt")).collect();
    let report = Report {
        items: (names.iter()).map(|n| item(n, ItemOutcome::Done)).collect(),
        notes: vec!["the last note".to_string()],
    };
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let Finished::Report(mut view) = finish(&copy_job(&refs), report) else {
        panic!("a report");
    };
    struct One<'a>(&'a mut ReportView);
    impl Window for One<'_> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.0.ui(ui.ctx(), dialog::grid(ui.ctx()));
        }
        fn ready(&self) -> bool {
            true
        }
    }
    let (w, h) = (480.0, 320.0);
    let mut drv = Driver::new(w, h);
    let mut win = One(&mut view);
    drv.idle(&mut win, 2);
    let frame = framed(&drv, "Copy report");
    assert!(inside(frame, w, h), "{frame:?} in {w}x{h}");
    all_in_cells(&drv, &["done        file-01.txt", "[ Close ]"]);
    let last = "the last note";
    assert!(!in_cells(&drv, last), "the control: below the rows shown");
    let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h));
    let border = Rect::from_min_max(
        Pos2::new(frame.right() - CELL.x, frame.top()),
        Pos2::new(frame.right(), frame.bottom()),
    );
    let bar: String = (drv.column(border.intersect(screen)).into_iter())
        .map(|(_, t)| t)
        .collect();
    assert!(
        bar.contains('↑') && bar.contains('↓'),
        "the scroll bar: {bar:?}"
    );
    for _ in 0..20 {
        drv.wheel(&mut win, frame.center(), -3.0);
    }
    assert!(in_cells(&drv, last), "the wheel brought it in");
    assert!(!in_cells(&drv, "done        file-01.txt"), "scrolled past");
}

/// Every dialog on the smallest window (480×320): framed on whole cells, the dialog and its
/// shadow inside the window, its buttons whole. The control: checked against a window that ends
/// a cell short of the shadow, it is not inside.
#[test]
fn x1b_every_dialog_fits_the_smallest_window() {
    let (w, h) = (480.0, 320.0);
    let (mut host, _) = self::host(&["a.txt"]);
    let mut drv = Driver::new(w, h);
    host.panel.select_group("*.rs", true);
    let long = here("/srv/a folder with a rather long name/and another one under it/더 깊은 폴더");
    type Open = fn(&mut Host, &OpsTarget);
    let cases: [(&str, &str, Open); 5] = [
        ("Copy", "[ Copy ]", |h, to| {
            h.ops
                .open_copy(Transfer::Copy, &h.panel, Some("me@box"), to);
        }),
        ("Move", "[ Move ]", |h, to| {
            h.ops.open_copy(Transfer::Move, &h.panel, None, to);
        }),
        ("Make folder", "[ Make ]", |h, _| {
            h.ops.open_make_folder(&h.panel, Some("me@box"));
        }),
        ("Delete", "[ Delete ]", |h, _| {
            h.ops
                .open_delete(&h.panel, Some("me@box"), Removal::Trash, true);
        }),
        ("Select", "[ Select ]", |h, _| h.ops.open_group(true)),
    ];
    for (title, button, open) in cases {
        open(&mut host, &long);
        drv.idle(&mut host, 2);
        let frame = framed(&drv, title);
        assert!(inside(frame, w, h), "{title}: {frame:?} in {w}x{h}");
        let short = frame.right() + CELL.x;
        assert!(!inside(frame, short, h), "{title}: the control");
        assert!(
            in_cells(&drv, button) && in_cells(&drv, "[ Cancel ]"),
            "{title}"
        );
        drv.key(&mut host, Key::Escape, Modifiers::NONE);
        assert!(frame_of(&drv, title).is_none(), "{title} closed");
    }
}

/// A target longer than its field opens scrolled to the caret at its end; a click on a cell the
/// field shows puts the caret before that cell's character — the cells and the editing engine
/// agree on what stands where — and typing goes in there. A move left keeps the view while the
/// caret is in it. The control: the character a cell before.
#[test]
fn x1b_a_click_in_a_scrolled_field_lands_on_the_character_it_shows() {
    let (mut h, _) = host(&[]);
    let mut drv = Driver::new(900.0, 500.0);
    at(&mut h.panel, "main.rs");
    let long: String = (0..12).map(|i| format!("/d{i:02}-abcdefg")).collect();
    h.ops
        .open_copy(Transfer::Copy, &h.panel, None, &here(&long));
    drv.idle(&mut h, 2);
    let to = drv.find_text("to").expect("the field's label");
    // The field: its row's runs of black and of the selection's colour after the label, inside
    // the frame (the shadow is black too).
    let (bar, inner) = (chrome::PALETTE.bar, framed(&drv, "Copy").shrink(CELL.x));
    let field = (drv.rects().into_iter())
        .filter(|&(r, _, _)| r.top() == to.top() && r.left() > to.right())
        .filter(|&(r, c, _)| r.right() <= inner.right() && (c == BLACK || c == bar))
        .map(|(r, _, _)| r)
        .reduce(|a, b| a.union(b))
        .expect("the field's cells");
    let (row, shown) = (drv.column(field).pop()).expect("the field's cells");
    let shown = shown.trim_end().to_string();
    assert!(
        shown.len() < long.len() && long.ends_with(&shown),
        "scrolled to its end: {shown:?}"
    );
    let start = long.len() - shown.len();
    // The left quarter of the field's eleventh cell: the caret goes before its character.
    let k = 10;
    let at_cell = Pos2::new(row.left() + k as f32 * CELL.x + 2.0, row.center().y);
    drv.click(&mut h, at_cell);
    type_text(&mut drv, &mut h, "X");
    let want = format!("{}X{}", &long[..start + k], &long[start + k..]);
    let got = h.ops.field_text().unwrap().to_string();
    let control = format!("{}X{}", &long[..start + k - 1], &long[start + k - 1..]);
    assert_ne!(got, control, "the control: a cell before");
    assert_eq!(
        got,
        want,
        "X before the clicked cell's {:?}",
        &long[start + k..start + k + 1]
    );
    let (_, now) = (drv.column(field).pop()).unwrap();
    drv.key(&mut h, Key::ArrowLeft, Modifiers::NONE);
    let (_, after) = (drv.column(field).pop()).unwrap();
    assert_eq!(now, after, "a move left inside the view keeps it");
}

/// A name in the way, its name and place far longer than the smallest window: the question
/// gives way ("…" on its last row kept), never the field, the check box or the buttons; the
/// dialog and its shadow inside the window. The control: the question's end is not shown.
#[test]
fn x1b_a_tall_question_is_cut_to_fit_the_smallest_window() {
    const NAME: &str = "a-file-whose-name-goes-on-and-on-and-on-and-on-and-on-and-on-and-on-\
                        and-on-and-on-and-on-and-on-and-on-and-on-and-on-and-on-and-on-and-on-\
                        and-on-and-on-and-on-and-on-and-on-and-on-and-on-and-on-and-on.txt";
    let place: String = (0..10)
        .map(|i| format!("/폴더-{i}-with-a-long-name"))
        .collect();
    let (w, h) = (480.0, 320.0);
    let (mut host, _) = self::host(&[NAME]);
    let mut drv = Driver::new(w, h);
    let entries = vec![entry(NAME, EntryKind::File, 1)];
    host.panel.fill("/srv/src".to_string(), entries, None, None);
    host.panel.select_all();
    host.ops
        .open_copy(Transfer::Copy, &host.panel, None, &here(&place));
    drv.idle(&mut host, 2);
    drv.key(&mut host, Key::Enter, Modifiers::NONE);
    drv.until(&mut host, 5, |h| {
        h.ops.running.as_ref().is_some_and(|r| r.asking())
    });
    drv.idle(&mut host, 2);
    let frame = framed(&drv, "Name exists");
    assert!(inside(frame, w, h), "{frame:?} in {w}x{h}");
    assert!(shadowed(&drv, frame));
    all_in_cells(
        &drv,
        &[
            "Rename to",
            "[ ] the same for the rest",
            "[ Skip ]",
            "[ Rename ]",
            "[ Cancel ]",
        ],
    );
    let rows: Vec<String> = (drv.texts().into_iter())
        .filter(|(r, _, _)| frame.contains_rect(*r))
        .map(|(_, _, t)| t)
        .collect();
    assert!(
        rows.iter().any(|t| t.ends_with('…')),
        "the question cut: {rows:#?}"
    );
    let end = format!("{}.", &place[place.len() - 12..]);
    assert!(
        !rows.iter().any(|t| t.ends_with(&end)),
        "the control: {rows:#?}"
    );
}

/// On a wide window a dialog is at most 76 cells inside its border and margin (its nominal
/// width where that is more): a place longer than that wraps by cells, a path broken anywhere.
/// The control: the question is on no one row whole.
#[test]
fn x1b_a_long_path_wraps_inside_76_cells_on_a_wide_window() {
    let place: String = (0..6)
        .map(|i| format!("/a-folder-{i}-with-a-long-name"))
        .collect();
    let (mut h, _) = host(&["main.rs"]);
    let mut drv = Driver::new(1000.0, 700.0);
    at(&mut h.panel, "main.rs");
    h.ops
        .open_copy(Transfer::Copy, &h.panel, None, &here(&place));
    drv.idle(&mut h, 2);
    drv.key(&mut h, Key::Enter, Modifiers::NONE);
    drv.until(&mut h, 5, |h| {
        h.ops.running.as_ref().is_some_and(|r| r.asking())
    });
    drv.idle(&mut h, 2);
    let frame = framed(&drv, "Name exists");
    let inside = frame.width() / CELL.x - 4.0;
    assert_eq!(inside, 76.0, "{frame:?}");
    let question = format!("main.rs is already in {place}.");
    assert!(!in_cells(&drv, &question), "the control: not on one row");
    assert!(shown_wrapped(&drv, &question), "wrapped by cells");
}

/// A field holding wide glyphs: a click lands on the character its cell shows — a Hangul
/// syllable's two cells its own, the left half before it, the right half after it — because the
/// editing engine lays each glyph at its cells, as the cells show it.
#[test]
fn x1b_a_click_on_wide_glyphs_lands_on_the_character_its_cell_shows() {
    let (mut h, _) = host(&[]);
    let mut drv = Driver::new(900.0, 500.0);
    h.ops.open_make_folder(&h.panel, None);
    drv.idle(&mut h, 2);
    type_text(&mut drv, &mut h, "새 폴더 2026");
    let typed = field_text(&drv, "새 폴더 2026", false);
    let cell = |k: f32| Pos2::new(typed.left() + k * CELL.x + 2.0, typed.center().y);
    // `2` stands on the field's cell 8: 새 two cells, a blank, 폴 two, 더 two, a blank.
    drv.click(&mut h, cell(8.0));
    type_text(&mut drv, &mut h, "X");
    assert_eq!(
        h.ops.field_text(),
        Some("새 폴더 X2026"),
        "before the clicked `2`"
    );
    // Cell 4 is 폴's second half: the caret goes after it.
    drv.click(&mut h, cell(4.0));
    type_text(&mut drv, &mut h, "W");
    assert_eq!(h.ops.field_text(), Some("새 폴W더 X2026"), "after 폴");
}

/// The field's IME rectangle — where the system puts its candidate window — is the caret's
/// cell after each syllable typed, as the cells show the caret (10 §4: a terminal's cursor goes
/// there too).
#[test]
fn x1b_the_ime_cursor_is_the_carets_cell() {
    struct Ime(Host, Option<egui::output::IMEOutput>);
    impl Window for Ime {
        fn ui(&mut self, ui: &mut egui::Ui) {
            Window::ui(&mut self.0, ui);
            self.1 = ui.ctx().output(|o| o.ime);
        }
        fn ready(&self) -> bool {
            true
        }
    }
    let (host, _) = host(&[]);
    let mut w = Ime(host, None);
    let mut drv = Driver::new(900.0, 500.0);
    w.0.ops.open_make_folder(&w.0.panel, None);
    drv.idle(&mut w, 2);
    let mut typed = String::new();
    for (k, syllable) in ["새", "폴", "더", "이", "름"].into_iter().enumerate() {
        typed.push_str(syllable);
        drv.frame(
            &mut w,
            vec![Event::Text(syllable.to_string())],
            Modifiers::NONE,
        );
        drv.idle(&mut w, 1);
        let field = drv.find_text(&typed).expect("the field's text");
        let caret = Rect::from_min_size(field.right_top(), CELL);
        let ime =
            w.1.expect("the IME rectangle while the field has the keyboard");
        let at = ime.cursor_rect;
        assert_eq!(
            field.width(),
            2.0 * CELL.x * (k + 1) as f32,
            "two cells a syllable"
        );
        // egui's cursor rectangle is the caret's line, 1.5 pt either side of it.
        assert_eq!(
            at.center().x,
            caret.left(),
            "after {typed}: {at:?} vs {caret:?}"
        );
        assert!(
            caret.y_range().contains(at.center().y),
            "{at:?} in {caret:?}"
        );
    }
}

/// A text wraps at its last blank that fits, the blank dropped — never leaving an empty last
/// row; no text is one empty row; a word longer than the row is cut after its last whole glyph,
/// never inside a wide one.
#[test]
fn x1b_texts_wrap_by_cells() {
    use super::super::dialog::wrap;
    assert_eq!(wrap("abc ", 3), ["abc"]);
    assert_eq!(wrap("abc  ", 3), ["abc"]);
    assert_eq!(wrap("", 3), [""]);
    assert_eq!(wrap("ab cd", 3), ["ab", "cd"]);
    assert_eq!(wrap("abcdef", 3), ["abc", "def"]);
    assert_eq!(wrap("가나다", 5), ["가나", "다"]);
    assert_eq!(wrap("a 가", 2), ["a", "가"]);
}

/// The report's scroll bar works as main's did: ↓ a row, ↑ back, a click on the track a page,
/// the thumb dragged to the track's end shows the last row. The control: the last row before.
#[test]
fn x1b_the_reports_scroll_bar_takes_clicks_and_a_drag() {
    let names: Vec<String> = (1..=20).map(|i| format!("file-{i:02}.txt")).collect();
    let report = Report {
        items: (names.iter()).map(|n| item(n, ItemOutcome::Done)).collect(),
        notes: vec![],
    };
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let Finished::Report(mut view) = finish(&copy_job(&refs), report) else {
        panic!("a report");
    };
    struct One<'a>(&'a mut ReportView);
    impl Window for One<'_> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.0.ui(ui.ctx(), dialog::grid(ui.ctx()));
        }
        fn ready(&self) -> bool {
            true
        }
    }
    let mut drv = Driver::new(900.0, 600.0);
    let mut win = One(&mut view);
    drv.idle(&mut win, 2);
    let frame = framed(&drv, "Copy report");
    let border = Rect::from_min_max(
        Pos2::new(frame.right() - CELL.x, frame.top()),
        Pos2::new(frame.right(), frame.bottom()),
    );
    let bar = |drv: &Driver, glyph: &str| -> Pos2 {
        (drv.column(border).into_iter())
            .find(|(_, t)| t == glyph)
            .unwrap_or_else(|| panic!("{glyph:?} on the frame's right border"))
            .0
            .center()
    };
    let last = "done        file-20.txt";
    assert!(!in_cells(&drv, last), "the control: below the rows shown");
    let (up, down) = (bar(&drv, "↑"), bar(&drv, "↓"));
    drv.click(&mut win, down);
    assert_eq!(win.0.top, 1, "↓: a row");
    drv.click(&mut win, up);
    assert_eq!(win.0.top, 0, "↑: back");
    drv.click(&mut win, down - Vec2::new(0.0, CELL.y));
    assert_eq!(
        win.0.top, 4,
        "the track under the thumb: a page, as far as it goes"
    );
    drv.click(&mut win, up + Vec2::new(0.0, CELL.y * 4.0));
    assert_eq!(win.0.top, 0, "the track over the thumb: a page back");
    // The thumb, on the track's first cell, dragged to its last.
    let thumb = up + Vec2::new(0.0, CELL.y);
    drv.drag(
        &mut win,
        egui::PointerButton::Primary,
        thumb,
        down - Vec2::new(0.0, CELL.y),
    );
    assert!(in_cells(&drv, last), "the thumb dragged to the end");
}
