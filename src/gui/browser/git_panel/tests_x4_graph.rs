//! X4-graph's tests (10 §17): the git panel's frame and graph in cells (03 §4.2–§4.4) — the
//! frame titled with the top level, the column heads, the rows with their lanes (X4-lanes),
//! labels, subject, Date, Author and Commit cut by cells, the bar, the marks, the label cursor,
//! the scroll bar, the rule and the foot; clicks, the right click, the wheel and the paging line by
//! cells; the cells it hands the other parts, and what it blanks under the dialogs and the help.

use super::tests::{commit, day, fixture_view, label, Fx};
use super::*;
use crate::git::graph::RowStream;
use crate::git::model::{CommitRow, ObjectType, Oid, RefKind, RefLabel};
use crate::gui::cells::{Layer, Seen};
use crate::gui::chrome::git::LOOK;
use crate::gui::chrome::PALETTE;
use eframe::egui::{Event, Modifiers, PointerButton, Pos2, Vec2};
use std::sync::Arc;

// ---------------------------------------------------------------------------------------------
// Reading the panel's cells.

/// The panel's grid: the one its host laid over the window's central area.
fn grid_of(fx: &Fx) -> Grid {
    Grid::over(fx.host.rect)
}

/// The panel's layer of cells: the one over all of its side.
pub(super) fn panel(fx: &Fx) -> Arc<Layer> {
    let grid = grid_of(fx);
    cells::layers(&fx.drv.ctx)
        .into_iter()
        .find(|l| l.buf.area == grid.all() && l.origin == grid.origin)
        .expect("the panel's cells over its side")
}

/// Every cell of the panel, a row at a time (a wide glyph's second cell shows nothing).
fn seen(fx: &Fx) -> Vec<Vec<Seen>> {
    panel(fx).seen()
}

/// Row `y` of the panel from column `from` to `to` (exclusive), as text.
fn text(fx: &Fx, y: u16, from: u16, to: u16) -> String {
    let rows = seen(fx);
    rows[usize::from(y)][usize::from(from)..usize::from(to)]
        .iter()
        .map(|c| c.symbol.as_str())
        .collect()
}

/// Row `y` of the panel, whole.
fn line(fx: &Fx, y: u16) -> String {
    text(fx, y, 0, grid_of(fx).cols)
}

/// The cell at column `x` of row `y`.
fn cell(fx: &Fx, x: u16, y: u16) -> Seen {
    seen(fx)[usize::from(y)][usize::from(x)].clone()
}

/// The row (of the panel's grid) the graph's row `i` is drawn on.
fn y_of(fx: &Fx, i: usize) -> u16 {
    let g = fx.view().geometry().expect("a pass drew the graph");
    g.row(i).expect("row drawn").y
}

/// The Graph column (its first `n` cells) of each drawn row.
pub(super) fn graph_cells(fx: &Fx, n: usize) -> Vec<Vec<Seen>> {
    let g = fx.view().geometry().expect("a pass drew the graph");
    let rows = seen(fx);
    let x = usize::from(g.rows.x);
    (0..fx.view().on_screen().len())
        .map(|k| rows[usize::from(g.rows.y) + k][x..x + n].to_vec())
        .collect()
}

/// The columns the graph's rows are laid in.
fn columns(fx: &Fx) -> graph_view::Columns {
    let g = fx.view().geometry().expect("a pass drew the graph");
    graph_view::Columns::new(usize::from(g.rows.width), fx.view().graph().width().max(1))
}

/// Where `text` first stands on row `y`: its first cell's column.
fn at(fx: &Fx, y: u16, text: &str) -> Option<u16> {
    let row = &seen(fx)[usize::from(y)];
    let (mut s, mut cols) = (String::new(), vec![]);
    for (x, c) in row.iter().enumerate() {
        for ch in c.symbol.chars() {
            s.push(ch);
            cols.push(x as u16);
        }
    }
    let start = s.find(text)?;
    Some(cols[s[..start].chars().count()])
}

/// The graph's row whose subject is `subject`.
fn index_of(fx: &Fx, subject: &str) -> usize {
    (0..fx.view().rows().len())
        .find(|&i| fx.view().line(i).is_some_and(|l| l.subject == subject))
        .unwrap_or_else(|| panic!("no row {subject:?}"))
}

/// The centre of cell (`x`, `y`) of the panel's grid, in points.
fn centre(fx: &Fx, x: u16, y: u16) -> Pos2 {
    grid_of(fx).rect(Cells::new(x, y, 1, 1)).center()
}

/// A press and a release of `button` at `p`, as one pass's events.
fn click_events(p: Pos2, button: PointerButton) -> Vec<Event> {
    let button_at = |pressed| Event::PointerButton {
        pos: p,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(p), button_at(true), button_at(false)]
}

/// A key's press and release, as one pass's events.
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

