//! The tree as a side's content, in cells (10 §4, X4-tree), driven by real egui key and pointer
//! events through the test kit: its rows, glyphs and info line at their cells (05 §3, §6.1), its
//! colours (`chrome::tree`), the bar and its keys (05 §1), its F-key row, the cells a dialog or the
//! help covers blanked first, §10's stand-ins in an ambiguous-wide terminal (T38); a model's rows
//! drawn, a `<SYS>` row from a fake mount table among them (05 `T-norules`); TS-4 — the same cells
//! in the window and in the terminal, a row being scanned, its mark moving; and a `SOPEN_CELLS`
//! run's wait for final sizes, capped by its watchdog.

use super::*;
use crate::gui::cells::{Layer, Seen};
use crate::gui::chrome::tree::{LOOK, SCAN_MARKS};
use crate::gui::chrome::{CELL, PALETTE};
use crate::gui::testkit::{self, Driver};
use crate::gui::Window;
use eframe::egui::{Event, Key, Modifiers, Pos2, Rect};
use std::sync::Arc;

/// The tree on the left side of the window's cell grid, as the browser lays a side out (10 §4),
/// with what its keys and clicks asked for.
struct Host {
    view: TreeView,
    active: bool,
    actions: Vec<TreeAction>,
    /// The cells a dialog of the list's stands on: blanked first.
    under: Vec<Cells>,
    /// The grid and the side the last pass drew the tree at.
    at: Option<(Grid, Cells)>,
}

impl Host {
    fn new(view: TreeView) -> Host {
        Host {
            view,
            active: true,
            actions: Vec::new(),
            under: Vec::new(),
            at: None,
        }
    }
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.actions.extend(keys(&ctx, &mut self.view));
        chrome::key_panel(ui, "tree-test-keys", &fkeys());
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let grid = Grid::over(ui.max_rect());
                let side = cells::halves(grid.all())[0];
                self.at = Some((grid, side));
                let asked = draw(ui, &grid, side, &mut self.view, self.active, &self.under);
                self.actions.extend(asked);
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
            host: Host::new(view),
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

    /// The tree's layer of cells: the one over all of its side.
    fn layer(&self) -> Arc<Layer> {
        let (_, side) = self.host.at.expect("the tree was drawn");
        cells::layers(&self.drv.ctx)
            .into_iter()
            .find(|l| l.buf.area == side)
            .expect("the tree's cells over its side")
    }

    /// The cells inside the frame.
    fn inner(&self) -> Cells {
        let (_, s) = self.host.at.expect("the tree was drawn");
        Cells::new(s.x + 1, s.y + 1, s.width - 2, s.height - 2)
    }

    /// The rows the tree has room for.
    fn visible(&self) -> usize {
        usize::from(self.inner().height)
    }

    /// The cells inside the frame, a row at a time.
    fn rows(&self) -> Vec<Vec<Seen>> {
        let inner = self.inner();
        let l = self.layer();
        let a = l.buf.area;
        let seen = l.seen();
        let (from, to) = (usize::from(inner.x - a.x), usize::from(inner.right() - a.x));
        (inner.top()..inner.bottom())
            .map(|y| seen[usize::from(y - a.y)][from..to].to_vec())
            .collect()
    }

    /// Cell `col` of row `row` inside the frame.
    fn cell(&self, col: u16, row: u16) -> Seen {
        self.rows()[usize::from(row)][usize::from(col)].clone()
    }

    /// Every place `text` stands inside the frame as a whole — a blank or the row's end either
    /// side — top to bottom: its first cell's column and its row.
    fn find_all(&self, text: &str) -> Vec<(u16, u16)> {
        let mut out = vec![];
        for (row, cells_of_row) in self.rows().iter().enumerate() {
            let (mut s, mut at) = (String::new(), vec![]);
            for (col, c) in cells_of_row.iter().enumerate() {
                for ch in c.symbol.chars() {
                    s.push(ch);
                    at.push(col as u16);
                }
            }
            let edge = |c: Option<char>| c.is_none_or(|c| c == ' ');
            for (start, _) in s.match_indices(text) {
                let before = s[..start].chars().last();
                let after = s[start + text.len()..].chars().next();
                if edge(before) && edge(after) {
                    out.push((at[s[..start].chars().count()], row as u16));
                }
            }
        }
        out
    }

    /// Where `text` first stands whole inside the frame (as [`Fx::find_all`]).
    fn find(&self, text: &str) -> Option<(u16, u16)> {
        self.find_all(text).first().copied()
    }

    /// What row `row` shows from column `from` to `to` (exclusive), as pieces two blanks apart at
    /// least: each with its first column and its end (exclusive).
    fn pieces(&self, row: u16, from: u16, to: u16) -> Vec<(u16, u16, String)> {
        let cells_of_row = &self.rows()[usize::from(row)];
        let blank = |c: u16| cells_of_row[usize::from(c)].symbol == " ";
        let mut out: Vec<(u16, u16, String)> = vec![];
        let mut c = from;
        while c < to {
            if blank(c) {
                c += 1;
                continue;
            }
            let start = c;
            let mut text = String::new();
            let mut end = c;
            while c < to && !(blank(c) && (c + 1 >= to || blank(c + 1))) {
                text.push_str(&cells_of_row[usize::from(c)].symbol);
                if !blank(c) {
                    end = c + 1;
                }
                c += 1;
            }
            out.push((start, end, text.trim_end().to_string()));
        }
        out
    }

    /// The piece (as [`Fx::pieces`]) from where `prefix` starts a word — after a blank or at the
    /// row's start — with its first column and its row.
    fn piece_starting_with(&self, prefix: &str) -> Option<(u16, u16, String)> {
        let width = self.inner().width;
        for (row, cells_of_row) in self.rows().iter().enumerate() {
            for col in 0..usize::from(width) {
                if col > 0 && cells_of_row[col - 1].symbol != " " {
                    continue;
                }
                let rest: String = cells_of_row[col..]
                    .iter()
                    .map(|c| c.symbol.as_str())
                    .collect();
                if rest.starts_with(prefix) {
                    let row = row as u16;
                    let (start, _, text) = self.pieces(row, col as u16, width).remove(0);
                    return Some((start, row, text));
                }
            }
        }
        None
    }

    /// The foreground colours of the cells `text` stands on (where [`Fx::find`] finds it), its
    /// blanks left out, repeats merged.
    fn fg_of(&self, text: &str) -> Vec<Color32> {
        let (col, row) = self
            .find(text)
            .unwrap_or_else(|| panic!("{text} not found"));
        let mut out: Vec<Color32> = vec![];
        let row_cells = &self.rows()[usize::from(row)];
        let n = cells::width(text);
        for c in &row_cells[usize::from(col)..usize::from(col) + n] {
            let fg = c.fg.expect("an RGB foreground");
            if c.symbol.trim().is_empty() || out.last() == Some(&fg) {
                continue;
            }
            out.push(fg);
        }
        out
    }

