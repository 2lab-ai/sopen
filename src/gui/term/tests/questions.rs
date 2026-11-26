//! TS-1, TS-4 and TS-6 (10 §14) for the hex window's questions (X3-questions): the close prompt
//! and Save As over the hex view of the file browser's viewer, through the terminal's bytes as
//! TS-1 checks the list — the window's shapes laid back onto cells against the backend's bytes
//! in the strict VT model, under truecolor, through the 256-colour table and in an
//! ambiguous-wide terminal; no hand-off box and no hand-off line where the egui questions had
//! them; and once with a Hangul name under the prompt's edges, whose half glyph a terminal
//! erases and the window must too.

use super::*;

/// Every byte, so the hex view's CP437 column holds glyphs of every width class.
fn every_byte() -> Vec<u8> {
    (0..=255u8).collect()
}

/// The browser in a window `rows` tall on the fixture with `name` (every byte) first among its
/// files, in its hex view (a binary opens there), viewing.
fn hex_view(name: &str, rows: u16) -> (Fx, Driver, Shared) {
    let fx = fixture_with(0, &[name]);
    std::fs::write(std::path::Path::new(&fx.root).join(name), every_byte()).unwrap();
    let (mut drv, mut win) = window(&fx, 100, rows);
    // `..`, three folders, then it.
    for _ in 0..4 {
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    }
    drv.until(&mut win, 20, |w| w.ready());
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 2);
    (fx, drv, win)
}

