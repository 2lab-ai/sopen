//! The tree as a side's content, driven by real egui key and pointer events through the test
//! kit: its rows, glyphs and info line (05 §3, §6.1), its colours (`chrome::tree`), the bar and
//! its keys (05 §1), and its F-key row.

use super::*;
use crate::gui::chrome::tree::{LOOK, SCAN_MARKS};
use crate::gui::chrome::{CELL, PALETTE};
use crate::gui::testkit::{self, Driver};
use crate::gui::Window;
use eframe::egui::{Event, Key, Modifiers};

/// The tree on the left half of the window, as a side of the browser is laid out, with what its
/// keys and clicks asked for.
struct Host {
    view: TreeView,
    active: bool,
    actions: Vec<TreeAction>,
    /// Where the last pass drew the tree.
    rect: Rect,
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.actions.extend(keys(&ctx, &mut self.view));
        chrome::key_panel(ui, "tree-test-keys", &fkeys());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let area = ui.max_rect();
                let half = (area.width() / 2.0).floor();
                let rect =
                    Rect::from_min_size(area.min, Vec2::new(half, area.height())).shrink(2.0);
                self.rect = rect;
                self.actions
                    .extend(draw(ui, rect, &mut self.view, self.active));
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

struct Fx {
    drv: Driver,
    host: Host,
}

impl Fx {
    /// `view` drawn once in a `w`×700 window.
    fn new(w: f32, view: TreeView) -> Self {
        let mut fx = Fx {
            drv: Driver::new(w, 700.0),
            host: Host {
                view,
                active: true,
                actions: Vec::new(),
                rect: Rect::NOTHING,
            },
        };
        fx.idle();
        fx
    }

    fn idle(&mut self) {
        self.drv.idle(&mut self.host, 1);
    }

    fn key(&mut self, key: Key, mods: Modifiers) -> Vec<TreeAction> {
        self.drv.key(&mut self.host, key, mods);
        std::mem::take(&mut self.host.actions)
    }

    fn press(&mut self, key: Key) -> Vec<TreeAction> {
        self.key(key, Modifiers::NONE)
    }

    fn typed(&mut self, text: &str) -> Vec<TreeAction> {
        self.drv.frame(
            &mut self.host,
            vec![Event::Text(text.into())],
            Modifiers::NONE,
        );
        self.idle();
        std::mem::take(&mut self.host.actions)
    }

    /// The room inside the tree's frame.
    fn inner(&self) -> Rect {
        chrome::inside(self.host.rect)
    }

    /// Where the last pass drew exactly `text` whole, every time, top to bottom.
    fn all(&self, text: &str) -> Vec<Rect> {
        self.drv
            .texts_starting_with(text)
            .into_iter()
            .filter(|(_, t)| t == text)
            .map(|(r, _)| r)
            .collect()
    }

    /// Where `text` is drawn on the row whose name is `name`.
    fn on_row(&self, name: &str, text: &str) -> Option<Rect> {
        let y = self.drv.find_text(name)?.top();
        self.all(text).into_iter().find(|r| r.top() == y)
    }

    /// Every text drawn on the row whose name is `name`, left to right.
    fn row_texts(&self, name: &str) -> Vec<(Rect, String)> {
        let y = self.drv.find_text(name).expect(name).top();
        let inner = self.inner();
        let mut out: Vec<(Rect, String)> = self
            .drv
            .texts_starting_with("")
            .into_iter()
            .filter(|(r, _)| r.top() == y && inner.contains_rect(*r))
            .collect();
        out.sort_by(|a, b| a.0.left().total_cmp(&b.0.left()));
        out
    }