    /// Cells `col`.. (`width` of them) of row `row` inside the frame, on screen.
    fn on_screen(&self, col: u16, row: u16, width: u16) -> Rect {
        let inner = self.inner();
        self.layer()
            .rect(Cells::new(inner.x + col, inner.y + row, width, 1))
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
        files: Some(1_234_567),
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

/// 05 §3 (05:69–86), T-rows' look, in cells: one layer over all of the side, its frame titled with
/// the root (double-lined: the tree has the keyboard); a row's glyph — `▼` open (by the owner or by
/// itself), `►` closed, none for a folder with no subfolders, `→` a link — in its own cell, a blank
/// cell, then its name: two cells deeper a level; a compact chain on one row; the hidden subfolders
/// as one row `· N hidden folders` in the glyph's place; a row a cell tall, in the order given.
#[test]
fn rows_draw_their_glyphs_names_and_chains() {
    let fx = sample_fx(0);
    let l = fx.layer();
    let a = l.buf.area;
    let top = cells::row_text(&l.buf, a.y).0;
    assert!(
        top.contains(" box:/srv/data "),
        "the frame's title: {top:?}"
    );
    assert_eq!(l.buf[(a.x, a.y)].symbol(), "╔", "the active tree's frame");
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
        let (col, row) = fx.find(name).unwrap_or_else(|| panic!("{name}"));
        let d = 2 * depth as u16;
        assert_eq!(col, d + 2, "{name}'s indent");
        assert_eq!(fx.cell(d + 1, row).symbol, " ", "{name}: a blank before it");
        assert_eq!(
            fx.cell(d, row).symbol,
            glyph.unwrap_or(" "),
            "{name}'s glyph"
        );
        for c in 0..d {
            assert_eq!(
                fx.cell(c, row).symbol,
                " ",
                "{name}: blank before its glyph"
            );
        }
    }
    let (col, row) = fx.find("· 2 hidden folders").expect("the hidden row");
    assert_eq!(col, 4, "in the glyph's place at depth 2");
    assert_eq!(row, fx.find("lib").unwrap().1 + 1, "below lib's");
    for (name, k) in [
        ("~", 0),
        ("src", 1),
        ("lib", 2),
        ("a/b/c", 4),
        ("Desktop", 5),
        ("new", 11),
    ] {
        assert_eq!(fx.find(name).unwrap().1, k, "{name}'s row");
    }
}

/// 05 §6.1 (05:168–211) with §6.3's lower bounds and mark (05:250, 05:264), in cells: each row
/// ends with three right-aligned columns — `d`, `f` and the size, a tag in the size's place — the
/// counts with `+` where a listing stopped early, the size with `+` (more below), `~` (cached),
/// `=… fs` (a boundary's filesystem); nothing for a folder not listed yet. The size column ends
/// two cells before the frame: a blank, then the scan's mark, one cell, on the row being scanned
/// alone. `not answering`, wider than the size column, overlaps nothing and moves its row's
/// counts left.
#[test]
fn the_info_line_reads_each_kind_of_row() {
    let fx = sample_fx(0);
    let chars = fx.inner().width;
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
    let mut ends: [Vec<(u16, &str)>; 3] = [vec![], vec![], vec![]];
    for (name, pieces) in want {
        let (col, row) = fx.find(name).unwrap();
        let name_end = col + cells::width(name) as u16;
        // Right of the name, up to the mark's cell (the last).
        let got = fx.pieces(row, name_end, chars - 1);
        let shown: Vec<&str> = got.iter().map(|(_, _, t)| t.as_str()).collect();
        assert_eq!(shown, *pieces, "{name}");
        let n = pieces.len();
        if !pieces.contains(&"not answering") {
            for (k, (_, end, _)) in got.iter().enumerate() {
                ends[3 - n + k].push((*end, name));
            }
        }
        if let Some((start, _, _)) = got.first() {
            assert!(name_end < *start, "{name}: a blank after the name");
        }
        if let Some((_, end, _)) = got.last() {
            assert_eq!(*end, chars - 2, "{name}: the size column's end");
        }
    }
    for (column, edges) in ends.iter().enumerate() {
        assert!(
            edges.iter().all(|(e, _)| *e == edges[0].0),
            "column {column}: {edges:?}"
        );
    }
    // The mark: in the last cell, after a blank, on the row being scanned and no other.
    let marks: Vec<String> = SCAN_MARKS.iter().map(char::to_string).collect();
    for (name, _) in want {
        let (_, row) = fx.find(name).unwrap();
        let last = fx.cell(chars - 1, row).symbol;
        if *name == "src" {
            assert!(marks.contains(&last), "src is being scanned: {last:?}");
            assert_eq!(fx.cell(chars - 2, row).symbol, " ");
        } else {
            assert_eq!(last, " ", "{name} is not being scanned");
        }
    }
}

/// 05 §6.3 (05:264–265): the mark after a size being scanned moves — another glyph a quarter
/// of a second later — and the tree asks to be painted again for its next step, by a quarter
/// of a second at most; with no row being scanned it asks for none and shows none.
#[test]
fn the_scan_mark_moves() {
    use std::sync::Mutex;
    use std::time::Duration;
    let mut fx = sample_fx(0);
    let delays = Arc::new(Mutex::new(Vec::new()));
    let seen = delays.clone();
    fx.drv
        .ctx
        .set_repaint_observer(move |info| seen.lock().unwrap().push(info.delay));
    let mark = |fx: &Fx| {
        let (_, row) = fx.find("src").unwrap();
        fx.cell(fx.inner().width - 1, row).symbol
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
    // Nothing scanned: no step asked for, no mark.
    let mut rows = sample();
    rows[1].info.scanning = false;
    fx.host.view.set_rows(rows);
    fx.idle();
    delays.lock().unwrap().clear();
    for _ in 0..3 {
        fx.idle();
    }
    assert!(!delays.lock().unwrap().iter().any(step), "{delays:?}");
    assert_eq!(mark(&fx), " ");
}

/// The colours are `chrome::tree`'s (AGENTS.md "One look", 05:83), a cell's foreground each:
/// folder names and their glyphs in the list's folder colour, a hidden folder's row and the
/// hidden-folders row dimmed, counts and sizes in the text colour, tags dim, `?` the error colour,
/// `not answering` the warning colour, the mark its own; on the active bar every cell in the bar's
/// text colour, on the bar's colour across the row — so a red `?` never vanishes on the red bar.
/// An inactive tree's bar is the idle one; every other row is on the body's colour.
#[test]
fn the_colours_are_chrome_trees() {
    let mut fx = sample_fx(0);
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
        assert_eq!(fx.fg_of(text), [color], "{text}");
    }
    let (_, src) = fx.find("src").unwrap();
    assert_eq!(fx.cell(2, src).fg, Some(LOOK.name), "a glyph as its name");
    let width = fx.inner().width;
    assert_eq!(fx.cell(width - 1, src).fg, Some(LOOK.mark), "the mark");
    let backs = |fx: &Fx, row: u16| -> Vec<Color32> {
        let mut out: Vec<Color32> = vec![];
        for c in &fx.rows()[usize::from(row)] {
            let bg = c.bg.expect("an RGB background");
            if out.last() != Some(&bg) {
                out.push(bg);
            }
        }
        out
    };
    assert_eq!(
        backs(&fx, 0),
        [PALETTE.bar],
        "the bar across the root's row"
    );
    assert_eq!(backs(&fx, src), [PALETTE.back]);
    // The bar on `?`: white on the red bar, never the error colour there.
    assert_eq!(LOOK.unreadable, PALETTE.bar, "why it matters");
    fx.host.view.bar = 8;
    fx.idle();
    assert_eq!(fx.fg_of("?"), [LOOK.on_bar]);
    assert_eq!(fx.fg_of("Private"), [LOOK.on_bar]);
    assert_eq!(fx.fg_of("~"), [LOOK.name]);
    // An inactive tree: the idle bar, the row in its own colours, a single-lined frame.
    fx.host.active = false;
    fx.idle();
    let (_, private) = fx.find("Private").unwrap();
    assert_eq!(backs(&fx, private), [PALETTE.bar_idle]);
    assert_eq!(fx.fg_of("?"), [LOOK.unreadable]);
    let l = fx.layer();
    assert_eq!(l.buf[(l.buf.area.x, l.buf.area.y)].symbol(), "┌");
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
    let visible = fx.visible();
    assert_eq!(fx.bar_name(), "f120");
    let at = fx.find("f120").expect("the bar's row is drawn");
    assert_eq!(usize::from(at.1), visible / 2);
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
    assert_eq!(fx.find("f120"), Some(at));
    // Near the end: the last rows fill the bottom.
    let fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 199));
    assert_eq!(fx.host.view.top, 201 - visible);
    assert_eq!(usize::from(fx.find("f199").unwrap().1), visible - 1);
}

