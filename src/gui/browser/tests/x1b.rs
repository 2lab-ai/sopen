//! X1b's dialogs over the list (10 §4; cells.rs's rule for one buffer laid over another): the
//! list blanks the cells a dialog and its shadow cover before it is painted, so a wide glyph
//! half under the dialog's edge is gone from the list too, as a terminal erases a wide glyph
//! half written over — never half a glyph beside the dialog.

use super::*;

/// What the cell of the window's grid at (`col`, `row`) shows, a layer at a time, the lowest
/// first.
fn cell(fx: &Fx, col: u16, row: u16) -> Vec<String> {
    let at = Pos2::new(f32::from(col) * 8.0, f32::from(row) * 16.0);
    let one = Rect::from_min_size(at, Vec2::new(8.0, 16.0));
    fx.drv.column(one).into_iter().map(|(_, t)| t).collect()
}

/// `+`'s dialog on the smallest window stands from the window's column 5 (44 cells inside, its
/// box 48, its shadow 2: (60 − 50) / 2); the name `abc한글.txt`, cut to `abc한…` in the
/// six-cell Name column, puts 한 on the list's cells 4 and 5 — half under the dialog's left
/// border. With the dialog up, the list's cell 4 is a blank. The control: 한 there before.
#[test]
fn a_wide_glyph_half_under_a_dialog_goes_from_the_list() {
    let mut fx = fixture();
    std::fs::write(format!("{}/abc한글.txt", fx.root), "x").unwrap();
    let root = fx.root.clone();
    fx.b.list(None, root, None, None);
    fx.settle();
    fx.drv.resize(480.0, 320.0);
    fx.drv.idle(&mut fx.b, 3);
    let name = (fx.drv.texts_starting_with("abc한").into_iter().next())
        .expect("the name's cells")
        .0;
    let row = (name.top() / 16.0) as u16;
    assert_eq!(name.left(), 8.0, "from the list's cell 1");
    assert_eq!(cell(&fx, 4, row), ["한"], "the control: 한 on cell 4");
    fx.typed("+");
    assert!(fx.b.ops.holds_keyboard(), "the dialog is up");
    assert_eq!(
        cell(&fx, 5, row).last().map(String::as_str),
        Some("║"),
        "the dialog's left border on cell 5, over 한's second half"
    );
    assert_eq!(cell(&fx, 4, row), [" "], "한 is gone from the list");
    fx.key(Key::Escape);
    assert_eq!(cell(&fx, 4, row), ["한"], "back with the dialog gone");
}

/// The title row's cells and the status and F-key rows' on a `w`×`h` window, as the column
/// query reads them.
fn edges(fx: &Fx, w: f32, h: f32) -> Vec<(Rect, String)> {
    let top = Rect::from_min_size(Pos2::ZERO, Vec2::new(w, 16.0));
    let bottom = Rect::from_min_max(Pos2::new(0.0, h - 32.0), Pos2::new(w, h));
    let mut out = fx.drv.column(top);
    out.extend(fx.drv.column(bottom));
    out
}

/// A dialog stands between the title row and the status row: F7's refusal of a 1,000-character
/// name on the smallest window is cut to fit there, its shadow too — the title, status and
/// F-key rows read as they did before it opened.
#[test]
fn a_dialog_stays_between_the_title_row_and_the_status_row() {
    let mut fx = fixture();
    let (w, h) = (480.0, 320.0);
    fx.drv.resize(w, h);
    fx.drv.idle(&mut fx.b, 3);
    let before = edges(&fx, w, h);
    fx.key(Key::F7);
    fx.typed(&"a".repeat(1000));
    fx.key(Key::Enter);
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.ops.holds_keyboard(), "refused: still open");
    // The refusal — `aaa…: 1000 bytes; a name has at most 255` — is cut to the room, its reason
    // with it: its rows are told from the field's (white on black, never cut) by the error
    // colour and the "…" its last kept row ends in.
    let refused = (fx.drv.texts_starting_with("aaaa").into_iter())
        .map(|(_, t)| t)
        .find(|t| t.ends_with('…'))
        .expect("the refusal's last row, cut");
    assert_eq!(
        fx.drv.colors_of(&refused),
        Some(vec![chrome::PALETTE.dialog_error]),
        "{refused}"
    );
    assert_eq!(edges(&fx, w, h), before, "nothing of the dialog over them");
}

/// A window whose middle is four cells by four: F7's dialog is cut to those cells — nothing of
/// it over the title, status or F-key rows — and nothing panics.
#[test]
fn a_dialog_on_a_tiny_grid_stays_inside_it() {
    let mut fx = fixture();
    let (w, h) = (32.0, 112.0);
    fx.drv.resize(w, h);
    fx.drv.idle(&mut fx.b, 3);
    let before = edges(&fx, w, h);
    fx.key(Key::F7);
    fx.drv.idle(&mut fx.b, 2);
    assert!(fx.b.ops.holds_keyboard(), "the dialog is up");
    assert_eq!(edges(&fx, w, h), before, "nothing of the dialog over them");
    fx.key(Key::Escape);
    assert!(!fx.b.ops.holds_keyboard());
}