    /// The name on the bar's row.
    fn bar_name(&self) -> String {
        self.host.view.bar_row().map(|r| r.name.clone()).unwrap()
    }
}

fn row(id: usize, depth: usize, name: &str, kind: RowKind, fold: Fold, info: Info) -> TreeRow {
    TreeRow {
        id,
        depth,
        name: name.to_string(),
        kind,
        fold,
        hidden: false,
        boundary: false,
        info,
    }
}

fn counted(dirs: u64, files: u64, size: Size) -> Info {
    Info {
        dirs: Some(Count {
            n: dirs,
            more: false,
        }),
        files: Some(Count {
            n: files,
            more: false,
        }),
        size,
        ..Default::default()
    }
}

fn bytes(n: u64, more: bool) -> Size {
    Size::Bytes {
        n,
        more,
        cached: false,
    }
}

fn tagged(tag: Tag) -> Info {
    Info {
        size: Size::Tag(tag),
        ..Default::default()
    }
}

/// A tree with every kind of row of 05 §3 and §6.1:
///
/// ```text
/// ▼ ~                5 d   6 f    52M+
///   ▼ src           19 d  40 f   9.8M+ ·
///       lib          0 d   2 f     79K
///     · 2 hidden folders
///   ► a/b/c          3 d   7 f   1.1M+
///   → Desktop                   <LINK>
///   ► proc                       <SYS>
///   ► home           2 d   0 f  =87G fs
///   ► Private                        ?
///   ► media          3 d   0 f  not answering
///   ► .cache        9 d+ 9991 f+ ~1.2M+
///   ► new
/// ```
fn sample() -> Vec<TreeRow> {
    use Fold::*;
    use RowKind::*;
    let mut src = row(
        1,
        1,
        "src",
        Folder,
        AutoOpened,
        counted(19, 40, bytes(9_800_000, true)),
    );
    src.info.scanning = true;
    let mut home = row(7, 1, "home", Folder, Closed, counted(2, 0, Size::Unknown));
    home.boundary = true;
    home.info.fs = Some(FsUse {
        bytes: 87_000_000_000,
        files: 1_234_567,
    });
    let mut media = row(
        9,
        1,
        "media",
        Folder,
        Closed,
        counted(3, 0, Size::Tag(Tag::NotAnswering)),
    );
    media.info.scanning = false;
    let mut cache = row(
        10,
        1,
        ".cache",
        Folder,
        Closed,
        Info {
            dirs: Some(Count { n: 9, more: true }),
            files: Some(Count {
                n: 9991,
                more: true,
            }),
            size: Size::Bytes {
                n: 1_200_000,
                more: true,
                cached: true,
            },
            ..Default::default()
        },
    );
    cache.hidden = true;
    vec![
        row(
            0,
            0,
            "~",
            Folder,
            Open,
            counted(5, 6, bytes(52_000_000, true)),
        ),
        src,
        row(
            2,
            2,
            "lib",
            Folder,
            Leaf,
            counted(0, 2, bytes(79_000, false)),
        ),
        row(3, 2, "", Hidden(2), Leaf, Info::default()),
        row(
            4,
            1,
            "a/b/c",
            Folder,
            Closed,
            counted(3, 7, bytes(1_100_000, true)),
        ),
        row(5, 1, "Desktop", Link, Leaf, tagged(Tag::Link)),
        row(6, 1, "proc", Folder, Closed, tagged(Tag::Sys)),
        home,
        row(8, 1, "Private", Folder, Closed, tagged(Tag::Unreadable)),
        media,
        cache,
        row(11, 1, "new", Folder, Closed, Info::default()),
    ]
}

/// The sample tree titled as its root, the bar on row `bar`, in a 1000-point window.
fn sample_fx(bar: usize) -> Fx {
    Fx::new(1000.0, TreeView::new("box:/srv/data", sample(), bar))
}

/// The root and `n` folders below it, `f000`, `f001`, … (row `k + 1` is `f{k}`).
fn long(n: usize) -> Vec<TreeRow> {
    let mut rows = vec![row(0, 0, "/", RowKind::Folder, Fold::Open, Info::default())];
    rows.extend((0..n).map(|k| {
        row(
            k + 1,
            1,
            &format!("f{k:03}"),
            RowKind::Folder,
            Fold::Closed,
            Info::default(),
        )
    }));
    rows
}

/// 05 §3 (05:69–86), T-rows' look: a row's glyph — `▼` open (by the owner or by itself), `►`
/// closed, none for a folder with no subfolders, `→` a link — a cell before its name, two
/// cells deeper a level; a compact chain on one row; the hidden subfolders as one row `· N
/// hidden folders` in the glyph's place; the frame titled with the root.
#[test]
fn rows_draw_their_glyphs_names_and_chains() {
    let fx = sample_fx(0);
    let inner = fx.inner();
    assert!(
        fx.drv.find_text("box:/srv/data").is_some(),
        "the frame's title"
    );
    let x = |cells: usize| inner.left() + CELL.x * cells as f32;
    // depth d: the glyph at 2d cells, the name two cells further.
    for (name, depth, glyph) in [
        ("~", 0, Some("▼")),
        ("src", 1, Some("▼")),
        ("lib", 2, None),
        ("a/b/c", 1, Some("►")),
        ("Desktop", 1, Some("→")),
        ("proc", 1, Some("►")),
        ("home", 1, Some("►")),
        (".cache", 1, Some("►")),
        ("new", 1, Some("►")),
    ] {
        let at = fx.drv.find_text(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(at.left(), x(2 * depth + 2), "{name}'s indent");
        let texts = fx.row_texts(name);
        let before: Vec<&str> = texts
            .iter()
            .filter(|(r, _)| r.left() < at.left())
            .map(|(_, t)| t.as_str())
            .collect();
        match glyph {
            Some(g) => {
                assert_eq!(before, [g], "{name}'s glyph");
                let g_at = fx.on_row(name, g).unwrap();
                assert_eq!(g_at.left(), x(2 * depth), "{name}'s glyph cell");
            }
            None => assert!(before.is_empty(), "{name} has no glyph: {before:?}"),
        }
    }
    let hidden = fx
        .drv
        .find_text("· 2 hidden folders")
        .expect("the hidden row");
    assert_eq!(hidden.left(), x(4), "in the glyph's place at depth 2");
    // Its row is the one below lib's.
    let lib = fx.drv.find_text("lib").unwrap();
    assert_eq!(hidden.top(), lib.top() + CELL.y);
    // One cell a row, top to bottom, in the order given.
    let tops: Vec<f32> = ["~", "src", "lib", "a/b/c", "Desktop", "new"]
        .iter()
        .map(|n| fx.drv.find_text(n).unwrap().top())
        .collect();
    let rows = [0.0, 1.0, 2.0, 4.0, 5.0, 11.0];
    for (top, k) in tops.iter().zip(rows) {
        assert_eq!(*top, inner.top() + CELL.y * k);
    }
}

/// 05 §6.1 (05:168–211) with §6.3's lower bounds and mark (05:250, 05:264): each row ends with
/// three right-aligned columns — `d`, `f` and the size, a tag in the size's place — the counts
/// with `+` where a listing stopped early, the size with `+` (more below), `~` (cached),
/// `=… fs` (a boundary's filesystem) and the moving mark after a size being scanned; nothing
/// for a folder not listed yet. `not answering`, wider than the size column, overlaps nothing
/// and stays inside the frame.
#[test]
fn the_info_line_reads_each_kind_of_row() {
    let fx = sample_fx(0);
    let inner = fx.inner();
    let want: &[(&str, &[&str])] = &[
        ("~", &["5 d", "6 f", "52M+"]),
        ("src", &["19 d", "40 f", "9.8M+"]),
        ("lib", &["0 d", "2 f", "79K"]),
        ("a/b/c", &["3 d", "7 f", "1.1M+"]),
        ("Desktop", &["<LINK>"]),
        ("proc", &["<SYS>"]),
        ("home", &["2 d", "0 f", "=87G fs"]),
        ("Private", &["?"]),
        ("media", &["3 d", "0 f", "not answering"]),
        (".cache", &["9 d+", "9991 f+", "~1.2M+"]),
        ("new", &[]),
    ];
    let mut right_edges = [Vec::new(), Vec::new(), Vec::new()];
    for (name, pieces) in want {
        let name_at = fx.drv.find_text(name).unwrap();
        // What the row shows after its name, left to right.
        let after: Vec<(Rect, String)> = fx
            .row_texts(name)
            .into_iter()
            .filter(|(r, _)| r.left() > name_at.left())
            .collect();
        let shown: Vec<&str> = after.iter().map(|(_, t)| t.as_str()).collect();
        let marks: Vec<String> = SCAN_MARKS.iter().map(char::to_string).collect();
        let shown_without_mark: Vec<&str> = shown
            .iter()
            .copied()
            .filter(|t| !marks.iter().any(|m| m == t))
            .collect();
        assert_eq!(shown_without_mark, *pieces, "{name}");
        // Columns: a three-piece row lines up with the others; a tag sits in the size's. Only
        // `not answering`, wider than the size column, moves its row's counts left.
        let n = pieces.len();
        if !pieces.contains(&"not answering") {
            for (k, (r, _)) in after.iter().enumerate().take(n) {
                right_edges[3 - n + k].push((r.right(), name.to_string()));
            }
        }
        // Nothing overlaps, a cell apart at least; all inside the frame.
        for pair in after.windows(2) {
            assert!(
                pair[0].0.right() + CELL.x <= pair[1].0.left(),
                "{name}: {pair:?}"
            );
        }
        if let Some((last, _)) = after.last() {
            assert!(inner.contains_rect(*last), "{name}: {last:?} in {inner:?}");
        }
        assert!(name_at.right() + CELL.x <= after.first().map_or(f32::MAX, |p| p.0.left()));
    }
    for (column, edges) in right_edges.iter().enumerate() {
        let first = edges[0].0;
        assert!(
            edges.iter().all(|(e, _)| *e == first),
            "column {column}: {edges:?}"
        );
    }
    // The mark: one cell after the size of the row being scanned, and on no other row.
    let size = fx.on_row("src", "9.8M+").unwrap();
    let mark = fx
        .row_texts("src")
        .into_iter()
        .find(|(_, t)| SCAN_MARKS.iter().any(|m| m.to_string() == *t))
        .expect("src is being scanned");
    assert_eq!(mark.0.left(), size.right() + CELL.x);
    for (name, _) in want.iter().filter(|(n, _)| *n != "src") {
        assert!(
            !fx.row_texts(name)
                .iter()
                .any(|(_, t)| SCAN_MARKS.iter().any(|m| m.to_string() == *t)),
            "{name} is not being scanned"
        );
    }
}

/// 05 §6.3 (05:264–265): the mark after a size being scanned moves — another glyph a quarter
/// of a second later — and the tree asks to be painted again for its next step, by a quarter
/// of a second at most; with no row being scanned it asks for none.
#[test]
fn the_scan_mark_moves() {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    let mut fx = sample_fx(0);
    let delays = Arc::new(Mutex::new(Vec::new()));
    let seen = delays.clone();
    fx.drv
        .ctx
        .set_repaint_observer(move |info| seen.lock().unwrap().push(info.delay));
    let mark = |fx: &Fx| {
        fx.row_texts("src")
            .into_iter()
            .find(|(_, t)| SCAN_MARKS.iter().any(|m| m.to_string() == *t))
            .map(|(_, t)| t)
            .expect("the mark")
    };
    let first = mark(&fx);
    let mut marks = vec![first.clone()];
    // 16 passes of 1/60 s: past one step.
    for _ in 0..16 {
        fx.idle();
        marks.push(mark(&fx));
    }
    assert!(marks.iter().any(|m| *m != first), "{marks:?}");
    let step = |d: &Duration| *d > Duration::ZERO && *d <= Duration::from_millis(250);
    assert!(delays.lock().unwrap().iter().any(step), "{delays:?}");
    // Nothing scanned: no step asked for.
    let mut rows = sample();
    rows[1].info.scanning = false;
    fx.host.view.set_rows(rows);
    fx.idle();
    delays.lock().unwrap().clear();
    for _ in 0..3 {
        fx.idle();
    }
    assert!(!delays.lock().unwrap().iter().any(step), "{delays:?}");
}

/// The colours are `chrome::tree`'s (AGENTS.md "One look", 05:83): folder names in the list's
/// folder colour, a hidden folder's row and the hidden-folders row dimmed, counts and sizes in
/// the text colour, tags dim, `?` the error colour, `not answering` the warning colour, the mark
/// its own; on the active bar every cell in the bar's text colour, on the bar's fill — so a red
/// `?` never vanishes on the red bar. An inactive tree's bar is the idle one.
#[test]
fn the_colours_are_chrome_trees() {
    let mut fx = sample_fx(0);
    let one = |fx: &Fx, t: &str| fx.drv.colors_of(t).unwrap_or_else(|| panic!("{t}"));
    for (text, color) in [
        ("~", LOOK.on_bar),
        ("5 d", LOOK.on_bar),
        ("52M+", LOOK.on_bar),
        ("src", LOOK.name),
        ("Desktop", LOOK.name),
        ("a/b/c", LOOK.name),
        ("19 d", LOOK.info),
        ("40 f", LOOK.info),
        ("9.8M+", LOOK.info),
        ("=87G fs", LOOK.info),
        ("· 2 hidden folders", LOOK.hidden),
        (".cache", LOOK.hidden),
        ("9 d+", LOOK.hidden),
        ("~1.2M+", LOOK.hidden),
        ("<LINK>", LOOK.tag),
        ("<SYS>", LOOK.tag),
        ("?", LOOK.unreadable),
        ("not answering", LOOK.stalled),
    ] {
        assert_eq!(one(&fx, text), [color], "{text}");
    }
    let glyph = fx.on_row("src", "▼").unwrap();
    let glyph_color = fx
        .drv
        .galleys()
        .into_iter()
        .find(|(pos, g)| g.text() == "▼" && g.rect.translate(pos.to_vec2()) == glyph)
        .map(|(_, g)| g.job.sections[0].format.color);
    assert_eq!(glyph_color, Some(LOOK.name), "a glyph as its name");
    let mark = fx
        .row_texts("src")
        .into_iter()
        .find(|(_, t)| SCAN_MARKS.iter().any(|m| m.to_string() == *t))
        .unwrap();
    assert_eq!(one(&fx, &mark.1), [LOOK.mark]);
    let on = |fx: &Fx, name: &str, fill| {
        let at = fx.drv.find_text(name).unwrap();
        fx.drv
            .rects()
            .iter()
            .any(|(r, f, _)| *f == fill && r.contains(at.center()))
    };
    assert!(on(&fx, "~", PALETTE.bar), "the bar behind the root");
    // The bar on `?`: white on the red bar, never the error colour there.
    assert_eq!(LOOK.unreadable, PALETTE.bar, "why it matters");
    fx.host.view.bar = 8;
    fx.idle();
    assert_eq!(one(&fx, "?"), [LOOK.on_bar]);
    assert_eq!(one(&fx, "Private"), [LOOK.on_bar]);
    assert_eq!(one(&fx, "~"), [LOOK.name]);
    // An inactive tree: the idle bar, the row in its own colours.
    fx.host.active = false;
    fx.idle();
    assert!(on(&fx, "Private", PALETTE.bar_idle));
    assert_eq!(one(&fx, "?"), [LOOK.unreadable]);
}

/// 05 §6.1 (05:185–198): sizes in the list's units (powers of 1000), the smallest unit that
/// fits, said as the info line says them — `60B`, `79K`, `9.8M` (one decimal under ten), `87G`
/// — with `+` for a lower bound and `~` for a cached figure; never longer than the size column.
#[test]
fn a_size_reads_in_the_smallest_unit() {
    for (n, want) in [
        (0, "0B"),
        (60, "60B"),
        (999, "999B"),
        (1_000, "1.0K"),
        (79_000, "79K"),
        (999_499, "999K"),
        (999_500, "1.0M"),
        (1_100_000, "1.1M"),
        (9_800_000, "9.8M"),
        (9_950_000, "10M"),
        (87_000_000_000, "87G"),
        (1_500_000_000_000, "1.5T"),
        (u64::MAX, "18E"),
    ] {
        assert_eq!(size_text(n, false, false), want, "{n}");
    }
    assert_eq!(size_text(60, true, false), "60B+");
    assert_eq!(size_text(9_800_000, false, true), "~9.8M");
    assert_eq!(size_text(9_800_000, true, true), "~9.8M+");
    let mut n: u64 = 1;
    while let Some(next) = n.checked_mul(7) {
        for m in [n - 1, n, n + 1] {
            let t = size_text(m, true, true);
            assert!(t.chars().count() <= 6, "{m}: {t}");
        }
        n = next;
    }
}

/// 05 §1 (05:29): the bar starts on the panel's folder with its row in the middle of the
/// tree; rows given anew later (a listing, a size) do not centre it again; near the end the
/// last row stays at the bottom.
#[test]
fn the_bar_starts_in_the_middle() {
    let mut fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 121));
    let inner = fx.inner();
    let visible = (inner.height() / CELL.y).floor() as usize;
    assert_eq!(fx.bar_name(), "f120");
    let at = fx.drv.find_text("f120").expect("the bar's row is drawn");
    assert_eq!(at.top(), inner.top() + CELL.y * (visible / 2) as f32);
    assert_eq!(fx.host.view.top, 121 - visible / 2);
    assert_eq!(
        fx.host.view.on_screen(),
        121 - visible / 2..121 + visible / 2
    );
    // Given anew: where it was.
    let mut rows = long(200);
    rows[50].info.size = bytes(10, false);
    fx.host.view.set_rows(rows);
    fx.idle();
    assert_eq!(fx.drv.find_text("f120"), Some(at));
    // Near the end: the last rows fill the bottom.
    let fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 199));
    assert_eq!(fx.host.view.top, 201 - visible);
    let last = fx.drv.find_text("f199").unwrap();
    assert_eq!(last.top(), inner.top() + CELL.y * (visible - 1) as f32);
}