/// 05 §1 (05:34): ↑ ↓ PgUp PgDn Home End move the bar, which stays on screen; they ask nothing
/// of the model.
#[test]
fn the_bar_keys_move_the_bar_and_keep_it_in_view() {
    let mut fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 0));
    let visible = fx.visible();
    let in_view = |fx: &Fx| {
        let name = fx.bar_name();
        let at = fx.find(&name).unwrap_or_else(|| panic!("{name}"));
        assert!(usize::from(at.1) < visible, "{name} at {at:?}");
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
    assert_eq!(usize::from(in_view(&fx).1), visible - 1);
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
    // Two keys in one pass: both move the bar.
    let down = || Event::Key {
        key: Key::ArrowDown,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    fx.drv
        .frame(&mut fx.host, vec![down(), down()], Modifiers::NONE);
    fx.idle();
    assert_eq!(fx.host.view.bar, visible + 2);
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
    let at = fx.find("f120").unwrap();
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
    assert_eq!(fx.find("f120"), Some(at), "where it was");
    // Its row gone: the bar on the last row, on screen.
    let mut rows = long(200);
    rows.truncate(30);
    fx.host.view.set_rows(rows);
    fx.idle();
    assert_eq!(fx.host.view.bar, 29);
    assert_eq!(fx.host.view.top, 0);
    assert!(fx.find("f028").is_some());
    // None at all: nothing to name, nothing asked.
    fx.host.view.set_rows(Vec::new());
    fx.idle();
    assert_eq!(fx.press(Key::Enter), []);
    assert_eq!(fx.press(Key::ArrowDown), []);
}

/// As in the list (01 §3a, `list_cells.rs`), by cells: a click on a row's cells puts the bar
/// there, a double click is Enter there — also a press and its release in one pass; the wheel
/// scrolls the rows and leaves the bar.
#[test]
fn a_click_moves_the_bar_and_a_double_click_goes_to() {
    let mut fx = sample_fx(0);
    let (col, row) = fx.find("a/b/c").unwrap();
    // The row's last cell, far from its name: the row's cells take the click.
    let width = fx.inner().width;
    let at = fx.on_screen(width - 3, row, 1).center();
    let _ = col;
    fx.drv.click(&mut fx.host, at);
    assert_eq!(fx.bar_name(), "a/b/c");
    assert_eq!(std::mem::take(&mut fx.host.actions), []);
    // Past egui's double-click delay (0.3 s), or the pair below counts as a triple click.
    for _ in 0..30 {
        fx.idle();
    }
    let (col, row) = fx.find("src").unwrap();
    let src = fx.on_screen(col, row, 3).center();
    fx.drv.click(&mut fx.host, src);
    fx.drv.click(&mut fx.host, src);
    assert_eq!(fx.bar_name(), "src");
    assert_eq!(fx.host.actions, [TreeAction::GoTo(1)]);
    for _ in 0..30 {
        fx.idle();
    }
    // A press and its release in one pass, on Desktop's cells.
    let (col, row) = fx.find("Desktop").unwrap();
    let p = fx.on_screen(col, row, 7).center();
    let button = |pressed| Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    fx.drv.frame(
        &mut fx.host,
        vec![Event::PointerMoved(p), button(true), button(false)],
        Modifiers::NONE,
    );
    fx.idle();
    assert_eq!(fx.bar_name(), "Desktop");
    // The wheel.
    let mut fx = Fx::new(1000.0, TreeView::new("box:/", long(200), 0));
    let middle = fx.on_screen(10, 10, 1).center();
    fx.drv.wheel(&mut fx.host, middle, -2.0);
    assert_eq!(fx.host.view.top, 6);
    assert_eq!(fx.host.view.bar, 0);
    assert_eq!(fx.find("f005"), Some((4, 0)));
    assert!(fx.find("/").is_none(), "scrolled off");
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
    assert!(fx.inner().width < 48);
    let (col, row) = fx.find("src").expect("src");
    let size = fx.find("9.8M+").expect("its size");
    assert_eq!(size.1, row);
    assert!(fx.find("<LINK>").is_some());
    assert!(fx.find("19 d").is_none() && fx.find("40 f").is_none());
    assert!(col + 3 < size.0, "a blank between: {col} {size:?}");
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
        let (start, row, cut) = fx
            .piece_starting_with("a-folder-name")
            .unwrap_or_else(|| panic!("{width}: the long name"));
        assert!(cut.ends_with('…') && cut.len() < long_name.len(), "{cut}");
        let end = start + cells::width(&cut) as u16;
        let size = fx.find("9.8M+").unwrap();
        assert_eq!(size.1, row);
        let first = fx.pieces(row, end, fx.inner().width)[0].0;
        assert!(end < first, "{width}: a blank after the cut name");
        for d in [12, 16, deep] {
            let name = format!("level-{d:02}");
            let (col, row, drawn) = fx
                .piece_starting_with(&name)
                .unwrap_or_else(|| panic!("{width}: {name} is not drawn"));
            // The piece holds the name and, a blank or two on, maybe the counts: the name is its
            // first word.
            let drawn = drawn.split(' ').next().unwrap().to_string();
            assert!(drawn.chars().count() >= 12, "{width}: {drawn}");
            if d < deep {
                assert!(col >= 2, "{width}: room for its glyph");
                assert_eq!(fx.cell(col - 2, row).symbol, "▼", "{width}: {name}");
            }
            let end = col + cells::width(&drawn) as u16;
            let info = fx.pieces(row, end, fx.inner().width);
            assert!(end < info[0].0, "{width}: {name}");
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
    assert!(fx.find(&fx.bar_name()).is_some());
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
        let (col, row) = fx.find("deepest").expect("the name is drawn");
        assert!(col + 7 <= fx.inner().width, "{depth}: inside the frame");
        assert_eq!(fx.cell(col - 2, row).symbol, "►", "{depth}: its glyph");
        assert!(
            col - 2 > fx.find("~").unwrap().0,
            "{depth}: deeper than the root"
        );
        let (at, _, _) = fx
            .piece_starting_with("· 3 hidden")
            .expect("the hidden row");
        assert_eq!(at, col - 2, "{depth}: in the glyph's place");
    }
}

// ---------------------------------------------------------------------------------------------
// X4-tree: Hangul by cells, what covers the tree, an ambiguous-wide terminal, a model's rows.

/// 10 §10, 01 §3a: a Hangul name takes two cells a glyph — its second cell holds nothing — and a
/// name cut to its room ends in "…" a cell earlier rather than split a glyph: a cut name is never
/// wider than the room it had, a cell clear of the info line.
#[test]
fn hangul_names_take_two_cells_a_glyph_and_are_cut_whole() {
    let long_name = "아주긴한글폴더이름이방에다들어가지않는다정말로";
    let rows = vec![
        row(0, 0, "~", RowKind::Folder, Fold::Open, Info::default()),
        row(
            1,
            1,
            "한글 폴더",
            RowKind::Folder,
            Fold::Closed,
            counted(2, 3, bytes(4_000, false)),
        ),
        row(
            2,
            1,
            long_name,
            RowKind::Folder,
            Fold::Closed,
            counted(1, 1, bytes(10, false)),
        ),
    ];
    for width in [1000.0, 640.0, 656.0] {
        let fx = Fx::new(width, TreeView::new("box:/srv/data", rows.clone(), 0));
        let (col, row) = fx.find("한글 폴더").expect("the Hangul name");
        assert_eq!(col, 4);
        assert_eq!(fx.cell(col, row).symbol, "한");
        assert_eq!(fx.cell(col + 1, row).symbol, "", "its second cell");
        assert_eq!(fx.cell(col + 2, row).symbol, "글");
        assert_eq!(fx.cell(col + 4, row).symbol, " ");
        let (start, row, cut) = fx.piece_starting_with("아주").expect("the long name");
        assert!(cut.ends_with('…'), "{width}: {cut}");
        assert!(long_name.starts_with(cut.trim_end_matches('…')));
        let end = start + cells::width(&cut) as u16;
        let info = fx.pieces(row, end, fx.inner().width);
        assert!(end < info[0].0, "{width}: a blank after {cut:?}");
        // Never half a glyph: every cell of the cut is a glyph's first or its second.
        let cells_of_row = &fx.rows()[usize::from(row)];
        for c in start..end {
            let s = &cells_of_row[usize::from(c)].symbol;
            let first = cells::width(s) == 2 || s == "…";
            assert!(first || s.is_empty(), "{width}: cell {c} {s:?}");
        }
    }
}

/// 10 §4 (cells.rs: one buffer over another), as the preview's side does (`preview.rs` `ui`): the
/// cells a dialog of the list's stands on, and those under the key help, are blanks in the body's
/// colours in the tree's own buffer — a wide glyph half under the edge erased whole, as a terminal
/// erases it — and nothing else changes.
#[test]
fn a_dialog_and_the_help_over_the_tree_blank_its_cells_first() {
    let rows = vec![
        row(0, 0, "~", RowKind::Folder, Fold::Open, Info::default()),
        row(
            1,
            1,
            "한글 폴더",
            RowKind::Folder,
            Fold::Closed,
            counted(2, 3, bytes(4_000, false)),
        ),
        row(2, 1, "plain", RowKind::Folder, Fold::Leaf, Info::default()),
    ];
    let mut fx = Fx::new(1000.0, TreeView::new("box:/srv/data", rows, 0));
    let inner = fx.inner();
    let (col, row) = fx.find("한글 폴더").unwrap();
    // A dialog from the name's second cell (its first glyph's second half) on, three rows down.
    let under = Cells::new(inner.x + col + 1, inner.y + row, 20, 3);
    fx.host.under = vec![under];
    fx.idle();
    let body = (Some(PALETTE.text), Some(PALETTE.back));
    let l = fx.layer();
    for y in under.top()..under.bottom() {
        for x in under.left()..under.right() {
            let c = &l.seen()[usize::from(y - l.buf.area.y)][usize::from(x - l.buf.area.x)];
            assert_eq!(
                (c.symbol.as_str(), c.fg, c.bg),
                (" ", body.0, body.1),
                "({x},{y})"
            );
        }
    }
    assert_eq!(
        fx.cell(col, row).symbol,
        " ",
        "the glyph half under the edge, erased"
    );
    assert_eq!(fx.cell(col - 2, row).symbol, "►", "its glyph stays");
    assert!(fx.find("plain").is_none(), "under the dialog");
    // The key help over the tree: the cells it stands on are blank in the tree's layer.
    fx.host.under.clear();
    help::open(
        &fx.drv.ctx,
        help::for_context(Context::Tree, super::super::view::Mode::default(), &[]),
    );
    fx.idle();
    let (grid, _) = fx.host.at.unwrap();
    let covered = help::covered(&fx.drv.ctx, &grid);
    assert!(!covered.is_empty(), "the help covers the side");
    let l = fx.layer();
    let a = l.buf.area;
    let seen = l.seen();
    let mut checked = 0;
    for r in covered.iter().map(|r| r.intersection(a)) {
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                let c = &seen[usize::from(y - a.y)][usize::from(x - a.x)];
                assert_eq!((c.symbol.as_str(), c.bg), (" ", body.1), "({x},{y})");
                checked += 1;
            }
        }
    }
    assert!(checked > 0, "some of the tree's cells under the help");
}

