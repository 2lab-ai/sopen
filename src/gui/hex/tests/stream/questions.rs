//! X3-questions (10 §17): the hex window's questions and the rows below its grid in cells,
//! through `ops::dialog` — the close prompt, the first Save's question above 256 MiB, Write in
//! place, the refusal's offer, the live question about a temp, the recovery rows — with the
//! same words and actions as the egui ones, each reached by keyboard and by click.

use super::*;
use crate::gui::browser::shown_wrapped;
use crate::gui::cells;

/// Whether the last pass drew `text` in cells, on one row of a cell layer.
fn in_cells(drv: &Driver, text: &str) -> bool {
    !cells::find(&cells::layers(&drv.ctx), text).is_empty()
}

/// Whether an egui galley of the last pass holds `text` (the cell painter's glyphs are one
/// character each): a text a terminal never shows.
fn in_a_galley(drv: &Driver, text: &str) -> bool {
    drv.galleys().iter().any(|(_, g)| g.text().contains(text))
}

/// The colours of the cells that show `text` (its first place).
fn pens_of(drv: &Driver, text: &str) -> Option<(Option<Color32>, Option<Color32>)> {
    let layers = cells::layers(&drv.ctx);
    let hit = cells::find(&layers, text).into_iter().next()?;
    let l = &layers[hit.layer];
    let row = &l.seen()[usize::from(hit.cells.y - l.buf.area.y)];
    let first = &row[usize::from(hit.cells.x - l.buf.area.x)];
    Some((first.fg, first.bg))
}

/// The bar's colours: the control with the keyboard.
fn focused_pen() -> (Option<Color32>, Option<Color32>) {
    let p = chrome::PALETTE;
    (Some(p.bar_text), Some(p.bar))
}

/// `Tab` (Shift with it, `back`) pressed once, a pass after it.
fn tab(ed: &mut Ed, back: bool) {
    let mods = if back {
        Modifiers::SHIFT
    } else {
        Modifiers::NONE
    };
    ed.key(Key::Tab, mods);
    ed.frames(1);
}

/// 07 §10 in cells: the close prompt's question, its four buttons and — cells have no hover —
/// where Discard deletes the kept edits, as a row under them, all cells and no egui text; Tab
/// walks the buttons (the bar's colours on the one with the keyboard), Enter on `[ Cancel ]`
/// keeps the window and the edit, Esc does too, Space on `[ Discard ]` closes it and drops the
/// journal.
#[test]
fn the_close_prompt_is_cells_and_its_keys_act() {
    let mut ed = ed(&body());
    ed.typed("AB");
    ed.kept();
    let dir = ed.journals().pop().expect("a journal");
    assert!(!ed.win.close_requested());
    ed.ready();
    ed.frames(1);
    let question = format!("Save the changes to {} before closing?", ed.shown());
    assert!(shown_wrapped(&ed.drv, &question), "{question}");
    assert!(!in_a_galley(&ed.drv, "before closing?"), "an egui text");
    assert!(in_cells(&ed.drv, " Unsaved changes "), "its title");
    let buttons = "[ Save ]  [ Save As… ]  [ Discard ]  [ Cancel ]";
    assert!(in_cells(&ed.drv, buttons), "{buttons}");
    let deletes = format!(
        "Discard deletes the kept edits at {}",
        crate::gui::binary::tilde(&dir)
    );
    assert!(shown_wrapped(&ed.drv, &deletes), "{deletes}");
    // Tab walks the buttons; Enter on Cancel: the window stays, with its edit.
    for button in ["[ Save ]", "[ Save As… ]", "[ Discard ]", "[ Cancel ]"] {
        tab(&mut ed, false);
        // The button with the keyboard is the one in the bar's colours (the prompt's own; the
        // toolbar's `[ Save ]` and `[ Save As… ]` are under it, in theirs).
        let hits = cells::find(&cells::layers(&ed.drv.ctx), button);
        let last = *hits.last().unwrap();
        let l = &cells::layers(&ed.drv.ctx)[last.layer];
        let row = &l.seen()[usize::from(last.cells.y - l.buf.area.y)];
        let cell = &row[usize::from(last.cells.x - l.buf.area.x)];
        assert_eq!(
            (cell.fg, cell.bg),
            focused_pen(),
            "{button} has the keyboard"
        );
    }
    ed.key(Key::Enter, Modifiers::NONE);
    assert!(!ed.win.prompt && !ed.win.close_ok && ed.win.doc.unsaved());
    // Esc: as Cancel.
    assert!(!ed.win.close_requested());
    ed.ready();
    ed.frames(1);
    ed.key(Key::Escape, Modifiers::NONE);
    ed.frames(1);
    assert!(
        !ed.win.prompt && !ed.win.close_ok && ed.win.doc.unsaved(),
        "Esc"
    );
    // Space on Discard: closes, the journal goes with the window.
    assert!(!ed.win.close_requested());
    ed.ready();
    ed.frames(1);
    for _ in 0..3 {
        tab(&mut ed, false);
    }
    ed.key(Key::Space, Modifiers::NONE);
    assert!(ed.win.discard && ed.win.close_ok, "Discard");
    let win = std::mem::replace(
        &mut ed.win,
        window_on(ed.tmp.path(), &ed.remote, Side::Local, None, &ed.moved).1,
    );
    drop(win);
    assert!(!dir.exists(), "the kept edits are gone");
}