/// 05 §1 (05:34): ↑ ↓ PgUp PgDn Home End move the bar, which stays on screen; they ask nothing
/// of the model.
#[test]
fn the_bar_keys_move_the_bar_and_keep_it_in_view() {
    let mut fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 0));
    let inner = fx.inner();
    let visible = (inner.height() / CELL.y).floor() as usize;
    let in_view = |fx: &Fx| {
        let name = fx.bar_name();
        let at = fx.drv.find_text(&name).unwrap_or_else(|| panic!("{name}"));
        assert!(inner.contains_rect(at), "{name} at {at:?}");
        at
    };
    assert_eq!(fx.host.view.top, 0);
    for _ in 0..3 {
        assert_eq!(fx.press(Key::ArrowDown), []);
    }
    assert_eq!(fx.host.view.bar, 3);
    in_view(&fx);
    assert_eq!(fx.press(Key::PageDown), []);
    assert_eq!(fx.host.view.bar, 3 + visible - 1);
    in_view(&fx);
    assert_eq!(fx.press(Key::End), []);
    assert_eq!(fx.host.view.bar, 200);
    let at = in_view(&fx);
    assert_eq!(at.top(), inner.top() + CELL.y * (visible - 1) as f32);
    assert_eq!(fx.press(Key::PageUp), []);
    assert_eq!(fx.host.view.bar, 200 - (visible - 1));
    in_view(&fx);
    assert_eq!(fx.press(Key::Home), []);
    assert_eq!((fx.host.view.bar, fx.host.view.top), (0, 0));
    in_view(&fx);
    assert_eq!(fx.press(Key::ArrowUp), []);
    assert_eq!(fx.host.view.bar, 0);
    // Held ↓ steps past the bottom: the rows scroll a row at a time.
    for _ in 0..visible {
        fx.press(Key::ArrowDown);
    }
    assert_eq!(fx.host.view.bar, visible);
    assert_eq!(fx.host.view.top, 1);
    in_view(&fx);
}

