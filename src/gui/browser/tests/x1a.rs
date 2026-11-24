//! X1a's window (10 §14 TS-8, §15): the list, the title, status and F-key rows in cells, today's
//! egui panes hosted at cell rects. The spike's three Q1 checks — what each pane paints stays in
//! its rect through a resize, and the tree's too; the keyboard moves between the grid's list and
//! egui's fields, dialogs and the viewer; Tab and clicks move it to and from the git panel — and
//! the cells themselves: the bar on a Hangul name in every cell, wide glyphs on whole cells, the
//! rename field in its row, clicks and double clicks on cell rows.

use super::*;
use crate::remote::treefs::{Budget, Disk, FsStat, Listing, Live, Mounts, Resolved};
use crate::tree::watchdog::{Pool, Timing};

/// The central area's grid on a `w`×`h` window: under the title row, above the status and F-key
/// rows, a cell each.
fn grid(w: f32, h: f32) -> Grid {
    let rows = Rect::from_min_max(Pos2::new(0.0, CELL), Pos2::new(w, h - 2.0 * CELL));
    Grid::over(rows)
}

const CELL: f32 = chrome::CELL.y;

/// The two sides of `g` in points.
fn sides(g: &Grid) -> [Rect; 2] {
    cells::halves(g.all()).map(|s| g.rect(s))
}

/// What the last pass drew out of its place: every text, rectangle, line, arc and picture that
/// can be seen, and every row of cells, lies in one of the window's parts — the title row, a
/// side, the rows below the grid — and a text in a side is drawn under a clip inside that side.
/// Each one that does not, said.
fn strays(fx: &Fx, size: Vec2, sides: [Rect; 2]) -> Vec<String> {
    let screen = Rect::from_min_size(Pos2::ZERO, size);
    let title = Rect::from_min_max(Pos2::ZERO, Pos2::new(size.x, CELL));
    let below = Rect::from_min_max(Pos2::new(0.0, sides[0].bottom()), screen.max);
    let parts = [title, sides[0], sides[1], below];
    let mut out = vec![];
    let mut check = |what: String, seen: Rect, clip: Option<Rect>| {
        if !seen.is_positive() || seen.area() < 0.5 {
            return;
        }
        let hit: Vec<usize> = (0..4)
            .filter(|&i| parts[i].intersect(seen).area() >= 0.5)
            .collect();
        match (hit.as_slice(), clip) {
            (&[i @ (1 | 2)], Some(clip)) => {
                if !parts[i].expand(0.5).contains_rect(clip.intersect(screen)) {
                    out.push(format!("{what} in side {} under clip {clip:?}", i - 1));
                }
            }
            ([_], _) => {}
            _ => out.push(format!("{what} at {seen:?} spans the parts {hit:?}")),
        }
    };
    for (bounds, clip, text) in fx.drv.texts() {
        let seen = bounds.intersect(clip).intersect(screen);
        check(format!("the text {text:?}"), seen, Some(clip));
    }
    // A frame of nothing (egui's panels paint one: no fill, no line) is not seen.
    let seen = |&(_, fill, line): &(Rect, egui::Color32, f32)| fill.a() > 0 || line > 0.0;
    for (r, fill, _) in fx.drv.rects().into_iter().filter(seen) {
        check(format!("a rect in {fill:?}"), r.intersect(screen), None);
    }
    for ([a, b], color) in fx.drv.segments() {
        let line = Rect::from_two_pos(a, b).expand(0.25);
        check(format!("a line in {color:?}"), line.intersect(screen), None);
    }
    for r in fx.drv.strokes().into_iter().chain(fx.drv.pictures()) {
        check("an arc or a picture".to_string(), r.intersect(screen), None);
    }
    for (r, text) in fx.drv.column(screen) {
        check(format!("the cells {text:?}"), r, None);
    }
    out
}