/// 10 §10, T38: in a terminal that draws a glyph of ambiguous width in two cells, the tree's own
/// glyphs of that width take §10's stand-ins at the source — `▼` `v`, `→` `>`, the hidden row's
/// `·` `.`, the mark's `·` and `•` `.` — where the printer could not tell them from a name's
/// letters; `►` and `∙` are one cell everywhere and stay; a name's own `·` and `→` are content and
/// stay. In a window, the glyphs as they are.
#[test]
fn in_an_ambiguous_wide_terminal_the_trees_glyphs_take_their_stand_ins() {
    let mut rows = sample();
    rows[4].name = "a·b→c".into();
    let mut fx = Fx::new(1000.0, TreeView::new("box:/srv/data", rows.clone(), 0));
    let shown = |fx: &Fx, name: &str| {
        let (col, row) = fx.find(name).unwrap_or_else(|| panic!("{name}"));
        fx.cell(col - 2, row).symbol
    };
    assert_eq!(shown(&fx, "~"), "▼");
    assert_eq!(shown(&fx, "Desktop"), "→");
    assert!(fx.find("· 2 hidden folders").is_some());
    cells::set_ambiguous_wide(&fx.drv.ctx, true);
    fx.idle();
    assert_eq!(shown(&fx, "~"), "v");
    assert_eq!(shown(&fx, "src"), "v");
    assert_eq!(shown(&fx, "Desktop"), ">");
    assert_eq!(shown(&fx, "proc"), "►", "one cell everywhere");
    assert_eq!(shown(&fx, "a·b→c"), "►", "a name is content");
    assert!(
        fx.find(". 2 hidden folders").is_some(),
        "the hidden row's dot"
    );
    assert!(fx.find("· 2 hidden folders").is_none());
    // The mark, over a whole turn: `.` for `·` and `•`, `∙` as it is.
    let (_, src) = fx.find("src").unwrap();
    let mut marks = std::collections::BTreeSet::new();
    for _ in 0..60 {
        fx.idle();
        marks.insert(fx.cell(fx.inner().width - 1, src).symbol);
    }
    let want: std::collections::BTreeSet<String> = [".", "∙"].map(String::from).into();
    assert_eq!(marks, want);
    // Every cell of the tree is one cell wide in such a terminal: no glyph of width A of its own.
    for c in fx.rows().iter().flatten() {
        let mut chars = c.symbol.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            let named = "a·b→c".contains(ch);
            assert!(named || !crate::gui::term::ambiguous(ch), "{ch:?}");
        }
    }
}