/// 05 §1 (05:30–35) through the key table's tree rows (keys.rs:493–508): F1 the help; F10,
/// Alt+F10 and Esc close; Enter goes to the row; → opens a closed folder, ← closes an open one
/// and on a closed one takes the bar to its parent; Space sizes; Cmd/Ctrl+R lists again; a
/// letter jumps to the next row starting with it (any case, around the end). Each names the
/// bar's row by its id.
#[test]
fn each_key_gives_its_action() {
    use TreeAction::*;
    let mut fx = sample_fx(1);
    assert_eq!(fx.press(Key::F1), [Help]);
    assert_eq!(fx.press(Key::F10), [Close]);
    assert_eq!(fx.key(Key::F10, Modifiers::ALT), [Close]);
    assert_eq!(fx.press(Key::Escape), [Close]);
    assert_eq!(fx.key(Key::F10, Modifiers::SHIFT), []);
    assert_eq!(fx.press(Key::Enter), [GoTo(1)]);
    // src is open (by itself): → has nothing to open, ← closes it.
    assert_eq!(fx.press(Key::ArrowRight), []);
    assert_eq!(fx.press(Key::ArrowLeft), [Collapse(1)]);
    // lib has no subfolders: ← takes the bar to src.
    fx.host.view.bar = 2;
    assert_eq!(fx.press(Key::ArrowLeft), []);
    assert_eq!(fx.bar_name(), "src");
    // a/b/c is closed: → opens it; ← takes the bar to the root.
    fx.host.view.bar = 4;
    assert_eq!(fx.press(Key::ArrowRight), [Expand(4)]);
    assert_eq!(fx.press(Key::Space), [SizeNow(4)]);
    assert_eq!(fx.key(Key::R, testkit::command(false)), [Reread(4)]);
    assert_eq!(fx.key(Key::R, Modifiers::NONE), []);
    assert_eq!(fx.press(Key::ArrowLeft), []);
    assert_eq!(fx.bar_name(), "~");
    // The root has no parent.
    fx.host.view.rows[0].fold = Fold::Closed;
    assert_eq!(fx.press(Key::ArrowLeft), []);
    assert_eq!(fx.host.view.bar, 0);
    // Letters.
    assert_eq!(fx.typed("d"), [Jump('d')]);
    assert_eq!(fx.bar_name(), "Desktop");
    assert_eq!(fx.typed("P"), [Jump('P')]);
    assert_eq!(fx.bar_name(), "proc");
    assert_eq!(fx.typed("p"), [Jump('p')]);
    assert_eq!(fx.bar_name(), "Private");
    assert_eq!(fx.typed("p"), [Jump('p')]);
    assert_eq!(fx.bar_name(), "proc", "around the end");
    assert_eq!(fx.typed("z"), [Jump('z')]);
    assert_eq!(fx.bar_name(), "proc", "no row starts with z");
    // A hidden folder's own row is a name like any; the hidden folders' row is not.
    assert_eq!(fx.typed("."), [Jump('.')]);
    assert_eq!(fx.bar_name(), ".cache");
    assert_eq!(fx.typed("·"), [Jump('·')]);
    assert_eq!(fx.bar_name(), ".cache");
}