/// [`strays`] at the window's size and its grid's sides — none — and, as the negative control,
/// with the sides' shared edge a cell to the left: the list's right border then spans two parts.
fn each_in_its_place(fx: &Fx, w: f32, h: f32) {
    let g = grid(w, h);
    let size = Vec2::new(w, h);
    let [left, right] = sides(&g);
    let strays_here = strays(fx, size, [left, right]);
    assert!(strays_here.is_empty(), "{w}x{h}: {strays_here:#?}");
    let edge = Vec2::new(chrome::CELL.x, 0.0);
    let moved = [
        Rect::from_min_max(left.min, left.max - edge),
        Rect::from_min_max(right.min - edge, right.max),
    ];
    assert!(
        !strays(fx, size, moved).is_empty(),
        "{w}x{h}: the check sees what crosses an edge"
    );
}

/// The rows of cells inside `area`, as the column query reads them.
fn cells_in(fx: &Fx, area: Rect) -> Vec<(Rect, String)> {
    fx.drv.column(area)
}

impl Fx {
    /// The bar rests, the side opposite follows.
    fn rested(&mut self) {
        self.drv.idle(&mut self.b, 12);
        self.settle();
        self.drv.idle(&mut self.b, 2);
    }

    /// The window is `w`×`h` points from now on.
    fn sized(&mut self, w: f32, h: f32) {
        self.drv.resize(w, h);
        self.drv.idle(&mut self.b, 3);
        self.settle();
    }
}

/// Q1 (TS-8): the list is cells over its whole side, the preview today's egui at the other
/// side's cells, and nothing either paints leaves its side — at 1000×700, at sizes off the cell
/// grid, with a hex preview wider than a narrow side, and with the tree (the spike had no check
/// for it).
#[test]
fn q1_panes_on_cell_rects_clip_there_and_follow_a_resize() {
    let mut fx = fixture();
    fx.select("notes.txt");
    fx.rested();
    let [left, right] = sides(&grid(1000.0, 700.0));
    // The list: a row of cells per row of its side, each the side's width.
    let list = cells_in(&fx, left);
    assert_eq!(list.len(), 40, "{list:?}");
    assert!(list.iter().all(|(r, _)| r.width() == left.width()));
    // The preview (egui) at the right side's cells: no cells there.
    assert_eq!(panes(&fx), [left, right]);
    assert!(cells_in(&fx, right).is_empty(), "the preview is egui's");
    let line = fx.drv.find_text("plain notes").expect("the previewed text");
    assert!(right.contains_rect(line), "{line:?} inside {right:?}");
    each_in_its_place(&fx, 1000.0, 700.0);
    // The keys are the list's: the preview takes none.
    fx.key(Key::ArrowDown);
    assert_eq!(fx.current(), "photo.png");
    // Tab: the right list in cells; a file under its bar previews on the left.
    fx.key(Key::Tab);
    fx.select("main.rs");
    fx.rested();
    assert_eq!(fx.b.active, 1);
    assert!(cells_in(&fx, left).is_empty(), "the preview on the left");
    assert_eq!(cells_in(&fx, right).len(), 40);
    each_in_its_place(&fx, 1000.0, 700.0);
    // Sizes off the grid: both sides on whole cells, the pane following its side.
    for (w, h) in [(803.0, 487.0), (641.0, 333.0), (1000.0, 700.0)] {
        fx.sized(w, h);
        let g = grid(w, h);
        let [l, r] = sides(&g);
        assert_eq!(panes(&fx), [l, r], "{w}x{h}");
        let rows = cells_in(&fx, r);
        assert_eq!(rows.len(), usize::from(g.rows), "{w}x{h}");
        each_in_its_place(&fx, w, h);
    }
    // The hex preview (rows wider than a narrow side) stays in its side too.
    fx.sized(641.0, 333.0);
    fx.select("data.bin");
    fx.rested();
    each_in_its_place(&fx, 641.0, 333.0);
    // The tree in place of the active list, the other side a snapshot of its list in cells.
    fx.key(Key::F9);
    fx.settle();
    let _tree = tree_at_root(&mut fx);
    fx.key(Key::F10);
    fx.settle();
    assert!(fx.b.tree.is_some(), "F10 opened the tree");
    let [l, r] = sides(&grid(641.0, 333.0));
    assert!(cells_in(&fx, r).is_empty(), "the tree is egui's");
    assert!(!cells_in(&fx, l).is_empty(), "the other side's list");
    each_in_its_place(&fx, 641.0, 333.0);
    fx.sized(1000.0, 700.0);
    each_in_its_place(&fx, 1000.0, 700.0);
}