mod model_rows {
    //! A model's rows drawn: the tree of a temp home through [`crate::tree::Tree`], its mount
    //! table a fake one (05 `T-norules`), each row as the browser maps it (`view_row`).

    use super::*;
    use crate::remote::treefs::{Budget, Disk, FsStat, Listing, Live, Mount, Mounts, Resolved};
    use crate::remote::LocalFs;
    use crate::tree::watchdog::{Pool, Timing};
    use std::time::{Duration, Instant};

    /// The real disk, with the home and the mount table the test says.
    struct Mounted {
        real: Arc<dyn Disk>,
        home: String,
        table: Vec<Mount>,
    }

    impl Disk for Mounted {
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
            Ok(Mounts {
                table: Some(self.table.clone()),
                macos: false,
                statvfs: true,
            })
        }

        fn statvfs(&self, path: &str) -> std::io::Result<FsStat> {
            self.real.statvfs(path)
        }
    }

    /// 05 §2, D-4 and `T-norules`: a `<SYS>` row stands only under `/` on a real machine, so a
    /// fake mount table puts a `proc` filesystem on a folder of a temp home. Its row reads
    /// `<SYS>` in the size's column, its glyph `►`, no counts; beside it, as the model gives
    /// them, a Hangul folder that opened by itself and its two rows, the hidden folders' row, a
    /// compact chain and a link, every size counted.
    #[test]
    fn a_sys_row_from_a_fake_mount_table_and_the_other_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(tmp.path()).unwrap();
        for d in [
            "proc/self",
            "문서/사진",
            "문서/음악",
            "a/b/c",
            ".git/objects",
            ".cache",
            "big/1",
            "big/2",
            "big/3",
            "big/4",
            "big/5",
        ] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        for (f, n) in [
            ("a/b/c/x.txt", 30),
            ("문서/사진/p.jpg", 50),
            ("big/3/z", 7000),
        ] {
            std::fs::write(home.join(f), vec![b'x'; n]).unwrap();
        }
        std::os::unix::fs::symlink(home.join("문서"), home.join("link")).unwrap();
        let home = home.to_str().unwrap().to_string();
        let disk: Arc<dyn Disk> = Arc::new(Mounted {
            real: LocalFs::disk(),
            home: home.clone(),
            table: vec![Mount {
                at: format!("{home}/proc"),
                fstype: "proc".into(),
                nobrowse: false,
            }],
        });
        let pool = Pool::new(Timing::default());
        let mut model = crate::tree::Tree::open(
            LocalFs::tree_with(disk, pool.clone()),
            crate::tree::Open {
                host: "x4-tree-sys".into(),
                folder: home.clone(),
                user: None,
                params: Default::default(),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut told = false;
        loop {
            model.poll();
            if model.ready() && !told {
                model.view(0, 30, None);
                told = true;
            }
            let rows = model.rows();
            let counting = rows.iter().any(|r| {
                matches!(r.info.size, crate::tree::Size::Bytes { scanning: true, .. })
                    || r.kind == crate::tree::RowKind::Folder
                        && r.info.dirs.is_none()
                        && !matches!(r.info.size, crate::tree::Size::Tag(_))
            });
            if told && model.built() && !counting {
                break;
            }
            assert!(Instant::now() < deadline, "the tree settles");
            std::thread::sleep(Duration::from_millis(5));
        }
        let rows: Vec<TreeRow> = model.rows().iter().map(|r| view_row(r, None)).collect();
        let fx = Fx::new(1000.0, TreeView::new(model.title().unwrap(), rows, 0));
        let chars = fx.inner().width;
        let (col, row) = fx.find("proc").expect("the proc row");
        assert_eq!(fx.cell(col - 2, row).symbol, "►");
        let shown = fx.pieces(row, col + 4, chars - 1);
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!((shown[0].1, shown[0].2.as_str()), (chars - 2, "<SYS>"));
        assert_eq!(fx.fg_of("<SYS>"), [LOOK.tag]);
        // The other kinds.
        let (col, row) = fx.find("link").unwrap();
        assert_eq!(fx.cell(col - 2, row).symbol, "→");
        assert!(fx.pieces(row, col + 4, chars - 1)[0].2 == "<LINK>");
        let (col, _) = fx.find("문서").unwrap();
        assert_eq!(col, 4);
        assert_eq!(fx.find("사진").unwrap().0, 6, "opened by itself");
        assert!(fx.find("음악").is_some());
        assert!(fx.find("a/b/c").is_some(), "a compact chain");
        assert!(fx.find("· 2 hidden folders").is_some());
        // Every size counted: `big` (five subfolders, closed) scanned to its end.
        let (col, row) = fx.find("big").unwrap();
        let info = fx.pieces(row, col + 3, chars - 1);
        assert_eq!(info.last().unwrap().2, "7.0K", "{info:?}");
        model.close();
        pool.shutdown();
    }
}