/// 05 §3 (05:73–74, 05:81, 05:85): a link never opens in place — → does nothing, Enter goes to
/// it (its target, which the model resolves); → or Enter on the hidden folders' row opens it.
#[test]
fn a_link_never_opens_and_the_hidden_row_does() {
    use TreeAction::*;
    let mut fx = sample_fx(5);
    assert_eq!(fx.press(Key::ArrowRight), []);
    assert_eq!(fx.press(Key::Enter), [GoTo(5)]);
    fx.host.view.bar = 3;
    assert_eq!(fx.press(Key::ArrowRight), [Expand(3)]);
    assert_eq!(fx.press(Key::Enter), [Expand(3)]);
    // ← on it: the bar to its folder.
    assert_eq!(fx.press(Key::ArrowLeft), []);
    assert_eq!(fx.bar_name(), "src");
}

/// 05 §1 (05:36), 04 §2: the F-key row is `1Help` and `10Tree`, the other slots empty — the key
/// table's tree rows — and a click on slot 10 closes the tree as F10 does (04 §6.1).
#[test]
fn the_f_key_row_is_help_and_tree() {
    let pairs: Vec<(u8, &str)> = fkeys().iter().map(|k| (k.n, k.label)).collect();
    assert_eq!(pairs, [(1, "Help"), (10, "Tree")]);
    let mut fx = sample_fx(0);
    assert!(fx.drv.find_text("Help   ").is_some());
    assert!(fx.drv.find_text("Tree   ").is_some());
    for other in ["Rename", "View", "Edit", "Copy", "Move", "Dual", "MkDir"] {
        assert!(
            fx.drv.text_starting_with(other).is_none(),
            "{other} has no slot here"
        );
    }
    fx.drv.click_text(&mut fx.host, "Tree   ");
    fx.idle();
    assert_eq!(fx.host.actions, [TreeAction::Close]);
}