/// A busy history of `n` commits, the newest first: a side line merged back every sixth commit
/// and another every tenth (three to four lanes), a local branch with a Hangul name every ninth
/// commit, a remote branch every seventeenth and a tag every thirteenth, Hangul and Latin
/// authors and subjects, the uncommitted row on top.
fn busy_view(n: u32) -> GitView {
    let id = |k: u32| Oid::parse(&format!("{k:040x}")).unwrap();
    let commits: Vec<CommitRow> = (1..=n)
        .rev()
        .map(|k| {
            let mut parents = Vec::new();
            if k > 1 {
                parents.push(id(k - 1));
            }
            if k % 6 == 0 && k > 6 {
                parents.push(id(k - 4));
            }
            if k % 10 == 0 && k > 10 {
                parents.push(id(k - 7));
            }
            let author = if k % 2 == 0 { "김철수" } else { "Ann Lee" };
            CommitRow {
                oid: id(k),
                parents,
                author_name: author.into(),
                author_email: "dev@example.com".into(),
                author_time: day(i64::from(k)),
                commit_time: day(i64::from(k)),
                subject: format!("커밋 {k}: 한글 제목과 an English subject"),
            }
        })
        .collect();
    let at = |refname: String, k: u32, kind: RefKind| RefLabel {
        object: id(k),
        object_type: ObjectType::Commit,
        peeled: None,
        refname,
        kind,
        upstream: None,
        track: String::new(),
        lossy: false,
    };
    let mut refs = vec![at("refs/heads/main".into(), n, RefKind::LocalBranch)];
    for k in 1..n {
        if k % 9 == 0 {
            refs.push(at(format!("refs/heads/기능-{k}"), k, RefKind::LocalBranch));
        }
        if k % 13 == 0 {
            refs.push(at(format!("refs/tags/v{k}"), k, RefKind::Tag));
        }
        if k % 17 == 0 {
            refs.push(at(
                format!("refs/remotes/origin/b{k}"),
                k,
                RefKind::RemoteBranch,
            ));
        }
    }
    let mut view = GitView::new("box:/srv/저장소");
    view.set_remotes(vec!["origin".into()]);
    view.set_refs(refs, HeadRef::Branch("refs/heads/main".into()));
    let mut stream = RowStream::new(Some(id(n)), true, Vec::new(), true);
    view.set_rows(stream.page(commits), false, 3);
    view.set_paging(Paging::AllLoaded);
    view
}

/// The panel's release frame cost at 1000×700 — 3,000 commits in three to four lanes, labels of
/// three kinds, Hangul authors and subjects, the panel over the whole window — run alone:
/// `cargo test --release --lib -- --ignored --exact
/// gui::browser::git_panel::tests_x4_graph::frame_cost_at_1000_by_700 --nocapture`. Printed, no
/// budget set.
#[test]
#[ignore]
fn frame_cost_at_1000_by_700() {
    use std::time::Instant;
    let mut fx = Fx::new(1000.0, 700.0, busy_view(3_000));
    fx.drv.idle(&mut fx.host, 20);
    let n = 400;
    let started = Instant::now();
    for _ in 0..n {
        fx.drv.frame(&mut fx.host, Vec::new(), Modifiers::NONE);
    }
    println!(
        "the git panel's frame at 1000x700: {:?}",
        started.elapsed() / n
    );
}

// ---------------------------------------------------------------------------------------------
// The frame, the heads, the rows and the foot (03 §4.2–§4.4).

/// 03 §4.2: the panel is one layer of cells over its side — its double frame titled with the top
/// level (` [filtered]` while a filter differs), the heads dim at `Columns::new`'s cells, each row's
/// labels, subject, Date, Author and Commit at theirs, the scroll bar on the right border, the rule
/// joined to the frame and the paging line under it.
#[test]
fn the_frame_heads_rows_and_foot_are_cells() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let g = grid_of(&fx);
    let (w, h) = (g.cols, g.rows);
    let top = line(&fx, 0);
    assert!(top.starts_with('╔') && top.ends_with('╗'), "{top:?}");
    assert!(top.contains(" box:/srv/repo "), "the title: {top:?}");
    let cols = columns(&fx);
    let x = |c: Option<usize>| 1 + c.expect("the column is shown") as u16;
    assert_eq!(at(&fx, 1, "Description"), Some(x(Some(cols.desc))));
    assert_eq!(at(&fx, 1, "Date"), Some(x(cols.date)));
    assert_eq!(at(&fx, 1, "Author"), Some(x(cols.author)));
    assert_eq!(at(&fx, 1, "Commit"), Some(x(cols.commit)));
    assert_eq!(
        cell(&fx, x(Some(cols.desc)), 1).fg,
        Some(LOOK.dim),
        "heads dim"
    );
    let y = y_of(&fx, 1);
    let desc = x(Some(cols.desc));
    assert_eq!(
        at(&fx, y, "►main↔origin,up"),
        Some(desc),
        "{:?}",
        line(&fx, y)
    );
    let local = cell(&fx, desc, y);
    assert_eq!(
        (local.fg, local.bg),
        (Some(LOOK.local.text), Some(LOOK.local.back))
    );
    let subject = desc + "►main↔origin,up".chars().count() as u16 + 1;
    assert_eq!(at(&fx, y, "Merge branch 'feature'"), Some(subject));
    assert_eq!(
        cell(&fx, subject - 1, y).bg,
        Some(PALETTE.back),
        "a blank between"
    );
    assert_eq!(
        at(&fx, y, &super::tests::date_of(day(9))),
        Some(x(cols.date))
    );
    assert_eq!(at(&fx, y, "Ann"), Some(x(cols.author)));
    assert_eq!(at(&fx, y, "09090909"), Some(x(cols.commit)));
    // The uncommitted row: `*` for its author and commit, its date blank.
    let y0 = y_of(&fx, 0);
    assert_eq!(text(&fx, y0, x(cols.author), x(cols.author) + 1), "*");
    assert_eq!(text(&fx, y0, x(cols.commit), x(cols.commit) + 1), "*");
    assert_eq!(text(&fx, y0, x(cols.date), x(cols.date) + 16).trim(), "");
    // The scroll bar on the border beside the rows, the rule, the foot.
    let rule = h - 3;
    assert_eq!(cell(&fx, w - 1, 2).symbol, "↑");
    assert_eq!(cell(&fx, w - 1, rule - 1).symbol, "↓");
    assert_eq!(cell(&fx, w - 1, 2).bg, Some(PALETTE.track_back));
    let ruled = line(&fx, rule);
    assert!(
        ruled.starts_with("╟─") && ruled.ends_with("─╢"),
        "{ruled:?}"
    );
    assert_eq!(at(&fx, h - 2, "8 commits · all loaded"), Some(1));
    assert_eq!(cell(&fx, 1, h - 2).fg, Some(LOOK.foot));
    // A filter set: the title says so.
    let mut filters = fx.view().filters().clone();
    filters.tags = false;
    fx.view_mut().filters = filters;
    fx.idle();
    assert!(
        line(&fx, 0).contains(" box:/srv/repo [filtered] "),
        "{}",
        line(&fx, 0)
    );
    // Not the keyboard's: the single frame, the bar idle.
    fx.host.active = false;
    fx.idle();
    let top = line(&fx, 0);
    assert!(top.starts_with('┌') && top.ends_with('┐'), "{top:?}");
    assert_eq!(cell(&fx, 5, y0).bg, Some(PALETTE.bar_idle));
}