/// The prompt's `[ Save ]` by keyboard: the Save runs, the window waits for it, then closes;
/// `[ Save As… ]` by keyboard opens the modal (in cells), the window closing once it landed.
#[test]
fn the_close_prompts_save_and_save_as_by_keyboard() {
    let mut ed = ed(&body());
    ed.typed("AB");
    assert!(!ed.win.close_requested());
    ed.ready();
    ed.frames(1);
    tab(&mut ed, false);
    ed.key(Key::Enter, Modifiers::NONE);
    ed.drv
        .until(&mut ed.win, 60, |w| w.job.is_none() && w.close_ok);
    assert_eq!(std::fs::read(&ed.remote).unwrap()[0], 0xAB, "saved");
    // Save As…
    let mut ed = ed_side(Some(&body()), Side::Local);
    ed.typed("CD");
    let out = ed.tmp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    ed.win.save_as_folder = Some(out.clone());
    assert!(!ed.win.close_requested());
    ed.ready();
    ed.frames(1);
    tab(&mut ed, false);
    tab(&mut ed, false);
    ed.key(Key::Enter, Modifiers::NONE);
    assert!(ed.win.save_as.is_some(), "the modal");
    ed.frames(2);
    assert!(in_cells(&ed.drv, " Save As "), "in cells");
    assert!(
        entries(&out).is_empty(),
        "the Enter that pressed Save As… saved too"
    );
    assert!(
        in_cells(
            &ed.drv,
            "Save model.bin with your edits to a new local file"
        ),
        "its words"
    );
    ed.key(Key::Enter, Modifiers::NONE);
    ed.drv.until(&mut ed.win, 60, |w| w.save_as.is_none());
    assert!(ed.win.close_ok, "closes after the Save As landed");
    assert_eq!(
        std::fs::read(out.join("model.edited.bin")).unwrap()[0],
        0xCD
    );
}

/// 07 §4.5 in cells: the first Save above 256 MiB asks, its words wrapped by cells; Esc cancels,
/// Enter on a focused `[ Cancel ]` cancels (a button keeps Enter as its own click), and Enter
/// with no button focused saves.
#[test]
fn the_first_save_question_is_cells_and_its_keys_act() {
    let mut ed = ed_side(None, Side::Local);
    ed.goto(E1_AT);
    ed.typed("DEADBEEF");
    let ask = |ed: &mut Ed| {
        ed.key(Key::S, command(false));
        ed.frames(2);
        assert!(ed.win.confirm.is_some(), "no question");
    };
    ask(&mut ed);
    let asked = format!(
        "Save to {} ({})? sopen checks its size, modification time and the pages under your \
         edits. A program writing to the file while it saves can lose that write.",
        ed.shown(),
        crate::human(ed.win.doc.size())
    );
    assert!(shown_wrapped(&ed.drv, &asked), "{asked}");
    assert!(!in_a_galley(&ed.drv, "can lose that write"), "an egui text");
    assert!(in_cells(&ed.drv, "[ Save (Enter) ]  [ Cancel ]"));
    ed.key(Key::Escape, Modifiers::NONE);
    assert!(ed.win.confirm.is_none() && ed.win.job.is_none(), "Esc");
    let starts = ed.win.save_starts;
    ask(&mut ed);
    tab(&mut ed, false);
    tab(&mut ed, false);
    assert_eq!(
        pens_of(&ed.drv, "[ Cancel ]"),
        Some(focused_pen()),
        "Cancel has the keyboard"
    );
    ed.key(Key::Enter, Modifiers::NONE);
    assert!(ed.win.confirm.is_none(), "answered");
    assert_eq!(ed.win.save_starts, starts, "Enter on Cancel saves nothing");
    ask(&mut ed);
    ed.key(Key::Enter, Modifiers::NONE);
    assert!(ed.win.confirm.is_none() && ed.win.confirmed, "Enter saves");
    ed.drv.until(&mut ed.win, 120, |w| w.job.is_none());
    ed.frames(2);
    assert!(
        ed.win.status.text.starts_with("saved "),
        "{}",
        ed.win.status.text
    );
}