/// An edit (Enter edits, `41` typed, Esc ends it), then Esc again: the viewer asks to close —
/// the hex window's close prompt; once the edit is kept on disk (the prompt says where Discard
/// deletes it: a row more, which moves its box), so both backends see one prompt.
fn edited_then_esc(drv: &mut Driver, win: &mut Shared) {
    drv.key(win, Key::Enter, Modifiers::NONE);
    for digit in ["4", "1"] {
        drv.frame(win, vec![egui::Event::Text(digit.into())], Modifiers::NONE);
    }
    drv.key(win, Key::Escape, Modifiers::NONE);
    drv.key(win, Key::Escape, Modifiers::NONE);
    drv.until(win, 20, |w| w.ready());
    let kept = |drv: &Driver| {
        let layers = cells::layers(&drv.ctx);
        !cells::find(&layers, "Discard deletes").is_empty()
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !kept(drv) {
        assert!(Instant::now() < deadline, "the edit never kept");
        drv.idle(win, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    drv.idle(win, 3);
}

/// The window's cells against the terminal's bytes — truecolor and the 256-colour table —
/// `shown` on screen, no hand-off box and no hand-off line; then an ambiguous-wide terminal:
/// chrome through §10's table, content of width A laid in two cells, the rest the window's.
fn the_same_cells(fx: &Fx, drv: &Driver, win: &mut Shared, rows: u16, shown: &[&str], what: &str) {
    // A narrow terminal first (an earlier wide pass left its answer in the context).
    cells::set_ambiguous_wide(&drv.ctx, false);
    let mut sw = switched(fx);
    let painted = window_cells(&window_pass(&drv.ctx, &mut sw, 100, rows), 100, rows);
    assert!(
        painted.0.iter().all(|c| c.covered),
        "{what}: every cell is painted"
    );
    let vt = terminal_of(&drv.ctx, win, 100, rows, true);
    let text = vt.text();
    for s in shown {
        assert!(text.contains(s), "{what}: {s:?} on screen:\n{text}");
    }
    assert!(!text.contains(HAND_OFF), "{what}: no hand-off box:\n{text}");
    assert!(
        !text.contains(MODAL_LINE),
        "{what}: no hand-off line:\n{text}"
    );
    let d = differences(&painted, &vt, Ink::Rgb);
    only_the_mode_button(&d, &vt, &format!("{what}, truecolor"));
    let vt = terminal_of(&drv.ctx, win, 100, rows, false);
    let d = differences(&painted, &vt, |c| Ink::Index(cells::index256(c)));
    only_the_mode_button(&d, &vt, &format!("{what}, 256 colours"));
    // The control: a cell of the window's changed is a difference more.
    let mut changed = painted.clone();
    changed.0[100 * 2 + 50].symbol = "X".into();
    let vt = terminal_of(&drv.ctx, win, 100, rows, true);
    let d = differences(&changed, &vt, Ink::Rgb);
    assert_eq!(
        d.len(),
        2,
        "{what}: the control:\n{}\n{}",
        d.join("\n"),
        vt.text()
    );
    // An ambiguous-wide terminal, on the same context (its probe says so to the passes).
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (100, rows);
        s.ambiguous = true;
    }
    let out = Bytes::default();
    let mut b = Backend::new(tape, out.clone(), drv.ctx.clone(), true);
    b.offset = later();
    b.enter().unwrap();
    b.pass(win, vec![]).unwrap();
    b.draw().unwrap();
    let mut wide = Vt::wide(100, rows);
    wide.feed(&out.0.borrow());
    let text = wide.text();
    assert!(!text.contains(HAND_OFF), "{what}, wide: no hand-off box");
    let grid = b.screen().buf;
    // The window passes run again in a narrow terminal's context after this one: the probe's
    // answer is the context's, so its own pass is laid again for the comparison.
    let painted = window_cells(&window_pass(&drv.ctx, &mut sw, 100, rows), 100, rows);
    let window = through_the_table(&painted, &grid);
    let wide_rows: Vec<u16> = (0..rows)
        .filter(|&y| (0..100).any(|x| laid_wide(&grid, x, y)))
        .collect();
    let words = |s: &str| -> String {
        let s = s.replace("[GUI]", "").replace("[TUI]", "");
        s.chars().filter(|c| !c.is_whitespace()).collect()
    };
    for &y in &wide_rows {
        let row: String = (0..100)
            .map(|x| window.0[usize::from(y) * 100 + x].symbol.as_str())
            .collect();
        assert_eq!(
            words(&wide.row(y)),
            words(&row),
            "{what}, wide: row {y}:\n{text}"
        );
        for x in 0..100u16 {
            let at = &painted.0[usize::from(y) * 100 + usize::from(x)];
            if at.symbol.chars().any(cells::frame_glyph) && chrome(&grid, x, y) {
                let want = stand_in(&grid, x, y).unwrap_or(&at.symbol);
                assert_eq!(wide.cell(x, y).symbol, want, "{what}: ({x},{y}):\n{text}");
            }
        }
    }
    for y in 0..rows {
        for x in 0..100 {
            let s = &wide.cell(x, y).symbol;
            let left = chrome(&grid, x, y) && narrow(grid[(x, y)].symbol()).is_some();
            assert!(
                !(left && *s == grid[(x, y)].symbol()),
                "{what}: ({x},{y}) chrome {s:?} sent as it is:\n{text}"
            );
        }
    }
    let d = differences_but(&window, &wide, Ink::Rgb, &wide_rows);
    if d.len() > 1 {
        let shown: String = (0..rows)
            .map(|y| {
                let w: String = (0..100)
                    .map(|x| window.0[usize::from(y) * 100 + x].symbol.as_str())
                    .collect();
                format!("{y:2} t {}\n{y:2} w {w}\n", wide.row(y))
            })
            .collect();
        eprintln!("{what}:\n{shown}");
    }
    only_the_mode_button(&d, &wide, &format!("{what}, ambiguous-wide"));
}

/// TS-1 and TS-4 on the hex view's close prompt (07 §10): in the terminal its box in cells —
/// question, `[ Save ]  [ Save As… ]  [ Discard ]  [ Cancel ]`, where Discard deletes the kept
/// edits — where an egui dialog was a hand-off box with its line; the window's cells exactly but
/// the mode button, in each colour mode and in an ambiguous-wide terminal.
#[test]
fn ts1_the_close_prompt_is_the_windows_cells_in_the_terminal() {
    let (fx, mut drv, mut win) = hex_view("a-blob.bin", 30);
    edited_then_esc(&mut drv, &mut win);
    let shown = [
        " Unsaved changes ",
        "before closing?",
        "[ Save ]  [ Save As… ]  [ Discard ]  [ Cancel ]",
    ];
    the_same_cells(&fx, &drv, &mut win, 30, &shown, "the close prompt");
    // Tab to a button: its colours and the focus are the same in both.
    drv.key(&mut win, Key::Tab, Modifiers::NONE);
    drv.idle(&mut win, 2);
    the_same_cells(&fx, &drv, &mut win, 30, &shown, "the prompt, Save focused");
}

/// TS-1 and TS-4 on Save As over the hex view (F7): the modal in cells — its words, the path
/// field with its caret, `[ Browse… ]`, the hint, `[ Save ]  [ Cancel ]` — the window's cells in
/// the terminal, but the mode button, in each colour mode and ambiguous-wide; then with a Hangul
/// name typed in its field (the terminal's cursor at its caret).
#[test]
fn ts1_save_as_is_the_windows_cells_in_the_terminal() {
    let (fx, mut drv, mut win) = hex_view("a-blob.bin", 30);
    drv.key(&mut win, Key::F7, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 3);
    let shown = [
        " Save As ",
        "Save a-blob.bin with your edits to a new local file",
        "[ Browse… ]",
        "never over an existing file · Enter saves · Esc cancels",
        "[ Save ]  [ Cancel ]",
    ];
    the_same_cells(&fx, &drv, &mut win, 30, &shown, "Save As");
    drv.key(&mut win, Key::A, Modifiers::COMMAND);
    let typed = vec![egui::Event::Text("한글 사본.bin".into())];
    drv.frame(&mut win, typed, Modifiers::NONE);
    drv.idle(&mut win, 2);
    let shown = ["한글 사본.bin", "[ Save ]  [ Cancel ]"];
    the_same_cells(&fx, &drv, &mut win, 30, &shown, "Save As, Hangul typed");
}

/// The close prompt over a Hangul name in a short window, so the prompt's box and its shadow
/// stand over the hex view's file line: the name's glyphs under the box's edges — at either
/// parity of the box's place — are erased in the window as the terminal erases them, and the two
/// show the same cells.
#[test]
fn ts1_a_hangul_name_under_the_prompts_edges_is_the_same_cells() {
    for name in [
        format!("a-{}.bin", "한글".repeat(30)),
        format!("a-x{}.bin", "한글".repeat(30)),
    ] {
        let (fx, mut drv, mut win) = hex_view(&name, 14);
        edited_then_esc(&mut drv, &mut win);
        let shown = [" Unsaved changes ", "[ Discard ]  [ Cancel ]"];
        the_same_cells(&fx, &drv, &mut win, 14, &shown, &name);
    }
}
