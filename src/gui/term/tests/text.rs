//! TS-4 (10 §14) for the text viewer (X3-text), through the terminal's bytes as TS-1 checks the
//! list: the browser with a UTF-16LE Hangul file open in view 1 — a row holding a combining
//! mark, an emoji and Hebrew among its rows — and again with F8's list of encodings over it.
//! The window's shapes laid back onto cells against the backend's bytes in the strict VT model,
//! under truecolor and through the 256-colour table; the mode button the one difference; one
//! cell of the window's changed is one difference more.

use super::*;

/// The file: Hangul rows, the second a mark on `e`, an emoji and Hebrew (a run reversed into
/// the cells' order) — UTF-16LE with its BOM, CRLF.
fn hangul_utf16() -> Vec<u8> {
    let mut lines: Vec<String> = (0..60)
        .map(|i| format!("{i:04} 한글 줄 text 안녕하세요"))
        .collect();
    lines[1] = "0001 cafe\u{301} \u{1F600} שלום עולם end".to_string();
    let text: String = lines.iter().map(|l| format!("{l}\r\n")).collect();
    let mut out = vec![0xFF, 0xFE];
    text.encode_utf16()
        .for_each(|u| out.extend(u.to_le_bytes()));
    out
}

/// The window's cells against the terminal's bytes, both colour modes, and the negative control.
fn the_same_cells(fx: &Fx, drv: &Driver, win: &mut Shared, what: &str) {
    let mut sw = switched(fx);
    let painted = window_cells(&window_pass(&drv.ctx, &mut sw, 100, 30), 100, 30);
    assert!(
        painted.0.iter().all(|c| c.covered),
        "{what}: every cell is painted"
    );
    let vt = terminal_of(&drv.ctx, win, 100, 30, true);
    let text = vt.text();
    for shown in [
        "a-korean.txt",
        "Text Viewer",
        "UTF-16LE (BOM)",
        "0000 한글 줄 text 안녕하세요",
        "cafe\u{301} \u{1F600} םלוע םולש end",
    ] {
        assert!(text.contains(shown), "{what}: {shown:?} on screen:\n{text}");
    }
    assert!(!text.contains(HAND_OFF), "{what}: no hand-off box:\n{text}");
    let d = differences(&painted, &vt, Ink::Rgb);
    only_the_mode_button(&d, &vt, &format!("{what}, truecolor"));
    let vt = terminal_of(&drv.ctx, win, 100, 30, false);
    let d = differences(&painted, &vt, |c| Ink::Index(cells::index256(c)));
    only_the_mode_button(&d, &vt, &format!("{what}, 256 colours"));
    let mut changed = painted.clone();
    changed.0[100 * 5 + 10].symbol = "X".into();
    let vt = terminal_of(&drv.ctx, win, 100, 30, true);
    assert_eq!(
        differences(&changed, &vt, Ink::Rgb).len(),
        2,
        "{what}: the control"
    );
}

#[test]
fn ts4_the_text_viewer_shows_the_windows_cells_in_the_terminal() {
    // First among the files: `..`, three folders, then it (its content written once the
    // fixture's listing of it is made).
    let fx = fixture_with(0, &["a-korean.txt"]);
    let path = std::path::Path::new(&fx.root).join("a-korean.txt");
    std::fs::write(path, hangul_utf16()).unwrap();
    let (mut drv, mut win) = window(&fx, 100, 30);
    for _ in 0..4 {
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    }
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 2);
    the_same_cells(&fx, &drv, &mut win, "view 1");
    drv.key(&mut win, Key::F8, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 2);
    assert!(
        terminal_of(&drv.ctx, &mut win, 100, 30, true)
            .text()
            .contains("Encoding"),
        "F8's list is open"
    );
    the_same_cells(&fx, &drv, &mut win, "F8's list");
}

/// TS-6 (10 §10, T38) on the text viewer: the glyphs it draws for control characters — BEL's
/// `•`, CAN's `↑`, a C1 control's `·` — are sopen's own (the CP437 table), so an ambiguous-wide
/// terminal is sent their stand-ins, substituted where the viewer lays its row (the printer cannot
/// tell them from content); the same characters written in the file stay as they are, content
/// laid in two cells; the bar's own words (` · `, a cut's `…`) are sent theirs too, so the stats
/// stay whole. A terminal that draws `─` in one cell gets the glyphs.
#[test]
fn ts6_the_text_viewers_control_glyphs_have_their_stand_ins() {
    // A name longer than the bar's room for the title, so the title is cut wherever the
    // host keeps its temporary folders.
    let name = format!("a-ctrl-{}.txt", "x".repeat(80));
    let fx = fixture_with(0, &[name.as_str()]);
    let path = std::path::Path::new(&fx.root).join(&name);
    std::fs::write(path, "a\u{7}b\u{18}c\u{85}d ♥• end\n").unwrap();
    let (mut drv, mut win) = window(&fx, 100, 30);
    for _ in 0..4 {
        drv.key(&mut win, Key::ArrowDown, Modifiers::NONE);
    }
    drv.key(&mut win, Key::Enter, Modifiers::NONE);
    drv.until(&mut win, 20, |w| w.ready());
    drv.idle(&mut win, 2);
    let narrow_vt = terminal_of(&drv.ctx, &mut win, 100, 30, true);
    let text = narrow_vt.text();
    assert!(text.contains("a•b↑c·d ♥• end"), "the glyphs:\n{text}");
    // An ambiguous-wide terminal on the same context: its probe says so to the passes.
    let tape = Tape::default();
    {
        let mut s = tape.0.borrow_mut();
        s.size = (100, 30);
        s.ambiguous = true;
    }
    let out = Bytes::default();
    let mut b = Backend::new(tape, out.clone(), drv.ctx.clone(), true);
    b.offset = later();
    b.enter().unwrap();
    b.pass(&mut win, vec![]).unwrap();
    b.draw().unwrap();
    let mut wide = Vt::wide(100, 30);
    wide.feed(&out.0.borrow());
    let text = wide.text();
    let row = (0..wide.rows)
        .map(|y| wide.row(y))
        .find(|r| r.contains("end"))
        .unwrap_or_else(|| panic!("the row:\n{text}"));
    assert!(row.contains("a.b^c.d "), "the stand-ins: {row:?}\n{text}");
    let content = row.split("d ").nth(1).unwrap_or_default();
    assert!(
        content.starts_with('♥') && content.contains('•'),
        "the file's own ♥ and • as they are: {row:?}"
    );
    // The bar's own words keep their cells: its ` · ` as ` . `, the stats whole, the cut title's
    // `…` a `~`.
    assert!(
        wide.row(1)
            .contains(" 1 lines . 20 B . plain text . UTF-8 "),
        "the bar whole: {:?}",
        wide.row(1)
    );
    assert!(wide.row(1).starts_with(" ~"), "{:?}", wide.row(1));
    b.leave();
}