/// 03 §4.4: labels that do not fit end in `+N` (dim, on the row's back), and the label cursor's
/// label has its colours swapped — its own text colour as the back — with no modifier (T37).
#[test]
fn labels_stop_with_plus_n_and_the_label_cursor_swaps_colours() {
    let mut view = fixture_view();
    let mut refs = super::tests::refs();
    for k in 0..6 {
        refs.push(label(&format!("refs/tags/release-{k}"), 9, None));
    }
    view.set_refs(refs, HeadRef::Branch("refs/heads/main".into()));
    // 40 cells inside, five lanes (10 cells, under a third): Date and Author dropped, Commit
    // kept (9 cells with its blank), so the description holds 40 - 10 - 9 = 21 cells.
    let mut fx = Fx::new(42.0 * 8.0, 400.0, view);
    assert_eq!(fx.view().graph().width(), 5);
    let cols = columns(&fx);
    assert_eq!((cols.graph, cols.desc_w), (10, 21));
    // The merge's seven labels: `►main↔origin,up` (15 cells) and `release-0` … `release-5` (9
    // each). The first fits with room kept for `+6` (15 + 1 + 2 ≤ 21); the second does not
    // (16 + 9 + 1 + 2 > 21): `+6`, and no subject.
    let y = y_of(&fx, 1);
    let desc = 1 + cols.desc as u16;
    let shown = text(&fx, y, desc, desc + 21);
    assert_eq!(
        shown,
        format!("{:<21}", "►main↔origin,up +6"),
        "{:?}",
        line(&fx, y)
    );
    let px = at(&fx, y, "+6").unwrap();
    assert_eq!(px, desc + 16);
    assert_eq!(cell(&fx, px, y).fg, Some(LOOK.dim));
    // The bar on the merge, the cursor on its first label.
    fx.press(Key::ArrowDown);
    fx.view_mut().set_label_cursor(1);
    fx.idle();
    let desc = 1 + columns(&fx).desc as u16;
    let c = cell(&fx, desc, y);
    assert_eq!(
        (c.fg, c.bg),
        (Some(LOOK.local.back), Some(LOOK.local.text)),
        "swapped"
    );
    assert!(c.modifier.is_empty());
    fx.view_mut().set_label_cursor(0);
    fx.idle();
    let c = cell(&fx, desc, y);
    assert_eq!((c.fg, c.bg), (Some(LOOK.local.text), Some(LOOK.local.back)));
}

/// 03 §4.2, 10 §10: the subject and the author are cut by cells — a Hangul glyph two cells,
/// never half of one, `…` where they stop — in the window as in a terminal.
#[test]
fn the_subject_and_the_author_are_cut_by_cells() {
    let mut view = GitView::new("box:/srv/한글");
    view.set_refs(Vec::new(), HeadRef::Unborn);
    let subject = "가나다라마바사아자차카타파하".repeat(8);
    let commits = vec![commit(1, &[], "김철수김철수김철수", day(1), &subject)];
    let mut stream = RowStream::new(None, false, Vec::new(), true);
    view.set_rows(stream.page(commits), false, 0);
    view.set_paging(Paging::AllLoaded);
    let fx = Fx::new(1000.0, 400.0, view);
    let cols = columns(&fx);
    let y = y_of(&fx, 0);
    let author = 1 + cols.author.unwrap() as u16;
    let shown = text(&fx, y, author, author + 14);
    assert_eq!(
        shown.trim_end(),
        "김철수김철수…",
        "13 cells: the 7th glyph does not fit"
    );
    assert_eq!(cells::width(shown.trim_end()), 13);
    let desc = 1 + cols.desc as u16;
    let room = cols.desc_w as u16;
    let shown = text(&fx, y, desc, desc + room);
    assert!(shown.trim_end().ends_with('…'), "{shown:?}");
    assert!(cells::width(shown.trim_end()) <= usize::from(room));
    assert!(
        cells::width(shown.trim_end()) >= usize::from(room) - 1,
        "a glyph early at most"
    );
}