/// The real disk with the home at `home` (the tree opens there, never in the real home).
struct HomeAt {
    real: Arc<dyn Disk>,
    home: String,
}

impl Disk for HomeAt {
    fn list(
        &self,
        path: &str,
        budget: Option<Budget>,
        full: bool,
        guard: &[String],
        live: &Live,
    ) -> std::io::Result<Listing> {
        self.real.list(path, budget, full, guard, live)
    }

    fn resolve(&self, path: &str) -> std::io::Result<Resolved> {
        self.real.resolve(path)
    }

    fn home(&self) -> std::io::Result<Resolved> {
        self.real.resolve(&self.home)
    }

    fn mounts(&self) -> std::io::Result<Mounts> {
        self.real.mounts()
    }

    fn statvfs(&self, path: &str) -> std::io::Result<FsStat> {
        self.real.statvfs(path)
    }
}

/// The pool the tree's reads run on, shut down with the test.
struct TreePool(Pool);

impl Drop for TreePool {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

/// The browser's tree reads the fixture's folder as the home, on a pool of its own.
fn tree_at_root(fx: &mut Fx) -> TreePool {
    static N: AtomicU64 = AtomicU64::new(0);
    let host = format!("x1a-{}", N.fetch_add(1, Ordering::Relaxed));
    let disk: Arc<dyn Disk> = Arc::new(HomeAt {
        real: LocalFs::disk(),
        home: fx.root.clone(),
    });
    let pool = Pool::new(Timing::default());
    let p = pool.clone();
    let connect: TreeConnect =
        Arc::new(move |_: &AtomicBool| Ok((LocalFs::tree_with(disk.clone(), p.clone()), None)));
    fx.b.set_tree_connect(connect, &host);
    TreePool(pool)
}

/// Q1 (TS-8): the keyboard moves between the grid's list and egui — F2's field in the row's
/// cells, F7's dialog over the grid, the viewer at all of it (its keys its own; the hex viewer's
/// Go to field keeps its Esc) — and a click on a cell row puts the bar there, a double click
/// opens it. Each with its negative control: the key a field or a dialog took jumps once it is
/// gone, a click a row lower puts the bar on that row, and a second click on another row opens
/// nothing.
#[test]
fn q1_the_keyboard_moves_between_the_grid_and_egui_fields_dialogs_and_the_viewer() {
    let mut fx = fixture();
    let g = grid(1000.0, 700.0);
    let [left, _] = sides(&g);
    fx.select("main.rs");
    let name = fx.drv.find_text("main").expect("main's cells");
    let band = Rect::from_x_y_ranges(left.x_range(), name.y_range());
    // F2: today's egui field at the row's cells inside the grid's list.
    fx.key(Key::F2);
    assert_eq!(fx.b.ops.renaming(), Some("main.rs"));
    assert!(fx.drv.ctx.text_edit_focused(), "the field has the typing");
    let field = fx.drv.find_text("main.rs").expect("the field's text");
    assert!(band.contains_rect(field), "{field:?} in the row {band:?}");
    fx.typed("z");
    assert_eq!(fx.current(), "main.rs", "typing is the field's, not a jump");
    fx.key(Key::Escape);
    assert_eq!(fx.b.ops.renaming(), None);
    assert!(!fx.drv.ctx.text_edit_focused());
    fx.typed("z");
    assert_eq!(
        fx.current(),
        "zeta.md",
        "the control: without the field, z jumps"
    );
    fx.select("main.rs");
    fx.key(Key::ArrowDown);
    assert_eq!(fx.current(), "notes.txt", "the list has the keyboard back");
    // F7: an egui dialog over the grid holds the keyboard; Esc gives it back.
    fx.key(Key::F7);
    assert!(fx.b.ops.holds_keyboard());
    fx.typed("p");
    assert_eq!(fx.current(), "notes.txt");
    fx.key(Key::Escape);
    assert!(!fx.b.ops.holds_keyboard());
    fx.typed("p");
    assert_eq!(
        fx.current(),
        "photo.png",
        "the control: without the dialog, p jumps"
    );
    fx.select("notes.txt");
    fx.key(Key::ArrowUp);
    assert_eq!(fx.current(), "main.rs");
    // Enter: the text viewer at the central area's cells, no list under it, the title row in
    // cells above it; its keys are its own.
    fx.open("main.rs");
    assert!(
        cells_in(&fx, g.rect(g.all())).is_empty(),
        "no list under a viewer"
    );
    let title = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, CELL));
    assert_eq!(cells_in(&fx, title).len(), 1, "the title row in cells");
    assert!(
        fx.drv.find_text("Unwrap").is_some(),
        "the viewer's F2 in the F-key row"
    );
    fx.key(Key::F2);
    assert!(fx.drv.find_text("Wrap").is_some(), "the viewer's F2");
    fx.key(Key::ArrowDown);
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    assert_eq!(fx.current(), "main.rs", "the list's bar did not move");
    assert_eq!(cells_in(&fx, left).len(), 40, "the list in cells again");
    // The hex viewer's Go to field inside the hosted viewer: its Esc is the field's.
    fx.open("data.bin");
    fx.key(Key::F6);
    fx.typed("1");
    assert!(fx.drv.ctx.text_edit_focused(), "the Go to field");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_some(), "the field's Esc did not leave");
    fx.key(Key::Escape);
    assert!(fx.b.viewer.is_none());
    // A click on a cell row puts the bar there; a second one right after opens it — on that
    // row only: a click a row lower is that row's, and a second click on another row opens
    // nothing.
    let beta = fx.drv.find_text("beta").expect("beta's cells").center();
    fx.drv.click(&mut fx.b, beta + Vec2::new(0.0, CELL));
    assert_eq!(fx.current(), ".hidden", "the control: the row below");
    fx.drv.click(&mut fx.b, beta);
    assert_eq!(fx.current(), "beta");
    assert_eq!(fx.b.panels[0].dir, fx.root, "a second click on another row");
    fx.drv.click(&mut fx.b, beta);
    fx.settle();
    assert_eq!(
        fx.b.panels[0].dir,
        format!("{}/beta", fx.root),
        "double click"
    );
}