/// 07 §4.1 and §4.4 in cells: a refusal's row says why, `[ Write in place… ]` and
/// `[ Save As… ]` under it; Tab from the ASCII column reaches the first, Enter opens Write in
/// place's dialog — its words wrapped by cells, `[ Write in place ]  [ Cancel ]` — and Esc
/// closes it; a click on the row's button opens it again and `[ Cancel ]` closes it.
#[test]
fn the_offer_row_and_write_in_place_are_cells() {
    let mut ed = ed(&body());
    ed.typed("AB");
    let why = "the server cannot copy this file (a test's refusal)";
    ed.win.offer = Some(why.to_string());
    ed.frames(2);
    assert!(shown_wrapped(&ed.drv, &format!("not saved: {why}")));
    assert!(!in_a_galley(&ed.drv, "a test's refusal"), "an egui text");
    assert!(in_cells(&ed.drv, "[ Write in place… ]  [ Save As… ]"));
    let p = chrome::PALETTE;
    assert_eq!(
        pens_of(&ed.drv, "not saved: "),
        Some((Some(p.error), Some(p.back))),
        "the error colour"
    );
    // By keyboard: Tab to the ASCII column, Tab to the row's first button, Enter.
    ed.win.place(ed.win.cursor.at, Pane::Hex);
    tab(&mut ed, false);
    assert_eq!(ed.win.cursor.pane, Pane::Ascii);
    tab(&mut ed, false);
    assert_eq!(pens_of(&ed.drv, "[ Write in place… ]"), Some(focused_pen()));
    assert!(ed.win.cursor.pane == Pane::Ascii, "the column stays");
    ed.key(Key::Enter, Modifiers::NONE);
    ed.frames(1);
    let question = ed
        .win
        .in_place_ask
        .clone()
        .expect("Write in place's dialog");
    assert!(shown_wrapped(&ed.drv, &question), "{question}");
    assert!(in_cells(&ed.drv, " Write in place "), "its title");
    assert!(in_cells(&ed.drv, "[ Write in place ]  [ Cancel ]"));
    ed.key(Key::Escape, Modifiers::NONE);
    ed.frames(1);
    assert!(ed.win.in_place_ask.is_none(), "Esc closes it");
    // By click.
    ed.drv.click_text(&mut ed.win, "[ Write in place… ]");
    assert!(ed.win.in_place_ask.is_some());
    ed.frames(1);
    ed.drv.click_text_on_top(&mut ed.win, "[ Cancel ]");
    assert!(ed.win.in_place_ask.is_none(), "Cancel closes it");
    assert!(ed.win.job.is_none(), "nothing written");
    // `[ Save As… ]`, by keyboard (the ASCII column, then the row's two buttons): the modal.
    tab(&mut ed, false);
    tab(&mut ed, false);
    ed.key(Key::Enter, Modifiers::NONE);
    assert!(
        ed.win.save_as.as_ref().is_some_and(|(m, ..)| !m.busy()),
        "Save As… opens the modal, and the Enter that pressed it saves nothing"
    );
}