/// 03 §4.2: the bar is the row in the bar's colours, its text white; a marked row is yellow.
#[test]
fn the_bar_and_marked_rows() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.press(Key::End);
    let y = y_of(&fx, 9);
    let w = grid_of(&fx).cols;
    let row = &seen(&fx)[usize::from(y)][1..usize::from(w) - 1];
    let (tag, rest): (Vec<_>, Vec<_>) = row.iter().partition(|c| c.bg == Some(LOOK.tag.back));
    assert_eq!(tag.len(), "v1.0".len(), "its label keeps its colours");
    assert!(
        rest.iter().all(|c| c.bg == Some(PALETTE.bar)),
        "the rest of the row"
    );
    let x = at(&fx, y, "Initial commit").unwrap();
    assert_eq!(cell(&fx, x, y).fg, Some(LOOK.on_bar));
    // Space marks the bar's row: yellow, the bar moving off it.
    fx.press(Key::Space);
    fx.press(Key::ArrowUp);
    assert_eq!(cell(&fx, x, y).fg, Some(LOOK.marked));
    assert_eq!(cell(&fx, x, y).bg, Some(PALETTE.back));
}

/// 03 §4.2: the foot says Find's `k of n` while Find has text, and the branch's ahead and behind
/// at the right — only when it fits after the paging line with two cells between.
#[test]
fn the_foot_says_find_and_the_tracking_when_it_fits() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.view_mut().tracking = Some(Tracking {
        branch: "main".into(),
        upstream: "origin/main".into(),
        ahead: 2,
        behind: 1,
    });
    fx.idle();
    let g = grid_of(&fx);
    let foot = g.rows - 2;
    let right = "main ↑2 ↓1 origin/main";
    let w = right.chars().count() as u16;
    assert_eq!(
        at(&fx, foot, right),
        Some(g.cols - 1 - w),
        "{}",
        line(&fx, foot)
    );
    // Find: `k of n` in its place.
    fx.typed("one");
    assert!(
        line(&fx, foot)
            .trim_start_matches('║')
            .starts_with("1 of 2"),
        "{}",
        line(&fx, foot)
    );
    fx.press(Key::Escape);
    // Too narrow for both: the paging line alone.
    fx.drv.resize(30.0 * 8.0, 500.0);
    fx.idle();
    fx.idle();
    let row = line(&fx, grid_of(&fx).rows - 2);
    assert!(row.contains("8 commits"), "{row:?}");
    assert!(!row.contains("origin/main"), "{row:?}");
}

/// 03 §4.7: a history search's results are the same rows without lanes — the description from the
/// frame's first cell.
#[test]
fn a_history_search_is_the_same_rows_without_lanes() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let query = SearchQuery {
        message: "one".into(),
        ..Default::default()
    };
    fx.view_mut().search = Some(find::Results::new(query.clone()));
    fx.view_mut().set_search_results(
        &query,
        vec![
            commit(3, &[1], "Cy", day(3), "x: one"),
            commit(2, &[1], "Cy", day(2), "y: one"),
        ],
        Paging::AllLoaded,
    );
    fx.idle();
    assert_eq!(at(&fx, 2, "x: one"), Some(1), "{}", line(&fx, 2));
    assert_eq!(at(&fx, 3, "y: one"), Some(1), "{}", line(&fx, 3));
    assert_eq!(at(&fx, 1, "Description"), Some(1));
    assert!(fx.view().geometry().is_none(), "no graph drawn");
    assert_eq!(
        at(&fx, grid_of(&fx).rows - 2, "2 found · all loaded"),
        Some(1)
    );
}

/// 10 §10, T38: in an ambiguous-wide terminal the panel's own `↔` and the foot's `↑ ↓` are §10's
/// stand-ins at the source, as the lanes' are through X4-lanes; a `↔` in a branch's own name is
/// content and stays.
#[test]
fn an_ambiguous_wide_terminal_gets_the_stand_ins_of_the_panels_own_glyphs() {
    let mut view = fixture_view();
    let mut refs = super::tests::refs();
    refs.push(label("refs/heads/a↔b", 3, None));
    view.set_refs(refs, HeadRef::Branch("refs/heads/main".into()));
    view.tracking = Some(Tracking {
        branch: "main".into(),
        upstream: "origin/main".into(),
        ahead: 2,
        behind: 1,
    });
    let mut fx = Fx::new(1000.0, 500.0, view);
    assert!(
        at(&fx, y_of(&fx, 1), "►main↔origin,up").is_some(),
        "the control"
    );
    cells::set_ambiguous_wide(&fx.drv.ctx, true);
    fx.idle();
    let y = y_of(&fx, 1);
    assert!(at(&fx, y, "►main-origin,up").is_some(), "{}", line(&fx, y));
    assert!(
        at(&fx, y_of(&fx, index_of(&fx, "x: one")), "a↔b").is_some(),
        "a name's own ↔ stays"
    );
    let foot = line(&fx, grid_of(&fx).rows - 2);
    assert!(foot.contains("main ^2 v1 origin/main"), "{foot:?}");
    let lanes = graph_cells(&fx, 2);
    assert!(
        (lanes.iter().flatten()).all(|c| !c.symbol.chars().any(crate::gui::term::ambiguous)),
        "{lanes:?}"
    );
    cells::set_ambiguous_wide(&fx.drv.ctx, false);
}

// ---------------------------------------------------------------------------------------------
// The pointer and the keys by cells, several events in one pass.