/// Rows given anew keep the bar on its row (by id) where it was on screen, though rows were
/// added above it; when its row is gone, the bar stays in the rows.
#[test]
fn rows_given_anew_keep_the_bar_on_its_row() {
    let mut fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 121));
    let at = fx.drv.find_text("f120").unwrap();
    let mut rows = long(200);
    let opened: Vec<TreeRow> = (0..5)
        .map(|k| {
            row(
                1000 + k,
                2,
                &format!("g{k}"),
                RowKind::Folder,
                Fold::Leaf,
                Info::default(),
            )
        })
        .collect();
    rows.splice(51..51, opened);
    fx.host.view.set_rows(rows);
    fx.idle();
    assert_eq!(fx.bar_name(), "f120");
    assert_eq!(fx.host.view.bar, 126);
    assert_eq!(fx.drv.find_text("f120"), Some(at), "where it was");
    // Its row gone: the bar on the last row, on screen.
    let mut rows = long(200);
    rows.truncate(30);
    fx.host.view.set_rows(rows);
    fx.idle();
    assert_eq!(fx.host.view.bar, 29);
    assert_eq!(fx.host.view.top, 0);
    assert!(fx.drv.find_text("f028").is_some());
    // None at all: nothing to name, nothing asked.
    fx.host.view.set_rows(Vec::new());
    fx.idle();
    assert_eq!(fx.press(Key::Enter), []);
    assert_eq!(fx.press(Key::ArrowDown), []);
}

/// As in the list (01 §3a): a click puts the bar on a row, a double click is Enter there; the
/// wheel scrolls the rows and leaves the bar.
#[test]
fn a_click_moves_the_bar_and_a_double_click_goes_to() {
    let mut fx = sample_fx(0);
    let abc = fx.drv.find_text("a/b/c").unwrap();
    fx.drv.click(&mut fx.host, abc.center());
    assert_eq!(fx.bar_name(), "a/b/c");
    assert_eq!(std::mem::take(&mut fx.host.actions), []);
    // Past egui's double-click delay (0.3 s), or the pair below counts as a triple click.
    for _ in 0..30 {
        fx.idle();
    }
    let src = fx.drv.find_text("src").unwrap();
    fx.drv.click(&mut fx.host, src.center());
    fx.drv.click(&mut fx.host, src.center());
    assert_eq!(fx.bar_name(), "src");
    assert_eq!(fx.host.actions, [TreeAction::GoTo(1)]);
    // The wheel.
    let mut fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 0));
    let middle = fx.inner().center();
    fx.drv.wheel(&mut fx.host, middle, -2.0);
    assert_eq!(fx.host.view.top, 6);
    assert_eq!(fx.host.view.bar, 0);
    assert!(fx.drv.find_text("f005").is_some());
    assert!(fx.drv.find_text("/").is_none(), "scrolled off");
}