/// 07 §3 in cells: an earlier session's row — its line in the error colour, `[ Restore ]
/// [ Later ]  [ Discard ]` — as cells, no egui text. Tab from the ASCII column takes the keyboard
/// to `[ Restore ]`, Tab and Shift+Tab walk the buttons, past the last back to the grid's hex
/// column; Esc gives the keyboard back to the grid, which types again; Enter on `[ Restore ]`
/// restores; Space on `[ Discard ]` discards; a click on `[ Later ]` puts the row away.
#[test]
fn the_recovery_rows_are_cells_reached_by_keyboard_and_click() {
    let mut ed = ed(&body());
    ed.typed("AB");
    ed.kept();
    let dir = ed.journals().pop().unwrap();
    ed.reopen();
    assert_eq!(ed.rows().len(), 1, "{:?}", ed.rows());
    let line = ed.rows()[0].clone();
    assert!(shown_wrapped(&ed.drv, &line), "{line}");
    assert!(!in_a_galley(&ed.drv, "unsaved edits from"), "an egui text");
    assert!(in_cells(&ed.drv, "[ Restore ]  [ Later ]  [ Discard ]"));
    let p = chrome::PALETTE;
    assert_eq!(
        pens_of(&ed.drv, "unsaved edits from "),
        Some((Some(p.error), Some(p.back)))
    );
    // Tab into the row and through it, back to the grid.
    ed.win.place(0, Pane::Ascii);
    tab(&mut ed, false);
    assert_eq!(pens_of(&ed.drv, "[ Restore ]"), Some(focused_pen()));
    assert_eq!(
        crate::gui::Window::view_state(&mut ed.win),
        crate::view_keys::State::Question,
        "the view keys leave every key to the row"
    );
    tab(&mut ed, false);
    assert_eq!(pens_of(&ed.drv, "[ Later ]"), Some(focused_pen()));
    tab(&mut ed, true);
    assert_eq!(pens_of(&ed.drv, "[ Restore ]"), Some(focused_pen()));
    for _ in 0..3 {
        tab(&mut ed, false);
    }
    assert_eq!(ed.drv.ctx.memory(|m| m.focused()), Some(grid_id()));
    assert_eq!(
        ed.win.cursor.pane,
        Pane::Hex,
        "past the last: the hex column"
    );
    // Shift+Tab from the hex column: the last button; Esc: the grid again.
    tab(&mut ed, true);
    assert_eq!(pens_of(&ed.drv, "[ Discard ]"), Some(focused_pen()));
    ed.key(Key::Escape, Modifiers::NONE);
    ed.frames(1);
    assert_eq!(ed.drv.ctx.memory(|m| m.focused()), Some(grid_id()));
    assert_eq!(ed.rows().len(), 1, "Esc answered nothing");
    // Typing while a button has the keyboard types nothing.
    tab(&mut ed, true);
    ed.typed("7");
    assert_eq!(ed.win.doc.changed_count(), 0, "a key typed into a byte");
    ed.key(Key::Escape, Modifiers::NONE);
    // Enter on Restore.
    ed.win.place(0, Pane::Ascii);
    tab(&mut ed, false);
    ed.key(Key::Enter, Modifiers::NONE);
    ed.goto(0);
    ed.ready();
    assert_eq!(ed.win.doc.byte(0), Some(0xAB), "restored");
    assert!(ed.rows().is_empty());
    // Space on Discard, after another kill.
    ed.kept();
    ed.reopen();
    ed.win.place(0, Pane::Hex);
    tab(&mut ed, true);
    assert_eq!(pens_of(&ed.drv, "[ Discard ]"), Some(focused_pen()));
    ed.key(Key::Space, Modifiers::NONE);
    ed.frames(2);
    assert!(ed.rows().is_empty() && !dir.exists(), "discarded");
    assert_eq!(ed.drv.ctx.memory(|m| m.focused()), Some(grid_id()));
    // A click on Later.
    let mut ed2 = ed_side(Some(&body()), Side::Local);
    ed2.typed("CD");
    ed2.kept();
    ed2.reopen();
    ed2.drv.click_text(&mut ed2.win, "[ Later ]");
    assert!(ed2.rows().is_empty(), "Later puts the row away");
}