/// Clicks by cells: a click on a row's cells puts the bar there; two in one pass, the second's
/// row; a double click is Enter (the details pane); a right click — the translator's
/// `Secondary` — the bar on its row and the row's menu.
#[test]
fn clicks_and_the_right_click_go_by_cells() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let p = centre(&fx, 40, y_of(&fx, 3));
    fx.drv.frame(
        &mut fx.host,
        click_events(p, PointerButton::Primary),
        Modifiers::NONE,
    );
    fx.idle();
    assert_eq!(fx.view().bar_place().0, 3);
    assert_eq!(
        cell(&fx, 40, y_of(&fx, 3)).bg,
        Some(PALETTE.bar),
        "the bar's cells"
    );
    // Two clicks on two rows in one pass: egui keeps one press a pass, the last one's place
    // (its `press_origin`), so the bar goes to the second click's row, row 7 — never row 5,
    // and never a row between.
    let (a, b) = (centre(&fx, 40, y_of(&fx, 5)), centre(&fx, 40, y_of(&fx, 7)));
    let mut events = click_events(a, PointerButton::Primary);
    events.extend(click_events(b, PointerButton::Primary));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    fx.idle();
    assert_eq!(fx.view().bar_place().0, 7, "the second click's row");
    assert_eq!(cell(&fx, 40, y_of(&fx, 7)).bg, Some(PALETTE.bar));
    // A double click on row 2 right after a click on row 7: the details pane of row 2 (egui
    // alone counts it a triple click; the list's and the tree's rule, `second_click`).
    let c = centre(&fx, 40, y_of(&fx, 2));
    let seven = centre(&fx, 40, y_of(&fx, 7));
    // Past egui's double-click time (0.3 s) since the clicks above.
    fx.drv.idle(&mut fx.host, 30);
    fx.drv.click(&mut fx.host, seven);
    assert!(!fx.view().details_open(), "one click on row 7");
    fx.drv.click(&mut fx.host, c);
    assert!(!fx.view().details_open(), "the control: one click on row 2");
    fx.drv.click(&mut fx.host, c);
    fx.idle();
    assert_eq!(fx.view().bar_place().0, 2);
    assert!(fx.view().details_open(), "double click is Enter");
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    assert!(!fx.view().details_open());
    // The right click on row 4's cells: the bar there, then its menu (a snapshot to act on).
    fx.view_mut().set_repo(super::tests_s3::repo());
    let r = centre(&fx, 60, y_of(&fx, 4));
    fx.drv.frame(
        &mut fx.host,
        click_events(r, PointerButton::Secondary),
        Modifiers::NONE,
    );
    fx.idle();
    assert_eq!(fx.view().bar_place().0, 4);
    assert!(fx.view().menu.is_some(), "its menu");
    // On the heads' row or the frame: nothing.
    fx.press(Key::Escape);
    let head = centre(&fx, 60, 1);
    fx.drv.frame(
        &mut fx.host,
        click_events(head, PointerButton::Secondary),
        Modifiers::NONE,
    );
    fx.idle();
    assert!(fx.view().menu.is_none(), "not a row");
    assert_eq!(fx.view().bar_place().0, 4);
}

/// The wheel by cells: a point delta a row each 16 points, line deltas three rows each — two
/// wheel events in one pass add up — and the bar stays; the paging line's click reads more.
#[test]
fn the_wheel_and_the_paging_line_go_by_cells() {
    let mut fx = Fx::new(1000.0, 400.0, busy_view(200));
    let over = centre(&fx, 40, 5);
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
    assert_eq!(fx.view().on_screen().start, 5, "2 rows and 3");
    assert_eq!(fx.view().bar_place().0, 0, "the bar stays");
    // The paging line: more is asked for once.
    fx.view_mut().set_paging(Paging::More(500));
    fx.idle();
    fx.take();
    let foot = centre(&fx, 3, grid_of(&fx).rows - 2);
    fx.drv.frame(
        &mut fx.host,
        click_events(foot, PointerButton::Primary),
        Modifiers::NONE,
    );
    fx.idle();
    assert_eq!(fx.take(), [GitAction::MoreRows]);
}

/// Keys and typing, several in one pass: two Downs move the bar two rows and that pass's cells
/// have it there; two letters typed in one pass start Find with both, and the bar is on the match
/// in the same pass's cells.
#[test]
fn several_keys_in_one_pass_draw_where_they_lead() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let mut events = key_events(Key::ArrowDown);
    events.extend(key_events(Key::ArrowDown));
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().bar_place().0, 2);
    assert_eq!(
        cell(&fx, 40, y_of(&fx, 2)).bg,
        Some(PALETTE.bar),
        "in that pass"
    );
    assert_eq!(cell(&fx, 40, y_of(&fx, 0)).bg, Some(PALETTE.back));
    let events = vec![Event::Text("y".into()), Event::Text(":".into())];
    fx.drv.frame(&mut fx.host, events, Modifiers::NONE);
    assert_eq!(fx.view().find_text(), Some("y:"));
    let y_row = index_of(&fx, "y: one");
    assert_eq!(fx.view().bar_place().0, y_row, "on the match");
    assert_eq!(
        cell(&fx, 40, y_of(&fx, y_row)).bg,
        Some(PALETTE.bar),
        "in that pass"
    );
    // Find's hint, moved into `find.rs`, on the status row while it is open.
    assert_eq!(fx.view().status_line().text, find::FIND_HINT);
}

// ---------------------------------------------------------------------------------------------
// The other parts' cells and what the panel blanks (10 §17's paragraph above the X4 table).