/// Every glyph of the tree is a VGA font glyph, one cell wide (05:82–83).
#[test]
fn the_glyphs_are_one_cell_of_the_vga_font() {
    use crate::gui::chrome::tree::{CLOSED, HIDDEN, LINK, OPEN};
    let fx = sample_fx(0);
    fx.drv.ctx.fonts_mut(|f| {
        for c in [OPEN, CLOSED, LINK, HIDDEN].into_iter().chain(SCAN_MARKS) {
            assert!(f.has_glyph(&chrome::font(), c), "{c:?}");
            assert_eq!(f.glyph_width(&chrome::font(), c), CELL.x, "{c:?}");
        }
    });
}

/// A narrow side (under 48 cells, where the list drops its Date and Time columns) keeps the
/// size and drops the counts, so names keep their room.
#[test]
fn a_narrow_side_keeps_the_size_and_drops_the_counts() {
    let fx = Fx::new(640.0, TreeView::new("box:/srv/data", sample(), 0));
    assert!(fx.inner().width() < 48.0 * CELL.x);
    assert!(fx.drv.find_text("src").is_some());
    assert!(fx.drv.find_text("9.8M+").is_some());
    assert!(fx.drv.find_text("<LINK>").is_some());
    assert!(fx.drv.find_text("19 d").is_none());
    assert!(fx.drv.find_text("40 f").is_none());
    let name = fx.drv.find_text("src").unwrap();
    let size = fx.on_row("src", "9.8M+").unwrap();
    assert!(name.right() + CELL.x <= size.left());
}

/// A name longer than its room ends in "…" a cell clear of the info line (01 §3a: every cell
/// fits at a glyph boundary); a row deeper than the side has room for — the path to the panel's
/// folder is always drawn (05 §5 rule 1), the bar on its last row (05:29) — keeps its glyph and
/// at least twelve cells of its name, drawn at the deepest indent that leaves them, on a wide
/// side and on a narrow one.
#[test]
fn long_names_are_cut_and_deep_rows_keep_their_names() {
    let long_name = "a-folder-name-much-longer-than-the-room-it-has-in-the-tree";
    let mut rows = vec![row(0, 0, "~", RowKind::Folder, Fold::Open, Info::default())];
    rows.push(row(
        1,
        1,
        long_name,
        RowKind::Folder,
        Fold::Closed,
        counted(19, 40, bytes(9_800_000, true)),
    ));
    let deep = 24;
    rows.extend((1..=deep).map(|d| {
        row(
            1 + d,
            d,
            &format!("level-{d:02}-folder"),
            RowKind::Folder,
            if d == deep { Fold::Leaf } else { Fold::Open },
            counted(1, 3, bytes(4_000, false)),
        )
    }));
    let last = rows.len() - 1;
    for width in [1000.0, 640.0] {
        let fx = Fx::new(width, TreeView::new("box:/srv/data", rows.clone(), last));
        let inner = fx.inner();
        let cut = fx
            .drv
            .text_starting_with("a-folder-name")
            .unwrap_or_else(|| panic!("{width}: the long name"));
        assert!(cut.ends_with('…') && cut.len() < long_name.len(), "{cut}");
        let cut_at = fx.drv.find_text(&cut).unwrap();
        let size = fx.on_row(&cut, "9.8M+").unwrap();
        let first = fx
            .row_texts(&cut)
            .into_iter()
            .find(|(r, _)| r.left() > cut_at.left())
            .unwrap();
        assert!(
            cut_at.right() + CELL.x <= first.0.left(),
            "{width}: {first:?}"
        );
        assert!(size.right() <= inner.right());
        for d in [12, 16, deep] {
            let name = format!("level-{d:02}-folder");
            let drawn = fx
                .drv
                .text_starting_with(&format!("level-{d:02}"))
                .unwrap_or_else(|| panic!("{width}: {name} is not drawn"));
            assert!(drawn.chars().count() >= 12, "{width}: {drawn}");
            let at = fx.drv.find_text(&drawn).unwrap();
            assert!(inner.contains_rect(at), "{width}: {name} at {at:?}");
            let texts = fx.row_texts(&drawn);
            let glyph = texts.iter().find(|(r, _)| r.right() <= at.left());
            if d < deep {
                assert_eq!(glyph.map(|g| g.1.as_str()), Some("▼"), "{width}: {name}");
            }
            let info = texts.iter().find(|(r, _)| r.left() > at.left()).unwrap();
            assert!(at.right() + CELL.x <= info.0.left(), "{width}: {name}");
        }
    }
}

/// The status row (05 §6.1, 05:203–204; §6.2, 05:227–228): what the whole tree says comes
/// first; else, on a boundary row, what its filesystem holds; else what the keys do.
#[test]
fn the_status_line_says_the_tree_then_the_bar_row() {
    let mut view = TreeView::new("box:/srv/data", sample(), 7);
    let said = view.status_line();
    assert!(!said.error);
    for part in ["home", "87G", "1234567 files"] {
        assert!(said.text.contains(part), "{part}: {}", said.text);
    }
    view.bar = 1;
    let hint = view.status_line();
    assert!(
        hint.text.contains("F1") && hint.text.contains("Esc"),
        "{}",
        hint.text
    );
    let cap = Status::error("4 calls not answering; the tree waits for one to return");
    view.status = Some(cap.clone());
    view.bar = 7;
    let said = view.status_line();
    assert_eq!((said.text, said.error), (cap.text, true));
}