/// Q1 (TS-8): Tab and clicks move the keyboard between the cell list and the git panel (egui at
/// the right side's cells), and back. The negative controls: Down in the list leaves the git
/// panel's bar, and a click a cell left of the git panel's side is the list's.
#[test]
fn q1_tab_and_clicks_move_the_keyboard_between_the_cell_list_and_the_git_panel() {
    let Some(git) = crate::git::history::fixture::git() else {
        return;
    };
    let mut g = super::s2::Gx::new(super::s2::small(&git));
    g.wait_for_the_panel();
    let [left, right] = sides(&grid(1000.0, 700.0));
    assert_eq!(panes(&g.fx), [left, right], "the list and the git panel");
    assert!(cells_in(&g.fx, right).is_empty(), "the git panel is egui's");
    assert_eq!(cells_in(&g.fx, left).len(), 40, "the list in cells");
    each_in_its_place(&g.fx, 1000.0, 700.0);
    let list_bar = g.fx.current();
    g.tab_into_git();
    let before = g.panel().view.bar_place();
    g.key(Key::ArrowDown);
    g.settle();
    assert_ne!(g.panel().view.bar_place(), before, "Down moved the git bar");
    assert_eq!(g.fx.current(), list_bar, "and not the list's");
    g.key(Key::Tab);
    g.settle();
    assert!(g.b().git_focused().is_none(), "Tab back to the cell list");
    let git_bar = g.panel().view.bar_place();
    g.key(Key::ArrowDown);
    assert_ne!(g.fx.current(), list_bar, "the list's keys again");
    assert_eq!(
        g.panel().view.bar_place(),
        git_bar,
        "the control: not the git panel's"
    );
    // Clicks: in the git panel the keyboard goes there — not a cell left of its side — and on a
    // cell row of the list, back.
    let beside = Pos2::new(right.left() - chrome::CELL.x / 2.0, right.center().y);
    g.fx.drv.click(&mut g.fx.b, beside);
    g.settle();
    assert!(
        g.b().git_focused().is_none(),
        "the control: a click left of its side"
    );
    g.fx.drv.click(&mut g.fx.b, right.center());
    g.settle();
    assert!(g.b().git_focused().is_some(), "a click in the git panel");
    let up =
        g.fx.drv
            .find_text("..")
            .expect("the list's .. row")
            .center();
    g.fx.drv.click(&mut g.fx.b, up);
    g.settle();
    assert!(g.b().git_focused().is_none(), "a click on the cell list");
    assert_eq!(g.fx.current(), "..");
}