/// 07 §3.1 in cells: a journal's row that asks about a temp shows the question under its line,
/// `[ Remove it ]  [ Keep it ]  │  [ Restore ]  [ Later ]  [ Discard ]`, Restore and Discard
/// off (the dialog's weak colour) with the reason a dim row under them — cells have no hover —
/// and Tab walks only the buttons that act; Enter on `[ Keep it ]` answers.
#[test]
fn a_rows_question_about_a_temp_is_cells_and_its_reason_a_row() {
    let mut ed = ed(&body());
    ed.typed("AB");
    kill_at(&mut ed, patch::Step::Copy(1));
    ed.reopen();
    let row = cell_row_of(&ed.drv, "[ Remove it ]").expect("the question's answers");
    assert!(
        row.contains("[ Remove it ]  [ Keep it ]  │  [ Restore ]  [ Later ]  [ Discard ]"),
        "{row}"
    );
    let p = chrome::PALETTE;
    assert_eq!(
        pens_of(&ed.drv, "[ Restore ]"),
        Some((Some(p.dialog_weak), Some(p.dialog_back))),
        "off"
    );
    let why = "Restore and Discard wait: first answer the question about the interrupted \
               save's file";
    assert!(shown_wrapped(&ed.drv, why), "the reason, a row");
    assert_eq!(
        pens_of(&ed.drv, "Restore and Discard wait"),
        Some((Some(p.dim), Some(p.back)))
    );
    // Tab: Remove it, Keep it, Later — never an off button.
    ed.win.place(0, Pane::Ascii);
    for button in ["[ Remove it ]", "[ Keep it ]", "[ Later ]"] {
        tab(&mut ed, false);
        assert_eq!(pens_of(&ed.drv, button), Some(focused_pen()), "{button}");
    }
    tab(&mut ed, true);
    ed.key(Key::Enter, Modifiers::NONE);
    ed.drv.until(&mut ed.win, 20, |w| w.removing.is_none());
    ed.frames(2);
    assert!(
        !ed.rows().iter().any(|l| l.ends_with("— remove it?")),
        "Keep it answered: {:?}",
        ed.rows()
    );
    assert!(in_cells(&ed.drv, "[ Restore ]  [ Later ]  [ Discard ]"));
    assert!(!shown_wrapped(&ed.drv, why), "no reason once answered");
}

/// 07 §3.1, the live question in cells: the next Save that finds a temp it cannot prove its own
/// asks under the grid, `[ Remove it ]  [ Keep it ]`; Tab reaches them and Enter on
/// `[ Keep it ]` answers, the Save going on.
#[test]
fn the_live_question_is_cells_and_answered_by_keyboard() {
    let body = body();
    let mut ed = ed_side(Some(&body), Side::Local);
    ed.typed("AB");
    let (temp, _) = cut_off_leaving_a_temp(&mut ed);
    ed.save();
    let line = live_question(&ed).expect("the question");
    assert!(shown_wrapped(&ed.drv, &line), "{line}");
    assert!(in_cells(&ed.drv, "[ Remove it ]  [ Keep it ]"));
    ed.win.place(0, Pane::Ascii);
    tab(&mut ed, false);
    tab(&mut ed, false);
    assert_eq!(pens_of(&ed.drv, "[ Keep it ]"), Some(focused_pen()));
    ed.key(Key::Enter, Modifiers::NONE);
    ed.drv.until(&mut ed.win, 30, |w| w.answering.is_none());
    ed.frames(2);
    assert!(live_question(&ed).is_none(), "answered");
    assert!(temp.exists(), "kept");
}