/// The cells under a dialog of the list's (`under`), under one of the panel's own
/// (`GitView::covered`) and under the key help are blank in the panel's cells — a Hangul glyph
/// half under a dialog's left edge goes whole, as a terminal erases it.
#[test]
fn the_panel_blanks_what_the_dialogs_and_the_help_cover() {
    let mut view = fixture_view();
    view.set_rows(rows_with_subject(&"한".repeat(60)), false, 2);
    let mut fx = Fx::new(1000.0, 500.0, view);
    let y = y_of(&fx, 2);
    let x = at(&fx, y, "한").unwrap();
    // A list's dialog whose left edge is a Hangul glyph's second cell.
    let edge = x + 7;
    assert_eq!(
        cell(&fx, edge, y).symbol,
        "",
        "the control: a glyph's second cell"
    );
    fx.host.under = vec![Cells::new(edge, y, 10, 2)];
    fx.idle();
    for c in edge..edge + 10 {
        let s = cell(&fx, c, y);
        assert_eq!(
            (s.symbol.as_str(), s.bg),
            (" ", Some(PALETTE.back)),
            "cell {c}"
        );
    }
    assert_eq!(
        cell(&fx, edge - 1, y).symbol,
        " ",
        "the glyph half under it, gone"
    );
    assert_eq!(cell(&fx, edge - 3, y).symbol, "한", "the one before stays");
    // One of the panel's own.
    fx.host.under.clear();
    fx.view_mut().test_covers = vec![Cells::new(3, 3, 5, 1)];
    fx.idle();
    assert_eq!(text(&fx, 3, 3, 8), "     ");
    fx.view_mut().test_covers.clear();
    fx.idle();
    assert_ne!(text(&fx, 3, 3, 8), "     ", "the control");
    // The key help.
    let page = crate::gui::help::for_context(keys::Context::Git, Mode::Preview, &[]);
    crate::gui::help::open(&fx.drv.ctx, page);
    fx.idle();
    let grid = grid_of(&fx);
    let covered = crate::gui::help::covered(&fx.drv.ctx, &grid);
    assert!(!covered.is_empty());
    let r = covered[0];
    let rows = seen(&fx);
    for yy in r.top()..r.bottom() {
        for xx in r.left()..r.right() {
            let s = &rows[usize::from(yy)][usize::from(xx)];
            assert_eq!(
                (s.symbol.as_str(), s.bg),
                (" ", Some(PALETTE.back)),
                "({xx}, {yy})"
            );
        }
    }
}

/// The fixture's rows with every commit's subject `subject`.
fn rows_with_subject(subject: &str) -> Vec<Row> {
    let commits = (super::tests::history().into_iter())
        .map(|mut c| {
            c.subject = subject.to_string();
            c
        })
        .collect();
    super::tests::rows_of(commits, true)
}

/// A part egui still draws (the details pane, Find's field, the banner) has its cells left to
/// `term::compose` — no glyph, no colour, the pane's with the border cell where its scroll bar
/// stands — so a terminal shows a hand-off box exactly there, inside the frame; the graph, the
/// rules and the foot stay cells. The banner over Find's row is one box.
#[test]
fn a_part_egui_draws_is_a_box_inside_the_frame() {
    let mut fx = Fx::new(1000.0, 600.0, fixture_view());
    let g = grid_of(&fx);
    let compose = |fx: &Fx| {
        let layers = cells::layers(&fx.drv.ctx);
        crate::gui::term::compose(&fx.drv.ctx, &layers, g.cols, g.rows).hand_offs
    };
    // Only the window's F-key row is egui's in this host: below the grid.
    let none: Vec<Cells> = compose(&fx).into_iter().filter(|r| r.y < g.rows).collect();
    assert!(none.is_empty(), "all cells: {none:?}");
    fx.press(Key::Enter);
    assert!(fx.view().details_open());
    let total = g.rows - 5;
    let graph = (total / 2).max(8).min(total - 2);
    let rule = 2 + graph;
    let pane = Cells::new(1, rule + 1, g.cols - 1, g.rows - 3 - (rule + 1));
    let ruled = line(&fx, rule);
    assert!(
        ruled.starts_with("╟─") && ruled.ends_with("─╢"),
        "{ruled:?}"
    );
    let left = cell(&fx, 1, rule + 1);
    assert_eq!((left.symbol.as_str(), left.bg), (" ", None), "left to egui");
    assert_eq!(
        cell(&fx, 0, rule + 1).symbol,
        "║",
        "the frame's left side stays"
    );
    let boxes: Vec<Cells> = compose(&fx).into_iter().filter(|r| r.y < g.rows).collect();
    assert_eq!(boxes, [pane], "one box where the pane is");
    fx.press(Key::Escape);
    fx.press(Key::Escape);
    // Find's field and the banner above it: one box of their rows.
    fx.view_mut().banner = Some(in_progress::BannerView::new("rebase in progress", vec![]));
    fx.typed("x");
    assert!(fx.view().find_text().is_some());
    let rows = u16::try_from(fx.view().banner.as_ref().unwrap().rows()).unwrap();
    let boxes: Vec<Cells> = compose(&fx).into_iter().filter(|r| r.y < g.rows).collect();
    assert_eq!(
        boxes,
        [Cells::new(1, 1, g.cols - 2, rows + 1)],
        "banner and Find: one box"
    );
    assert!(
        line(&fx, rows + 2).contains("Uncommitted changes (2)"),
        "the rows below"
    );
}

/// The grid of a `w`×`h` window of the test host: its central area, above the F-key row.
fn window_grid(w: f32, h: f32) -> Grid {
    Grid::over(egui::Rect::from_min_size(
        Pos2::ZERO,
        Vec2::new(w, h - 16.0),
    ))
}