/// The topmost rectangle the last pass filled under `p`: the colour a cell's background is
/// painted in there.
fn background_at(fx: &Fx, p: Pos2) -> Option<egui::Color32> {
    let rects = fx.drv.rects();
    rects
        .iter()
        .rev()
        .find(|(r, _, _)| r.contains(p))
        .map(|r| r.1)
}

/// Where the cell painter drew `glyph` alone, a galley of its own: the glyphs of a text in cells.
fn glyph_at(fx: &Fx, glyph: &str) -> Vec<Pos2> {
    let galleys = fx.drv.galleys().into_iter();
    galleys
        .filter(|(_, g)| g.text() == glyph)
        .map(|(p, _)| p)
        .collect()
}

/// TS-8: the bar on a Hangul name is light red (`ff5555`) in every cell of it — each glyph's two
/// cells, the blank between words — and its glyphs are white, each at its own cell; on the
/// other side's bar, the panel not active, the same name is on black in every cell.
#[test]
fn the_bar_on_a_hangul_name_is_light_red_in_every_cell() {
    let mut fx = fixture();
    let name = "한글 문서 초안.txt";
    std::fs::write(format!("{}/{name}", fx.root), "본문\n").unwrap();
    fx.key(Key::F9);
    fx.b.list(None, fx.root.clone(), None, None);
    fx.settle();
    fx.select(name);
    let at = fx.b.panels[0].highlight;
    fx.b.panels[1].highlight = at;
    fx.drv.idle(&mut fx.b, 2);
    let stem = "한글 문서 초안";
    let hits = fx.drv.texts_starting_with(stem);
    let (on_active, on_idle) = match hits[..] {
        [(a, ref ta), (b, ref tb)] if ta == stem && tb == stem => (a, b),
        _ => panic!("the name on both bars: {hits:?}"),
    };
    // Six Hangul on two cells each and two blanks: 14 cells, from the first inside the frame.
    let [left, right] = sides(&grid(1000.0, 700.0));
    assert_eq!(on_active.left(), left.left() + 8.0);
    assert_eq!(on_idle.left(), right.left() + 8.0);
    assert_eq!(
        (on_active.width(), on_idle.width()),
        (14.0 * 8.0, 14.0 * 8.0)
    );
    let p = chrome::PALETTE;
    assert_eq!(p.bar, egui::Color32::from_rgb(0xff, 0x55, 0x55));
    for (row, back) in [(on_active, p.bar), (on_idle, p.bar_idle)] {
        for k in 0..14 {
            let centre = row.left_center() + Vec2::new(8.0 * k as f32 + 4.0, 0.0);
            assert_eq!(
                background_at(&fx, centre),
                Some(back),
                "cell {k} of {row:?}"
            );
        }
    }
    assert_eq!(
        fx.drv.colors_of(stem),
        Some(vec![p.bar_text]),
        "white glyphs"
    );
    // Each Hangul glyph alone at its first cell's corner, on both sides.
    for (i, glyph) in ["한", "글", "문", "서", "초", "안"].into_iter().enumerate() {
        let cell = [0, 2, 5, 7, 10, 12][i];
        let x = 8.0 * cell as f32;
        let want = [
            Pos2::new(on_active.left() + x, on_active.top()),
            Pos2::new(on_idle.left() + x, on_idle.top()),
        ];
        assert_eq!(glyph_at(&fx, glyph), want, "{glyph}");
    }
}