/// The hex view in the file browser (08 D35, viewing): an earlier session's journal is offered
/// in a row of cells under the grid; Tab twice (the ASCII column, then the row) and Enter
/// restore it — the view keys take neither Enter (it would start editing) nor Tab while the row
/// has the keyboard.
#[test]
fn in_the_browser_tab_and_enter_restore_an_earlier_sessions_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("blob.bin"), body()).unwrap();
    let browser = || {
        crate::gui::browser::Browser::new(crate::gui::browser::Start {
            fs: Box::new(LocalFs::new()),
            connect: Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)),
            label: None,
            destination: None,
            abs: root.to_str().unwrap().to_string(),
            cache: tmp.path().join("cache"),
            state_dir: tmp.path().join("state"),
            launch: Arc::new(|abs: &str, _| Ok(format!("opened {abs}"))),
            git: None,
        })
    };
    let open = |drv: &mut Driver, b: &mut crate::gui::browser::Browser| {
        drv.until(b, 20, |b| b.ready());
        drv.key(b, Key::ArrowDown, Modifiers::NONE);
        drv.until(b, 20, |b| b.ready());
        drv.key(b, Key::Enter, Modifiers::NONE);
        drv.until(b, 20, |b| b.ready());
        drv.idle(b, 2);
        drv.press_typed(b, Key::Num2, "2");
        drv.until(b, 20, |b| b.ready());
        drv.idle(b, 2);
        assert!(drv.find_text(VIEWING).is_some(), "the hex view, viewing");
    };
    // A first session edits a byte and ends with it unsaved: its journal stays.
    let mut b = browser();
    let mut drv = Driver::new(1120.0, 480.0);
    open(&mut drv, &mut b);
    drv.key(&mut b, Key::Enter, Modifiers::NONE);
    drv.idle(&mut b, 1);
    drv.frame(&mut b, vec![Event::Text("A".into())], Modifiers::NONE);
    drv.frame(&mut b, vec![Event::Text("B".into())], Modifiers::NONE);
    drv.idle(&mut b, 30);
    std::thread::sleep(Duration::from_millis(300));
    drv.idle(&mut b, 3);
    drop(b);
    // The next: its row, then Tab, Tab, Enter.
    let mut b = browser();
    let mut drv = Driver::new(1120.0, 480.0);
    open(&mut drv, &mut b);
    let deadline = Instant::now() + Duration::from_secs(20);
    while drv.find_text("[ Restore ]").is_none() {
        assert!(Instant::now() < deadline, "the earlier session's row");
        drv.idle(&mut b, 1);
        std::thread::sleep(Duration::from_millis(10));
    }
    drv.key(&mut b, Key::Tab, Modifiers::NONE);
    drv.idle(&mut b, 1);
    drv.key(&mut b, Key::Tab, Modifiers::NONE);
    drv.idle(&mut b, 1);
    assert_eq!(
        pens_of(&drv, "[ Restore ]"),
        Some(focused_pen()),
        "the row's"
    );
    drv.key(&mut b, Key::Enter, Modifiers::NONE);
    drv.until(&mut b, 20, |b| b.ready());
    drv.idle(&mut b, 3);
    assert!(drv.find_text("[ Restore ]").is_none(), "answered");
    assert!(
        in_cells(&drv, "AB 01 02 03"),
        "restored: the first row's bytes"
    );
    // Still viewing: Enter started no editing, so a digit types nothing.
    drv.press_typed(&mut b, Key::Num7, "7");
    drv.idle(&mut b, 2);
    assert!(in_cells(&drv, "AB 01 02 03"), "Enter started editing");
}

/// Raising the close prompt keeps the edits at once (07 §3), on a worker — not at the
/// schedule's next tick, and never on the frame, which a held journal would stop — and the window
/// is not settled until they are kept (the prompt gains where Discard deletes them, a row that
/// moves its box). A background generation's end repaints the window — the context of its last
/// pass — so nothing waits for another input to show `kept at` (a terminal runs a pass only when
/// asked).
#[test]
fn closing_keeps_the_edits_at_once_and_a_generations_end_repaints() {
    let mut ed = ed(&body());
    ed.typed("AB");
    assert_eq!(ed.win.schedule.status(), Keep::Keeping, "not kept yet");
    // A context of its own, where nothing else asks for a repaint, as the last pass's.
    let woken = egui::Context::default();
    let asked = Arc::new(Mutex::new(0usize));
    let seen = asked.clone();
    woken.set_request_repaint_callback(move |_| *seen.lock().unwrap() += 1);
    ed.win.wake = Some(woken);
    assert!(!ed.win.close_requested());
    assert!(
        ed.win.writing.is_some(),
        "started at the prompt, on a worker"
    );
    assert!(!ed.win.ready(), "not settled while it is on its way");
    let deadline = Instant::now() + Duration::from_secs(10);
    while *asked.lock().unwrap() == 0 {
        assert!(Instant::now() < deadline, "its end repainted nothing");
        std::thread::sleep(Duration::from_millis(5));
    }
    ed.frames(1);
    assert_eq!(ed.win.schedule.status(), Keep::Kept, "kept");
    assert!(ed.win.ready(), "settled");
    let dir = ed.win.journal_dir.clone().expect("its journal");
    let deletes = format!(
        "Discard deletes the kept edits at {}",
        crate::gui::binary::tilde(&dir)
    );
    assert!(shown_wrapped(&ed.drv, &deletes), "on the prompt");
    // An edit no pass has seen yet (a state's patch, as `close=ask` finds it): kept at the
    // prompt too.
    let mut ed = ed_side(Some(&body()), Side::Local);
    let cursor = ed.win.cursor;
    assert!(ed.win.doc.set(0, 0xAB, cursor));
    assert!(!ed.win.close_requested());
    assert!(ed.win.writing.is_some(), "an edit no pass saw");
    ed.drv.until(&mut ed.win, 20, |w| w.ready());
    assert_eq!(ed.win.schedule.status(), Keep::Kept);
}