/// What the last pass of the panel at `side` shows outside it, in the grid's rows: every
/// rectangle painted (not wholly transparent: the host's central panel is one) and text seen left
/// of the side, where nothing else stands in this host.
fn strays(fx: &Fx, g: &Grid, side: Cells) -> Vec<String> {
    let left = g.rect(Cells::new(0, 0, side.x, g.rows));
    let mut out: Vec<String> = (fx.drv.rects().into_iter())
        .filter(|(r, c, w)| r.intersect(left).area() > 0.0 && (c.a() > 0 || *w > 0.0))
        .map(|(r, c, _)| format!("rect {r:?} {c:?}"))
        .collect();
    out.extend(
        (fx.drv.texts_starting_with("").into_iter())
            .filter(|(r, _)| r.intersect(left).area() > 0.0)
            .map(|(r, t)| format!("text {t:?} at {r:?}")),
    );
    out
}

/// A panel too small for its parts — the heads' row, the rule and the foot under the banner —
/// draws its frame alone, never past its cells, whatever is open: on the right side, nothing of
/// it is seen left of the side, nor does a click there reach it (review round 2: the short
/// branch's egui parts stood outside its cells); at a few rows more every part has its cells.
#[test]
fn a_panel_too_small_for_its_parts_draws_its_frame() {
    for (w, h) in [(96.0, 16.0), (200.0, 64.0), (400.0, 96.0), (400.0, 160.0)] {
        let mut view = fixture_view();
        view.banner = Some(in_progress::BannerView::new("rebase in progress", vec![]));
        let mut fx = Fx::new(w, h + 16.0, view);
        let g = window_grid(w, h + 16.0);
        let [_, side] = cells::halves(g.all());
        fx.host.side = Some(side);
        fx.idle();
        fx.press(Key::Enter);
        fx.typed("x");
        fx.idle();
        let l = cells::layers(&fx.drv.ctx)
            .into_iter()
            .find(|l| l.buf.area == side)
            .expect("the panel's cells over its side");
        assert_eq!(l.origin, g.origin, "{w}x{h}");
        let room = side.height.saturating_sub(2) >= 4;
        assert_eq!(
            fx.view().geometry().is_some(),
            room,
            "{w}x{h}: {:?}",
            fx.view().geometry()
        );
        assert_eq!(strays(&fx, &g, side), Vec::<String>::new(), "{w}x{h}");
        let case = fx.view().find.as_ref().map(|f| f.case);
        fx.take();
        for y in 0..g.rows {
            for x in 0..side.x {
                let p = g.rect(Cells::new(x, y, 1, 1)).center();
                fx.drv.frame(
                    &mut fx.host,
                    click_events(p, PointerButton::Primary),
                    Modifiers::NONE,
                );
            }
        }
        fx.idle();
        assert!(
            fx.take().is_empty(),
            "{w}x{h}: a click outside asked something"
        );
        assert_eq!(
            fx.view().find.as_ref().map(|f| f.case),
            case,
            "{w}x{h}: the Aa toggle"
        );
    }
}

/// Review round 2: in a panel too short for its parts, the Backups list's buttons — Restore,
/// Show, Delete, which egui lays at its foot — are not laid on the frame's title row: a click
/// there asks nothing and the list stays open.
#[test]
fn a_short_panels_title_row_is_not_the_backups_buttons() {
    let mut fx = Fx::new(1000.0, 80.0, fixture_view());
    fx.view_mut().set_repo(super::tests_s3::repo());
    fx.view_mut().open_backups(super::tests_s3::records());
    fx.idle();
    fx.idle();
    assert_eq!(grid_of(&fx).rows, 4);
    fx.take();
    for x in [3, 12, 18] {
        let p = centre(&fx, x, 0);
        fx.drv.frame(
            &mut fx.host,
            click_events(p, PointerButton::Primary),
            Modifiers::NONE,
        );
        fx.idle();
        assert!(fx.take().is_empty(), "cell ({x}, 0) asked something");
        assert!(fx.view().backups().is_some(), "the list stays open");
        assert!(!fx.view().dialog_up(), "no dialog from cell ({x}, 0)");
    }
}