/// 10 §10: a wide glyph — Hangul, an emoji — takes two whole cells, its second showing nothing,
/// and a name too long for its column is cut a glyph early with "…", never half a glyph.
#[test]
fn wide_glyphs_take_two_whole_cells_and_a_cut_never_halves_one() {
    let mut fx = fixture();
    let song = "\u{1F3B5} 노래.mp3";
    let long = "가나다라마바사아자차카타.txt";
    for file in [song, long] {
        std::fs::write(format!("{}/{file}", fx.root), "x").unwrap();
    }
    fx.b.list(None, fx.root.clone(), None, None);
    fx.settle();
    // The left list's Name column at 1000 points: 21 cells from cell 1, then the gap.
    let rows = |x: u16, w: u16| -> Vec<String> {
        let area = Rect::from_min_max(
            Pos2::new(f32::from(x) * 8.0, 16.0 + 2.0 * CELL),
            Pos2::new(f32::from(x + w) * 8.0, 16.0 + 39.0 * CELL),
        );
        cells_in(&fx, area).into_iter().map(|(_, t)| t).collect()
    };
    let (names, gap) = (rows(1, 21), rows(22, 1));
    let row = |stem: &str| {
        (names.iter())
            .position(|t| t.starts_with(stem))
            .unwrap_or_else(|| panic!("{stem}: {names:?}"))
    };
    // The note and each Hangul two cells, the blank one: read a cell at a time, a wide glyph's
    // second cell shows nothing.
    let y = row("\u{1F3B5}");
    let one = |x: u16| rows(x, 1)[y].clone();
    let cells: Vec<String> = (1..8).map(one).collect();
    assert_eq!(cells, ["\u{1F3B5}", "", " ", "노", "", "래", ""]);
    assert_eq!(gap[y], " ");
    // Twelve Hangul (24 cells) in 21: ten of them and "…", on the column's last cell.
    let y = row("가나");
    assert_eq!(names[y], "가나다라마바사아자차…");
    assert_eq!(gap[y], " ", "no glyph's half in the gap");
    // On screen, each glyph at its own cell: the note at cell 1, 노 at cell 4.
    let at = fx
        .drv
        .find_text("\u{1F3B5} 노래")
        .expect("the song's cells");
    assert_eq!(glyph_at(&fx, "노")[0], at.min + Vec2::new(24.0, 0.0));
    assert_eq!(glyph_at(&fx, "\u{1F3B5}")[0], at.min);
}

/// F2's field stays a working field in its row of cells: over the Name and Ext columns, the
/// name in it; typed into, Enter renames the file and the cells show the new name.
#[test]
fn the_rename_field_is_a_field_in_its_row_of_cells() {
    let mut fx = fixture();
    fx.select("notes.txt");
    let cells = fx.drv.find_text("notes").expect("the name's cells");
    fx.key(Key::F2);
    assert_eq!(fx.b.ops.renaming(), Some("notes.txt"));
    let field = fx.drv.find_text("notes.txt").expect("the field's text");
    assert_eq!(field.min, cells.min, "the field's text where the name was");
    // Over Name (21 cells) and Ext (5) and the gap between: from cell 1 to cell 28.
    let bodies: Vec<Rect> = (fx.drv.rects().into_iter())
        .filter(|&(r, _, _)| r.top() == cells.top() && r.left() == 8.0 && r.height() == CELL)
        .map(|(r, _, _)| r)
        .collect();
    assert!(
        bodies.iter().any(|r| r.right() == 28.0 * 8.0),
        "the field over Name and Ext: {bodies:?}"
    );
    fx.drv
        .key(&mut fx.b, Key::A, crate::gui::testkit::command(false));
    fx.typed("memo.txt");
    fx.key(Key::Enter);
    fx.drv
        .until(&mut fx.b, 20, |b| !b.ops.busy() && b.listing.is_empty());
    fx.settle();
    assert_eq!(fx.b.ops.renaming(), None);
    assert!(Path::new(&format!("{}/memo.txt", fx.root)).exists());
    assert_eq!(fx.current(), "memo.txt");
    // The list's Name column: the new name, not the old one.
    let [left, _] = sides(&grid(1000.0, 700.0));
    let names: Vec<String> = cells_in(&fx, Rect::from_x_y_ranges(8.0..=176.0, left.y_range()))
        .into_iter()
        .map(|(_, t)| t.trim_end().to_string())
        .collect();
    assert!(names.iter().any(|t| t == "memo"), "{names:?}");
    assert!(!names.iter().any(|t| t == "notes"), "{names:?}");
}