/// What [`keys`] asks for over one frame of `events`, before the view was ever drawn — the
/// calling order allows it (keys first, then the pass draws).
fn keys_before_any_draw(view: &mut TreeView, events: Vec<Event>) -> Vec<TreeAction> {
    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        events,
        ..Default::default()
    };
    let mut asked = Vec::new();
    let mut out = ctx.run_ui(raw, |ui| asked = keys(ui.ctx(), view));
    out.textures_delta.clear();
    asked
}

/// Review round 1: a bar (and a first row) the caller put past the end — the fields are
/// public — is taken as the last row before a key acts, never added to: a letter jumps around
/// the end from there, Enter names the last row, an empty tree asks nothing; the rows on screen
/// stay inside the rows.
#[test]
fn a_key_with_the_bar_past_the_end_acts_on_the_last_row() {
    use TreeAction::*;
    let one = || {
        vec![row(
            7,
            0,
            "a",
            RowKind::Folder,
            Fold::Closed,
            Info::default(),
        )]
    };
    let mut view = TreeView::new("box:/srv/data", one(), usize::MAX);
    let typed = |t: &str| vec![Event::Text(t.into())];
    let enter = || {
        vec![Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]
    };
    assert_eq!(keys_before_any_draw(&mut view, typed("a")), [Jump('a')]);
    assert_eq!(view.bar, 0);
    view.bar = usize::MAX;
    assert_eq!(keys_before_any_draw(&mut view, enter()), [GoTo(7)]);
    assert_eq!(view.bar, 0);
    // None at all.
    let mut empty = TreeView::new("box:/srv/data", Vec::new(), usize::MAX);
    assert_eq!(keys_before_any_draw(&mut empty, typed("a")), [Jump('a')]);
    assert_eq!(keys_before_any_draw(&mut empty, enter()), []);
    assert_eq!(empty.bar, 0);
    // The first row past the end too: the rows on screen and rows given anew stay in range.
    let mut fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 0));
    fx.host.view.top = usize::MAX;
    let on = fx.host.view.on_screen();
    assert!(on.start <= on.end && on.end <= 201, "{on:?}");
    fx.host.view.bar = usize::MAX;
    fx.host.view.set_rows(long(200));
    fx.host.view.top = usize::MAX;
    fx.host.view.set_rows(long(10));
    fx.idle();
    assert!(fx.host.view.top <= 10 && fx.host.view.bar <= 10);
    assert!(fx.drv.find_text(&fx.bar_name()).is_some());
}

/// Review round 1: rows that shrink under the bar between passes (the caller replaced them
/// without `set_rows`), then a key before the next draw: it acts on the last row as it is now —
/// Enter goes to it, ← closes it, a letter jumps from it.
#[test]
fn rows_shrinking_under_the_bar_then_a_key() {
    use TreeAction::*;
    let mut fx = sample_fx(11);
    assert_eq!(fx.bar_name(), "new");
    fx.host.view.rows.truncate(2);
    assert_eq!(fx.press(Key::Enter), [GoTo(1)]);
    assert_eq!(fx.bar_name(), "src");
    fx.host.view.rows = sample();
    fx.host.view.bar = 11;
    fx.idle();
    fx.host.view.rows.truncate(2);
    assert_eq!(fx.press(Key::ArrowLeft), [Collapse(1)]);
    fx.host.view.rows = sample();
    fx.host.view.bar = 11;
    fx.idle();
    fx.host.view.rows.truncate(2);
    assert_eq!(fx.typed("~"), [Jump('~')]);
    assert_eq!(fx.bar_name(), "~");
}

/// Review round 1: a row at the greatest depth a `usize` holds is drawn as any too-deep row —
/// at the deepest indent that keeps its name, inside the frame — never wrapped to the left.
#[test]
fn a_row_at_the_greatest_depth_is_drawn_inside_the_frame() {
    for depth in [isize::MAX as usize, usize::MAX] {
        let rows = vec![
            row(0, 0, "~", RowKind::Folder, Fold::Open, Info::default()),
            row(
                1,
                depth,
                "deepest",
                RowKind::Folder,
                Fold::Closed,
                counted(1, 2, bytes(3_000, false)),
            ),
            row(
                2,
                depth,
                "",
                RowKind::Hidden(3),
                Fold::Leaf,
                Info::default(),
            ),
        ];
        let fx = Fx::new(1000.0, TreeView::new("box:/srv/data", rows, 1));
        let inner = fx.inner();
        let name = fx.drv.find_text("deepest").expect("the name is drawn");
        assert!(inner.contains_rect(name), "{depth}: {name:?} in {inner:?}");
        let glyph = fx.on_row("deepest", "►").expect("its glyph");
        assert!(inner.contains_rect(glyph) && glyph.right() <= name.left());
        let root = fx.drv.find_text("~").unwrap();
        assert!(glyph.left() > root.left(), "{depth}: deeper than the root");
        // Its glyph and name cells: cut at a glyph boundary, as any name that long.
        let hidden = fx
            .drv
            .text_starting_with("· 3 hidden")
            .expect("the hidden row");
        let at = fx.drv.find_text(&hidden).unwrap();
        assert!(inner.contains_rect(at), "{depth}: {hidden} at {at:?}");
        assert_eq!(at.left(), glyph.left(), "{depth}: in the glyph's place");
    }
}