// ---------------------------------------------------------------------------------------------
// TS-4 (10 §14): the tree in both backends; the receipt's wait for final sizes (X4-tree).

mod ts4 {
    use super::super::super::{parse_keys, Browser, Start, TreeConnect, TreeModel};
    use super::*;
    use crate::gui::term::{self, Backend, End, Probed, Receipt, Tty};
    use crate::remote::treefs::{Budget, Disk, FsStat, Listing, Live, Mounts, Resolved};
    use crate::remote::{LocalFs, RemoteFs};
    use crate::tree::watchdog::{Pool, Timing};
    use std::collections::HashSet;
    use std::io;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

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

    /// The terminal's run of `win` with `SOPEN_CELLS` on a `cols` × `rows` terminal, its passes'
    /// clock starting at `at` seconds, its receipt given `limit` to settle: what it wrote, or the
    /// run's error — and the time of its last pass.
    fn terminal_run<W: Window>(
        win: &mut W,
        cols: u16,
        rows: u16,
        at: f64,
        limit: Duration,
    ) -> (io::Result<String>, f64) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cells.txt");
        let ctx = egui::Context::default();
        chrome::install(&ctx);
        let mut b = Backend::new(Still((cols, rows)), io::sink(), ctx.clone(), true);
        b.offset = at;
        b.receipt = Some(Receipt::new(path.clone(), limit));
        let mut no_switch = |_: &mut Backend<Still, io::Sink>| -> io::Result<Option<String>> {
            panic!("no switch in this test")
        };
        let ran = b.run(win, &mut no_switch);
        let when = ctx.input(|i| i.time);
        let written = ran.and_then(|end| {
            assert_eq!(end, End::Closed);
            std::fs::read_to_string(&path)
        });
        (written, when)
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