// ---------------------------------------------------------------------------------------------
// TS-4 (10 §14): the graph — its lanes, labels and a Hangul author — in both backends.

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

    /// The fixture with a Hangul author, the stash and the uncommitted row: the graph host's.
    fn host() -> Host {
        let mut commits = history();
        commits[1].author_name = "김철수".into();
        let mut view = GitView::new("box:/srv/repo");
        view.set_refs(refs(), HeadRef::Branch("refs/heads/main".into()));
        view.set_rows(rows_of(commits, true), false, 2);
        view.set_paging(Paging::AllLoaded);
        Host {
            view,
            active: true,
            actions: Vec::new(),
            rect: Rect::NOTHING,
            under: Vec::new(),
            side: None,
        }
    }

    /// TS-4's lanes: the terminal's receipt of the graph and the window's are the same bytes — at
    /// 100×30, and at 44×20, where Author and Date are dropped; the lanes' glyphs among them.
    #[test]
    fn ts4_the_graph_is_the_same_cells_in_both_backends() {
        for (cols, rows) in [(100, 30), (44, 20)] {
            let mut tui = host();
            let (terminal, when) = terminal_run(&mut tui, cols, rows);
            let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
            let mut win = host();
            drv.idle(&mut win, 2);
            let in_window = window_receipt(&drv, &mut win, cols, rows, when);
            assert_eq!(terminal, in_window, "{cols}x{rows}");
            for glyph in ["◙", "■", "≡", "○", "│", "┐", "┼", "►main↔origin,up"] {
                assert!(
                    terminal.contains(glyph),
                    "{cols}x{rows}: {glyph}\n{terminal}"
                );
            }
            let wide = cols == 100;
            assert_eq!(terminal.contains("Author"), wide, "{terminal}");
            assert_eq!(
                terminal.contains("김철수"),
                wide,
                "the Hangul author: {terminal}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Review round 1.

/// The cells the composed screen leaves to hand-off boxes, inside the panel's grid.
fn boxes(fx: &Fx) -> Vec<Cells> {
    let g = grid_of(fx);
    let layers = cells::layers(&fx.drv.ctx);
    let screen = crate::gui::term::compose(&fx.drv.ctx, &layers, g.cols, g.rows);
    screen
        .hand_offs
        .into_iter()
        .filter(|r| r.y < g.rows)
        .collect()
}

/// R1 MUST 1: the planner draws its egui scroll bar inside its own cells, so the frame's right
/// border stays the panel's on every body row (`║` in the window and the terminal), and the box a
/// terminal shows is exactly its cells: the heads' row and the body. The details pane, whose
/// scroll bar egui draws on the border, takes the border cell too. (The log and the Backups list
/// are cells since X4-lists, their scroll bar on the border: `tests_x4_lists.rs`.)
#[test]
fn the_log_backups_and_planner_leave_the_frames_right_border() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    let v = fx.view_mut();
    v.set_repo(super::tests_s4::repo());
    v.open_planner(super::tests_s4::model());
    fx.idle();
    fx.idle();
    let g = grid_of(&fx);
    let (w, h) = (g.cols, g.rows);
    for y in 1..h - 3 {
        assert_eq!(cell(&fx, w - 1, y).symbol, "║", "row {y}");
        assert_eq!(cell(&fx, 0, y).symbol, "║", "row {y}");
    }
    assert_eq!(boxes(&fx), [Cells::new(1, 1, w - 2, h - 4)]);
}

/// R1 MUST 2: a panel too short for its parts still runs those that hold the keyboard: the log's
/// entry shown whole closes with Esc, and text typed into an open Find goes into it, the bar on
/// the match.
#[test]
fn a_panel_too_short_still_runs_the_logs_entry_and_find() {
    let mut fx = Fx::new(1000.0, 500.0, fixture_view());
    fx.view_mut().open_log(vec![super::tests_s3::entry(
        "git fetch origin",
        crate::git::runner::Ending::Exit(1),
        "fatal: no",
        0,
    )]);
    fx.idle();
    fx.press(Key::Enter);
    assert!(fx.view().dialog_up(), "the entry shown whole");
    // Four rows of grid: the frame and two rows inside, no room for the heads, rule and foot.
    fx.drv.resize(1000.0, 80.0);
    fx.idle();
    fx.idle();
    assert_eq!(grid_of(&fx).rows, 4);
    // Drawn (the window is shorter than the dialog: part of it is off the window).
    let drawn = |fx: &Fx| fx.drv.texts().iter().any(|(_, _, t)| t == "Command");
    assert!(drawn(&fx), "the entry still shows");
    fx.press(Key::Escape);
    assert!(!fx.view().dialog_up(), "Esc closed it");
    assert!(!drawn(&fx));
    // Find in the short panel.
    fx.press(Key::Escape);
    assert!(fx.view().log_newest().is_none(), "the log closed");
    fx.typed("y");
    assert_eq!(fx.view().find_text(), Some("y"));
    fx.typed(":");
    assert_eq!(fx.view().find_text(), Some("y:"), "typed into the field");
    assert_eq!(
        fx.view().bar_place().0,
        index_of(&fx, "y: one"),
        "the bar on the match"
    );
}

/// R1 MUST 3: a label's text in an ambiguous-wide terminal costs the room it is shown in, never
/// a copy of the whole name: only the shown start is built, its joiner substituted there.
#[test]
fn a_long_joined_label_costs_its_room_only() {
    let long = format!("►{}↔origin", "b".repeat(100_000));
    let joined = long.find('↔');
    let label = Label {
        text: long.clone(),
        kind: LabelKind::CheckedOut,
        lossy: false,
        joined,
    };
    let short = Label {
        text: "►main↔origin,up".into(),
        kind: LabelKind::CheckedOut,
        lossy: false,
        joined: "►main↔origin,up".find('↔'),
    };
    let copied = |f: &dyn Fn() -> String| {
        graph_view::COPIED.with(|c| c.set(0));
        let out = f();
        (out, graph_view::COPIED.with(|c| c.get()))
    };
    for wide in [false, true] {
        let (out, bytes) = copied(&|| graph_view::label_text(&label, 30, wide));
        assert_eq!(out.chars().count(), 31, "the room and one more: {out:?}");
        assert!(bytes <= 4 * 31 * 2, "{wide}: {bytes} bytes copied");
    }
    let (out, _) = copied(&|| graph_view::label_text(&short, 30, true));
    assert_eq!(out, "►main-origin,up", "the joiner's stand-in");
    let (out, _) = copied(&|| graph_view::label_text(&short, 30, false));
    assert_eq!(out, "►main↔origin,up");
    let (out, _) = copied(&|| graph_view::label_text(&short, 3, true));
    assert_eq!(out, "►mai", "a joiner past the room: not built");
}