    /// TS-4 with a row being scanned: the terminal's receipt of the tree and the window's at the
    /// same moment are the same bytes, its mark among them; at a later moment both show the mark's
    /// next glyph — the one cell that differs between two moments, the mark's.
    #[test]
    fn ts4_a_row_being_scanned_is_the_same_cells_in_both_backends_its_mark_moving() {
        let (cols, rows) = (100, 30);
        let mut seen: Vec<String> = vec![];
        let mut at = 1_000.1;
        for _ in 0..8 {
            let mut host = Host::new(TreeView::new("box:/srv/data", sample(), 0));
            let (terminal, when) = terminal_run(&mut host, cols, rows, at, Duration::from_secs(30));
            let terminal = terminal.expect("the receipt");
            let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
            let mut win = Host::new(TreeView::new("box:/srv/data", sample(), 0));
            drv.idle(&mut win, 2);
            let in_window = window_receipt(&drv, &mut win, cols, rows, when);
            assert_eq!(terminal, in_window, "the same bytes at {when}");
            let step = (when / chrome::tree::SCAN_STEP).floor() as usize;
            let mark = SCAN_MARKS[step % SCAN_MARKS.len()];
            assert!(
                terminal.contains(&format!("9.8M+ {mark}")),
                "{mark:?} at {when}"
            );
            if !seen.contains(&terminal) {
                seen.push(terminal);
            }
            if seen.len() > 1 {
                break;
            }
            at = when + chrome::tree::SCAN_STEP;
        }
        assert!(seen.len() > 1, "the mark moved");
        let differ: Vec<(usize, usize)> = (seen[0].lines().zip(seen[1].lines()).enumerate())
            .flat_map(|(y, (a, b))| {
                (a.chars().zip(b.chars()).enumerate())
                    .filter(|(_, (p, q))| p != q)
                    .map(move |(x, _)| (x, y))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(differ.len(), 1, "one cell: {differ:?}");
    }

    /// The real disk with the home at `home`; a row's listing of a `held` path, and every scan's
    /// listing while `scans` is set, wait — beating, so the watchdog never writes them off —
    /// until released.
    struct HoldDisk {
        real: Arc<dyn Disk>,
        home: String,
        held: Mutex<(HashSet<String>, bool)>,
        moved: Condvar,
    }

    impl HoldDisk {
        fn new(home: &str, paths: &[String], scans: bool) -> Arc<HoldDisk> {
            Arc::new(HoldDisk {
                real: LocalFs::disk(),
                home: home.to_string(),
                held: Mutex::new((paths.iter().cloned().collect(), scans)),
                moved: Condvar::new(),
            })
        }

        fn release(&self) {
            *self.held.lock().unwrap() = (HashSet::new(), false);
            self.moved.notify_all();
        }
    }

    impl Disk for HoldDisk {
        fn list(
            &self,
            path: &str,
            budget: Option<Budget>,
            full: bool,
            guard: &[String],
            live: &Live,
        ) -> std::io::Result<Listing> {
            let mut held = self.held.lock().unwrap();
            while held.0.contains(path) && full || held.1 && !full {
                if (live.stop)() {
                    return Err(std::io::Error::other("stopped"));
                }
                (live.beat)();
                held = self
                    .moved
                    .wait_timeout(held, Duration::from_millis(50))
                    .unwrap()
                    .0;
            }
            drop(held);
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

    /// A temp home: `notes.txt` (the bar goes on it: the other side previews it, no free space
    /// that moves between two runs), `문서` with `사진` and `음악` (it opens by itself), `프로젝트`
    /// with five subfolders (it stays closed and is scanned), the compact chain `a/b/c`, two
    /// hidden folders and a link to `문서`.
    struct Home {
        _tmp: tempfile::TempDir,
        root: String,
        cache: std::path::PathBuf,
    }

    fn home() -> Home {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap().join("home");
        for d in [
            "문서/사진",
            "문서/음악",
            "프로젝트/a",
            "프로젝트/b",
            "프로젝트/c",
            "프로젝트/d",
            "프로젝트/e",
            "a/b/c",
            ".config/x",
            ".cache",
        ] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for (f, n) in [
            ("notes.txt", 12),
            ("a/b/c/x.txt", 30),
            ("문서/사진/p.jpg", 50),
            ("프로젝트/c/z.bin", 7000),
        ] {
            std::fs::write(root.join(f), vec![b'x'; n]).unwrap();
        }
        std::os::unix::fs::symlink(root.join("문서"), root.join("link")).unwrap();
        let cache = tmp.path().join("cache");
        let root = root.to_str().unwrap().to_string();
        Home {
            _tmp: tmp,
            root,
            cache,
        }
    }

    /// The pool a tree's reads run on, shut down with the test.
    struct TreePool(Pool);

    impl Drop for TreePool {
        fn drop(&mut self) {
            self.0.shutdown();
        }
    }

    /// A browser on `h`, its tree on `disk` under the host name `host` (the per-process cache
    /// keeps a home and the sizes per host: a test's own), the bar going on `notes.txt`;
    /// `whole`: a `SOPEN_CELLS` run's; `keys`: `SOPEN_KEYS`.
    fn browser(
        h: &Home,
        disk: Arc<dyn Disk>,
        host: &str,
        whole: bool,
        keys: &str,
    ) -> (Browser, TreePool) {
        let mut b = Browser::new(Start {
            fs: Box::new(LocalFs::new()),
            connect: Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>)),
            label: None,
            destination: None,
            abs: h.root.clone(),
            cache: h.cache.clone(),
            state_dir: h.cache.with_file_name("state"),
            launch: Arc::new(|abs: &str, _| Ok(format!("opened {abs}"))),
            git: None,
        });
        let pool = Pool::new(Timing::default());
        let p = pool.clone();
        let connect: TreeConnect =
            Arc::new(move |_: &AtomicBool| Ok((LocalFs::tree_with(disk.clone(), p.clone()), None)));
        b.set_tree_connect(connect, host);
        b.whole_sizes = whole;
        b.bar_first = Some("notes.txt".into());
        b.script = parse_keys(keys).unwrap().into();
        (b, TreePool(pool))
    }

    fn real(h: &Home) -> Arc<dyn Disk> {
        HoldDisk::new(&h.root, &[], false)
    }

    /// Whether a row of `b`'s tree is being scanned now.
    fn scanning(b: &Browser) -> bool {
        let Some(t) = &b.tree else { return false };
        let TreeModel::Open(model) = &t.model else {
            return false;
        };
        (model.rows().iter())
            .any(|r| matches!(r.info.size, crate::tree::Size::Bytes { scanning: true, .. }))
    }

    /// The model of `b`'s tree, open.
    fn model(b: &mut Browser) -> &mut crate::tree::Tree {
        match b.tree.as_mut().map(|t| &mut t.model) {
            Some(TreeModel::Open(m)) => m,
            _ => panic!("the tree is open"),
        }
    }

    /// TS-4 (X4-tree's receipt): with the home a fixture, F10 in the window and in the terminal —
    /// both `SOPEN_CELLS` runs — writes the same bytes: the tree in cells where a hand-off box
    /// stood, its Hangul names, the folder that opened by itself, the hidden folders' row, the
    /// compact chain and the link, every size counted (the closed `프로젝트` exact, no mark), the
    /// other side the preview of `notes.txt`; and the title row blank from the mode button's place
    /// to its end in both — the terminal's `[GUI]` and the clock.
    #[test]
    fn ts4_the_tree_in_the_window_and_in_the_terminal_is_the_same_bytes() {
        let h = home();
        let (cols, rows) = (140, 30);
        let mut drv = Driver::new(f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
        let (mut win, _p1) = browser(&h, real(&h), "x4-tree-ts4", true, "F10");
        drv.until(&mut win, 30, |b| b.ready());
        drv.idle(&mut win, 2);
        let in_window = window_receipt(&drv, &mut win, cols, rows, 30_000.0);
        // The same host: a size the window's scan left in the process cache (`~`) is scanned
        // again before this run settles.
        let (mut b, _p2) = browser(&h, real(&h), "x4-tree-ts4", true, "F10");
        let (terminal, _) = terminal_run(&mut b, cols, rows, 40_000.0, Duration::from_secs(30));
        let terminal = terminal.expect("the terminal's receipt");
        assert!(!terminal.contains(term::HAND_OFF), "{terminal}");
        for shown in [
            "▼ ~",
            "▼ 문서",
            "사진",
            "음악",
            "► 프로젝트",
            "a/b/c",
            "→ link",
            "<LINK>",
            "· 2 hidden folders",
            "notes.txt",
        ] {
            assert!(terminal.contains(shown), "{shown:?} in:\n{terminal}");
        }
        let project = (terminal.lines())
            .find(|l| l.contains("프로젝트"))
            .expect("its row");
        assert!(
            project.contains("5 d") && project.contains("7.0K "),
            "{project}"
        );
        for mark in SCAN_MARKS {
            assert!(!project.contains(&format!("7.0K {mark}")), "{project}");
        }
        let title = terminal.lines().nth(1).unwrap_or_default();
        assert!(!title.contains("[GUI]") && !title.contains("am") && !title.contains("pm"));
        assert!(title.ends_with("      "), "blank to its end: {title:?}");
        assert_eq!(terminal, in_window, "the same bytes");
    }

    /// The receipt's title row (X4-tree): `chrome::mode_clock_rect` runs from the mode button's
    /// place to the row's end — `[GUI]`'s cells where the terminal sets the button, the same cells
    /// left blank in a window started directly — so a receipt leaves the clock out with the
    /// button; the button's own rect (its click) stays the label's alone.
    #[test]
    fn the_receipt_leaves_the_clock_out_with_the_mode_button() {
        let h = home();
        let (mut b, _p) = browser(&h, real(&h), "x4-tree-clock", false, "");
        let mut drv = Driver::new(1000.0, 700.0);
        drv.until(&mut b, 30, |b| b.ready());
        let blank = chrome::mode_clock_rect(&drv.ctx).expect("the title row's");
        assert!(
            chrome::mode_button_rect(&drv.ctx).is_none(),
            "no button here"
        );
        let cols = (1000.0 / CELL.x).floor();
        assert_eq!(blank.right(), cols * CELL.x, "to the row's end");
        assert_eq!(
            blank.width(),
            CELL.x * 15.0,
            "the button's 5, a blank, `│`, the clock's 7 and the row's last blank"
        );
        chrome::set_mode_button(&drv.ctx, "[GUI]");
        drv.idle(&mut b, 1);
        let button = chrome::mode_button_rect(&drv.ctx).expect("drawn");
        let blank = chrome::mode_clock_rect(&drv.ctx).unwrap();
        assert_eq!(
            (blank.left(), blank.right()),
            (button.left(), cols * CELL.x)
        );
        assert_eq!(button.width(), 5.0 * CELL.x);
    }

    /// X4-tree: in a `SOPEN_CELLS` run the tree settles only once no row is being scanned — the
    /// receipt holds final sizes and no moving mark — where any other run settles as today, with
    /// a scan still running.
    #[test]
    fn a_cells_run_waits_for_the_scans_another_run_does_not() {
        let h = home();
        let disk = HoldDisk::new(&h.root, &[], true);
        let (mut b, _p) = browser(&h, disk.clone(), "x4-tree-scans", false, "F10");
        let mut drv = Driver::new(1000.0, 700.0);
        drv.until(&mut b, 30, |b| b.ready() && scanning(b));
        assert!(b.tree.is_some(), "F10 opened the tree");
        b.whole_sizes = true;
        for _ in 0..20 {
            drv.idle(&mut b, 1);
            std::thread::sleep(Duration::from_millis(10));
            assert!(!b.ready(), "a row is being scanned");
        }
        disk.release();
        drv.until(&mut b, 30, |b| b.ready());
        assert!(!scanning(&b));
    }

    /// X4-tree: "no row is scanning" alone holds while a row on screen waits for its listing —
    /// phase 3 starts a scan only once its row is listed (`tree/scan.rs` `scannable`) — so a
    /// `SOPEN_CELLS` run also waits for every row on screen to have its counts or a tag. Here
    /// `프로젝트` is opened once the tree has settled (as `SOPEN_KEYS`' `F10,…,Right` would), and
    /// the listing of its `c` is held: nothing scans, the tree is built, and only that row waits.
    #[test]
    fn a_cells_run_waits_for_a_row_still_to_be_listed() {
        let h = home();
        let held = format!("{}/프로젝트/c", h.root);
        let disk = HoldDisk::new(&h.root, std::slice::from_ref(&held), false);
        let (mut b, _p) = browser(&h, disk.clone(), "x4-tree-unread", true, "F10");
        let mut drv = Driver::new(1000.0, 700.0);
        drv.until(&mut b, 30, |b| b.ready());
        let project = (model(&mut b).rows().into_iter())
            .find(|r| r.name == "프로젝트")
            .expect("its row")
            .id;
        assert!(model(&mut b).expand(project));
        // Until its five rows are drawn and all but `c` listed.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            drv.idle(&mut b, 1);
            let rows = model(&mut b).rows();
            let listed = |name: &str| {
                (rows.iter()).any(|r| r.depth == 2 && r.name == name && r.info.dirs.is_some())
            };
            if ["a", "b", "d", "e"].iter().all(|n| listed(n)) && !scanning(&b) {
                break;
            }
            assert!(Instant::now() < deadline, "its rows are listed");
            std::thread::sleep(Duration::from_millis(5));
        }
        let t = b.tree.as_ref().unwrap();
        assert!(
            t.view.rows.iter().any(|r| r.name == "c" && r.depth == 2),
            "c is drawn"
        );
        assert!(t.settled(false), "settled as any other run would be");
        for _ in 0..10 {
            drv.idle(&mut b, 1);
            std::thread::sleep(Duration::from_millis(10));
            assert!(!b.ready(), "c is still to be listed");
        }
        disk.release();
        drv.until(&mut b, 30, |b| b.ready());
    }

    /// X4-tree, AGENTS.md: a `SOPEN_CELLS` run whose tree never settles — a scan that never
    /// ends — ends at its watchdog: an error, nothing written, never a hang.
    #[test]
    fn a_cells_run_whose_tree_never_settles_ends_at_its_watchdog() {
        let h = home();
        let disk = HoldDisk::new(&h.root, &[], true);
        let (mut b, _p) = browser(&h, disk.clone(), "x4-tree-stall", true, "F10");
        let at = Instant::now();
        let (ran, _) = terminal_run(&mut b, 100, 30, 1_000.0, Duration::from_millis(1500));
        let e = ran.expect_err("the run ends at its watchdog");
        assert!(e.to_string().contains("stalled"), "{e}");
        assert!(at.elapsed() < Duration::from_secs(20), "{:?}", at.elapsed());
        assert!(b.tree.is_some() && scanning(&b), "it stalled on the scan");
        disk.release();
    }
}

/// The tree's release frame cost at 1000×700 — 300 rows below the root, each with its info line,
/// Hangul names, a row in seven being scanned — run alone: `cargo test --release --lib --
/// --ignored --exact gui::browser::tree::tests::frame_cost_at_1000_by_700 --nocapture`. Printed,
/// no budget set.
#[test]
#[ignore]
fn frame_cost_at_1000_by_700() {
    use std::time::Instant;
    let root = counted(300, 12, bytes(52_000_000, true));
    let mut rows = vec![row(0, 0, "~", RowKind::Folder, Fold::Open, root)];
    rows.extend((0..300usize).map(|k| {
        let fold = if k % 3 == 2 { Fold::Leaf } else { Fold::Closed };
        let size = bytes((k * 12_345) as u64, k % 2 == 0);
        let info = counted((k % 20) as u64, (k * 3) as u64, size);
        let name = format!("폴더-{k:03}-folder");
        let mut r = row(k + 1, 1 + k % 3, &name, RowKind::Folder, fold, info);
        r.info.scanning = k % 7 == 0;
        r
    }));
    let mut fx = Fx::new(1000.0, TreeView::new("box:/srv/data", rows, 150));
    fx.drv.idle(&mut fx.host, 20);
    let n = 400;
    let started = Instant::now();
    for _ in 0..n {
        fx.drv.frame(&mut fx.host, Vec::new(), Modifiers::NONE);
    }
    println!("the tree's frame at 1000x700: {:?}", started.elapsed() / n);
}
