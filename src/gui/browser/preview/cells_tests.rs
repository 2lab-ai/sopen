//! The preview side in cells (10 §17, X3-preview): its frame titled with the file and its lines
//! — the read's progress, `…`, why there is none — are cells, wrapped by cells; a text or hex
//! preview paints its own cells inside the frame's, so a terminal shows no hand-off box there (a
//! page file's preview there is its source, VM-8); an image's or a sound's side stays egui's
//! whole, one box in a terminal, from its first pass and across a refresh; and the window paints
//! the cells a terminal shows (TS-1).

use super::*;
use crate::gui::cells::{self, Cells, Grid, Layer};
use crate::gui::testkit::Driver;
use crate::remote::LocalFs;
use eframe::egui::epaint::ClippedShape;
use eframe::egui::{Color32, Pos2, Rect};

/// The host at the cells `side` gives of the window's grid, as the browser draws a side: told
/// the file under the bar each pass, then drawn.
struct Pane {
    host: Host,
    want: Want,
    side: fn(&Grid) -> Cells,
    /// What a dialog of the list's stands over.
    under: Vec<Cells>,
}

impl Window for Pane {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.host
            .sync(&ctx, Some(self.want.clone()), Some(&self.want.path));
        let grid = Grid::over(ui.max_rect());
        let side = (self.side)(&grid);
        self.host.ui(ui, &grid, side, &self.under);
    }

    /// The file's preview is on screen (before the first pass the host has no file to show).
    fn ready(&self) -> bool {
        self.host.display.is_some() && self.host.settled() && !self.host.busy()
    }
}

/// The whole grid.
fn all(g: &Grid) -> Cells {
    g.all()
}

/// The right half, as the browser's preview mode puts the side opposite the left list.
fn right(g: &Grid) -> Cells {
    cells::halves(g.all())[1]
}

/// 30 cells wide, 12 tall, from (4, 2): a narrow side.
fn narrow(_: &Grid) -> Cells {
    Cells::new(4, 2, 30, 12)
}

/// 24 cells wide, 10 tall, from (2, 1).
fn slim(_: &Grid) -> Cells {
    Cells::new(2, 1, 24, 10)
}

fn local() -> Connect {
    Arc::new(|| Ok(Box::new(LocalFs::new()) as Box<dyn RemoteFs + Send>))
}

/// A folder with `files` (name, bytes).
fn folder(files: &[(&str, Vec<u8>)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (file, bytes) in files {
        std::fs::write(tmp.path().join(file), bytes).unwrap();
    }
    tmp
}

/// The pane on the file named `name` in `tmp`, at `side`.
fn pane_in(tmp: &tempfile::TempDir, name: &str, side: fn(&Grid) -> Cells) -> Pane {
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let host = Host::new(local(), None, tmp.path().join("cache"));
    let want = Want {
        path: root.join(name).to_str().unwrap().to_string(),
        name: name.to_string(),
    };
    Pane {
        host,
        want,
        side,
        under: vec![],
    }
}

/// A folder with `files` and the pane on the one named `name`, at `side`.
fn pane_on(
    files: &[(&str, Vec<u8>)],
    name: &str,
    side: fn(&Grid) -> Cells,
) -> (tempfile::TempDir, Pane) {
    let tmp = folder(files);
    let pane = pane_in(&tmp, name, side);
    (tmp, pane)
}

/// The pane on a file nobody reads: the host's state is the test's own.
fn pane_at(path: &str, side: fn(&Grid) -> Cells) -> Pane {
    let connect: Connect = Arc::new(|| anyhow::bail!("no connection in this test"));
    let want = Want {
        path: path.to_string(),
        name: path.rsplit('/').next().unwrap().to_string(),
    };
    let mut host = Host::new(connect, None, PathBuf::from("/nonexistent"));
    host.display = Some(want.clone());
    Pane {
        host,
        want,
        side,
        under: vec![],
    }
}

fn hangul_rows() -> Vec<u8> {
    (0..40)
        .map(|i| format!("{i:02} 한글 줄 text 안녕하세요\n"))
        .collect::<String>()
        .into_bytes()
}

/// The layer whose cells are exactly `side`: the side's own.
fn layer_at(drv: &Driver, side: Cells) -> Option<Arc<Layer>> {
    cells::layers(&drv.ctx)
        .into_iter()
        .find(|l| l.buf.area == side)
}

/// Row `y` of `l` as text, each cell's symbol (a wide glyph's second cell adds nothing).
fn row(l: &Layer, y: u16) -> String {
    cells::row_text(&l.buf, y).0
}

/// What the cells of `inner`'s rows of `l` say, row by row, blanks at the end cut.
fn said(l: &Layer, inner: Cells) -> Vec<String> {
    (inner.top()..inner.bottom())
        .map(|y| {
            let (text, at) = cells::row_text(&l.buf, y);
            let chars: String = (text.chars().zip(at))
                .filter(|&(_, x)| x >= inner.left() && x < inner.right())
                .map(|(c, _)| c)
                .collect();
            chars.trim_end().to_string()
        })
        .collect()
}

/// The cells inside a frame at `side`.
fn inside(side: Cells) -> Cells {
    Cells::new(side.x + 1, side.y + 1, side.width - 2, side.height - 2)
}

/// The tests' window: 800 × 480 points, 100 × 30 cells.
const SIZE: Vec2 = Vec2::new(800.0, 480.0);

/// The tests' window's grid.
fn grid() -> Grid {
    Grid::over(Rect::from_min_size(Pos2::ZERO, SIZE))
}

/// The side's frame is a layer of cells over all of the side — single-lined, the path in its
/// top border cut from the left — and the text preview's own cells, its frame and rows, are that
/// frame's inner cells.
#[test]
fn the_sides_frame_and_title_are_cells_and_the_preview_paints_inside_them() {
    // A name longer than the title's 44 cells, so the path is cut wherever the host keeps its
    // temporary folders.
    let name = "a-preview-named-longer-than-its-sides-title-has-room-for.txt";
    let files = [(name, b"plain notes\nsecond row\n".to_vec())];
    let (_tmp, mut pane) = pane_on(&files, name, right);
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    drv.until(&mut pane, 20, |p| p.ready());
    drv.idle(&mut pane, 2);
    let side = right(&grid());
    let frame = layer_at(&drv, side).expect("the side's frame is a layer of cells");
    let (l, r, t, b) = (side.left(), side.right() - 1, side.top(), side.bottom() - 1);
    for (x, y, corner) in [(l, t, "┌"), (r, t, "┐"), (l, b, "└"), (r, b, "┘")] {
        assert_eq!(
            frame.buf[(x, y)].symbol(),
            corner,
            "single-lined: ({x},{y})"
        );
    }
    let top = row(&frame, t);
    assert!(
        top.starts_with("┌─ "),
        "the title in the top border: {top:?}"
    );
    assert!(
        top.ends_with("-has-room-for.txt ─┐"),
        "the file's path, its end kept: {top:?}"
    );
    assert!(top.starts_with("┌─ …"), "cut from the left: {top:?}");
    // The preview's own cells — its frame and rows — are the frame's inner cells.
    let layers = cells::layers(&drv.ctx);
    let viewer = cells::find(&layers, "Text Viewer");
    let viewer = &layers[viewer.first().expect("the text preview in cells").layer];
    assert_eq!(
        viewer.rect(viewer.buf.area),
        frame.rect(inside(side)),
        "inside the side's frame, on its inner cells"
    );
    assert!(
        drv.find_text("plain notes").is_some(),
        "the previewed text, in cells"
    );
}

/// A preparation of the pane's file, running — `done` of `total` bytes read — until the sender
/// it returns is dropped.
fn reading(pane: &mut Pane, done: u64, total: u64) -> mpsc::Sender<Result<Previewed>> {
    let (tx, rx) = mpsc::channel();
    pane.host.prep = Some(Prep {
        want: pane.want.clone(),
        cancel: Arc::new(AtomicBool::new(false)),
        budget: Budget::preview(),
        done: Arc::new(AtomicU64::new(done)),
        total: Arc::new(AtomicU64::new(total)),
        pages: Pages::Source(None),
        rx,
        worker: None,
    });
    tx
}

/// While its file is read, the side says how far, in cells at the top of its frame; once the
/// preparation is cancelled it is frozen with what it said, then `…`.
#[test]
fn the_progress_line_is_cells_with_its_percentage_then_its_ellipsis() {
    let mut pane = pane_at("/d/big.bin", right);
    let (done, total) = (1 << 20, 4 << 20);
    let _tx = reading(&mut pane, done, total);
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    drv.idle(&mut pane, 2);
    let side = right(&grid());
    let read = format!(
        "reading big.bin 25% · {} / {}",
        crate::human(done),
        crate::human(total)
    );
    let frame = layer_at(&drv, side).expect("the side in cells");
    let rows = said(&frame, inside(side));
    assert_eq!(
        rows[0],
        format!("{read}…"),
        "at the top of the frame, in its cells"
    );
    assert!(rows[1..].iter().all(String::is_empty), "{rows:?}");
    // Cancelled (the bar left): the side is frozen with what it said, then `…`.
    pane.host
        .prep
        .as_ref()
        .unwrap()
        .cancel
        .store(true, Ordering::Relaxed);
    drv.idle(&mut pane, 2);
    let frame = layer_at(&drv, side).expect("the side in cells");
    assert_eq!(said(&frame, inside(side))[0], format!("{read} …"));
}

/// Why there is no preview is said in cells, wrapped by cells at the frame's inner width — never
/// by the font's advance, which egui's layout measured (a syllable 13.84 points of a Mac's 16).
#[test]
fn a_failure_line_is_cells_wrapped_by_cells() {
    let mut pane = pane_at("/d/a.txt", slim);
    // 22 cells inside the frame. The first word, twelve syllables, is 24 cells: cut after its
    // eleventh.
    let why =
        "가나다라마바사아자차카타 한글 파일 이름 테스트 문장입니다 and abcdefghijklmnopqrstuvwxyz";
    pane.host.shown = Some(Shown::Failed(why.to_string()));
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    drv.idle(&mut pane, 2);
    let side = slim(&grid());
    let frame = layer_at(&drv, side).expect("the side in cells");
    let rows = said(&frame, inside(side));
    assert_eq!(
        rows,
        [
            "가나다라마바사아자차카",
            "타 한글 파일 이름",
            "테스트 문장입니다 and",
            "abcdefghijklmnopqrstuv",
            "wxyz",
            "",
            "",
            "",
        ],
        "wrapped at the frame's 22 cells: at a blank, a word longer than a row after its last \
         whole glyph"
    );
    // A syllable's second cell shows nothing: two cells a glyph, from the frame's first inner
    // cell.
    let first = &frame.seen()[1];
    assert_eq!(
        [first[1].symbol.as_str(), first[2].symbol.as_str()],
        ["가", ""]
    );
}

/// A Hangul name in the side's title takes two cells a syllable, and a path too long for the
/// border is cut from the left, as the list's title is, never half a syllable.
#[test]
fn a_hangul_name_in_the_title_takes_two_cells_a_syllable_and_is_cut_from_the_left() {
    let mut pane = pane_at("/data/folder/한글 문서 초안.txt", narrow);
    pane.host.shown = Some(Shown::Failed("no preview".to_string()));
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    drv.idle(&mut pane, 2);
    let side = narrow(&grid());
    let frame = layer_at(&drv, side).expect("the side in cells");
    // 30 cells: the title gets 24 — `…` and the path's last 23 cells, never half a syllable —
    // centred, a blank either side.
    assert_eq!(row(&frame, side.y), "┌─ …lder/한글 문서 초안.txt ─┐");
    let top = &frame.seen()[0];
    let x = |cell: u16| usize::from(cell);
    assert_eq!(top[x(3)].symbol, "…", "the cut, where the title starts");
    for (cell, syllable) in [(9, "한"), (11, "글"), (14, "문"), (16, "서")] {
        assert_eq!(top[x(cell)].symbol, syllable);
        assert_eq!(top[x(cell) + 1].symbol, "", "{syllable}: its second cell");
    }
    assert_eq!(
        [top[x(26)].symbol.as_str(), top[x(27)].symbol.as_str()],
        ["t", " "],
        "24 cells: the path's end, then the blank before the border"
    );
}

/// What a dialog of the list's and the key help stand over is blanked in the side's cells first
/// (cells.rs: one buffer over another): a syllable of the title half under a dialog's edge goes
/// whole, as a terminal erases it.
#[test]
fn the_cells_under_a_dialog_and_the_key_help_are_blanked_first() {
    let mut pane = pane_at("/data/folder/한글 문서 초안.txt", narrow);
    pane.host.shown = Some(Shown::Failed("no preview".to_string()));
    // The title's `한` takes cells 13–14 of the top border (row 2) and `글 문` 15–19; the line's
    // `w` is at cell 14 of row 3. A dialog over cells 14–19 covers half the syllable — it goes
    // whole — the title's cells after it, and the line's last letter.
    pane.under = vec![Cells::new(14, 2, 6, 3)];
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    drv.idle(&mut pane, 2);
    let side = narrow(&grid());
    let frame = layer_at(&drv, side).expect("the side in cells");
    assert_eq!(row(&frame, 2), "┌─ …lder/       서 초안.txt ─┐");
    assert_eq!(
        said(&frame, inside(side))[0],
        "no previe",
        "the line under the dialog's edge"
    );
    // F1's help over the side: the cells it covers, box and shadow, are blanks of the side's.
    pane.under.clear();
    let page = crate::gui::help::for_context(
        super::super::keys::Context::List,
        super::super::view::Mode::Preview,
        &[],
    );
    crate::gui::help::open(&drv.ctx, page);
    drv.idle(&mut pane, 2);
    let covered = crate::gui::help::covered(&drv.ctx, &grid());
    assert!(!covered.is_empty(), "the help is open");
    let frame = layer_at(&drv, side).expect("the side in cells");
    let mut blanked = 0;
    for r in covered.iter().map(|r| r.intersection(side)) {
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                assert_eq!(frame.buf[(x, y)].symbol(), " ", "({x},{y}) under the help");
                blanked += 1;
            }
        }
    }
    assert!(blanked > 0, "the help stands over the side");
}

/// A PNG of `w`×`h`.
fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 128]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// The screen a terminal shows of the pass just run in `drv` (10 §4: its layers laid out,
/// where no layer is a hand-off box).
fn terminal_screen(drv: &Driver) -> crate::gui::term::Screen {
    let layers = cells::layers(&drv.ctx);
    crate::gui::term::compose(&drv.ctx, &layers, 100, 30)
}

/// In a terminal (its context marked so) a text preview, a hex one and a page file's — its
/// source there (VM-8) — are cells over all of the side, the ring around the preview too: no
/// hand-off box. An image's side is egui's whole: one box, never a box inside a frame of cells.
#[test]
fn in_a_terminal_a_text_or_hex_preview_and_a_pages_source_show_no_hand_off_box() {
    let files = [
        ("한글 노트.txt", hangul_rows()),
        ("data.bin", (0..=255u8).cycle().take(4096).collect()),
        ("index.html", b"<!doctype html><p>hello page</p>\n".to_vec()),
        ("photo.png", png(40, 30)),
    ];
    for name in ["한글 노트.txt", "data.bin", "index.html"] {
        let (_tmp, mut pane) = pane_on(&files, name, all);
        let mut drv = Driver::new(SIZE.x, SIZE.y);
        crate::gui::mark_terminal(&drv.ctx);
        drv.until(&mut pane, 20, |p| p.ready());
        drv.idle(&mut pane, 2);
        let screen = terminal_screen(&drv);
        assert_eq!(
            screen.hand_offs,
            [],
            "{name}: the side is cells, ring and all"
        );
        let top = cells::row_text(&screen.buf, 0).0;
        assert!(
            top.starts_with("┌─") && top.contains(&format!("{name} ─")) && top.ends_with("─┐"),
            "{name}: its frame, titled with the file: {top:?}"
        );
        let inner = cells::row_text(&screen.buf, 1).0;
        assert!(
            inner.starts_with("│┌") && inner.ends_with("┐│"),
            "{name}: the preview's own frame inside the side's: {inner:?}"
        );
    }
    // The negative control: an image's side is egui's, a hand-off box — the whole side, one box,
    // never a box inside a frame.
    let (_tmp, mut pane) = pane_on(&files, "photo.png", all);
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    crate::gui::mark_terminal(&drv.ctx);
    drv.until(&mut pane, 20, |p| p.ready());
    drv.idle(&mut pane, 2);
    let screen = terminal_screen(&drv);
    assert_eq!(screen.hand_offs, [grid().all()]);
    assert!(
        layer_at(&drv, grid().all()).is_none(),
        "no frame of cells under it"
    );
}

/// The terminal's screen of `drv`'s last pass: one hand-off box over all of the side, and no
/// layer of cells there — `what` says which.
fn one_box(drv: &Driver, what: &str) {
    let screen = terminal_screen(drv);
    assert_eq!(screen.hand_offs, [grid().all()], "{what}: one hand-off box");
    assert!(
        layer_at(drv, grid().all()).is_none(),
        "{what}: no frame of cells"
    );
}

/// An image's, a video's or a sound's side is egui's from its first pass (10 §17: its side is
/// the hand-off box until X6): while its file is read and when its preparation failed, the
/// frame and the line are egui's — one box in a terminal, never cells first and a box once it
/// is ready.
#[test]
fn an_images_or_medias_side_is_one_box_while_it_is_read_and_when_it_fails() {
    for name in ["photo.png", "clip.mp4", "song.wav"] {
        let mut pane = pane_at(&format!("/d/{name}"), all);
        let _tx = reading(&mut pane, 1 << 20, 4 << 20);
        let mut drv = Driver::new(SIZE.x, SIZE.y);
        crate::gui::mark_terminal(&drv.ctx);
        drv.idle(&mut pane, 2);
        one_box(&drv, name);
        let line = format!("reading {name} 25%");
        assert!(
            drv.text_starting_with(&line).is_some(),
            "{name}: its line, egui's"
        );
        pane.host.prep = None;
        pane.host.shown = Some(Shown::Failed("no preview: it failed".to_string()));
        drv.idle(&mut pane, 2);
        one_box(&drv, &format!("{name}, failed"));
    }
}

/// A picture ready, then its file changed (`refresh`): it is prepared again, and its side stays
/// egui's meanwhile — one box in a terminal, never the cells of a side being read; a text
/// file's side stays cells across the same.
#[test]
fn a_refresh_keeps_the_sides_frame_while_the_file_is_read_again() {
    let files = [
        ("photo.png", png(40, 30)),
        ("notes.txt", b"plain notes\n".to_vec()),
    ];
    let (_tmp, mut pane) = pane_on(&files, "photo.png", all);
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    crate::gui::mark_terminal(&drv.ctx);
    drv.until(&mut pane, 20, |p| p.ready());
    drv.idle(&mut pane, 2);
    one_box(&drv, "ready");
    let path = pane.want.path.clone();
    pane.host.refresh(&path);
    drv.idle(&mut pane, 1);
    assert!(pane.host.busy(), "prepared again");
    one_box(&drv, "read again");
    drv.until(&mut pane, 20, |p| p.ready());
    one_box(&drv, "ready again");
    // The text file: cells, ready, read again and ready again.
    let mut pane = pane_in(&_tmp, "notes.txt", all);
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    crate::gui::mark_terminal(&drv.ctx);
    drv.until(&mut pane, 20, |p| p.ready());
    drv.idle(&mut pane, 2);
    assert_eq!(terminal_screen(&drv).hand_offs, [], "ready: cells");
    let path = pane.want.path.clone();
    pane.host.refresh(&path);
    drv.idle(&mut pane, 1);
    assert!(pane.host.busy(), "prepared again");
    assert_eq!(terminal_screen(&drv).hand_offs, [], "read again: cells");
    let frame = layer_at(&drv, grid().all()).expect("the side in cells");
    assert!(
        said(&frame, inside(grid().all()))[0].starts_with("reading notes.txt"),
        "its line in cells"
    );
}

/// A file whose name says cells but whose ready preview is egui's — a missing `.txt`: its lstat
/// fails and the side shows the info card — keeps its box while it is prepared again after a
/// refresh: the frame of its last ready preview, not its name's.
#[test]
fn a_refresh_keeps_the_frame_of_the_ready_preview_not_the_names() {
    let tmp = folder(&[]);
    let mut pane = pane_in(&tmp, "missing.txt", all);
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    crate::gui::mark_terminal(&drv.ctx);
    drv.until(&mut pane, 20, |p| p.ready());
    drv.idle(&mut pane, 2);
    assert!(
        matches!(&pane.host.shown, Some(Shown::Ready(p)) if p.card().is_some()),
        "the info card"
    );
    one_box(&drv, "the card, ready");
    let path = pane.want.path.clone();
    pane.host.refresh(&path);
    drv.idle(&mut pane, 1);
    assert!(pane.host.busy(), "prepared again");
    one_box(&drv, "read again");
}

/// In a window where pages show (this thread's engine the fake), a plain `.xml` — its page not
/// its default view (09 §5), so it previews as text — is framed in cells while it is read, as
/// its preview will be; an `.html`, whose page always is, is egui's.
#[test]
fn in_a_window_with_pages_a_plain_xml_is_cells_while_it_is_read() {
    let fake = crate::gui::web::fake::FakeEngine::native();
    fake.install();
    for (name, cells) in [("pom.xml", true), ("index.html", false)] {
        let mut pane = pane_at(&format!("/d/{name}"), all);
        let _tx = reading(&mut pane, 1 << 10, 4 << 10);
        let mut drv = Driver::new(SIZE.x, SIZE.y);
        drv.idle(&mut pane, 2);
        assert!(
            pane.host.pages_here(&drv.ctx, name).shown(),
            "{name}: pages show here"
        );
        assert_eq!(
            layer_at(&drv, grid().all()).is_some(),
            cells,
            "{name}: framed in cells"
        );
    }
}

/// The bar moves to another file (the display commits it): its side's frame is decided again —
/// a text file's cells while it is read after a picture's box, and back.
#[test]
fn another_file_on_display_decides_its_frame_again() {
    let files = [
        ("photo.png", png(40, 30)),
        ("notes.txt", b"plain notes\n".to_vec()),
    ];
    let (tmp, mut pane) = pane_on(&files, "photo.png", all);
    let notes = pane_in(&tmp, "notes.txt", all).want;
    let photo = pane.want.clone();
    let mut drv = Driver::new(SIZE.x, SIZE.y);
    crate::gui::mark_terminal(&drv.ctx);
    drv.until(&mut pane, 20, |p| p.ready());
    drv.idle(&mut pane, 2);
    one_box(&drv, "the picture, ready");
    pane.want = notes;
    drv.idle(&mut pane, 1);
    assert!(pane.host.busy(), "the text file is read");
    assert_eq!(
        terminal_screen(&drv).hand_offs,
        [],
        "its side: cells at once"
    );
    assert!(layer_at(&drv, grid().all()).is_some(), "its frame of cells");
    drv.until(&mut pane, 20, |p| p.ready());
    drv.idle(&mut pane, 2);
    pane.want = photo;
    drv.idle(&mut pane, 1);
    one_box(&drv, "the picture again, being read");
}

// ---------------------------------------------------------------------------------------------
// TS-1: what the window paints is what a terminal shows.

/// One cell as the window painted it.
#[derive(Debug, Clone, PartialEq)]
struct Painted {
    symbol: String,
    fg: Color32,
    bg: Color32,
    /// Something was painted there.
    covered: bool,
}

/// One idle pass of `win` in `ctx` on a `cols` × `rows` window: its shapes.
fn shapes_of(
    ctx: &egui::Context,
    win: &mut impl Window,
    cols: u16,
    rows: u16,
    time: f64,
) -> Vec<ClippedShape> {
    let size = Vec2::new(
        f32::from(cols) * chrome::CELL.x,
        f32::from(rows) * chrome::CELL.y,
    );
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(time),
        max_texture_side: Some(8192),
        focused: true,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| {
        cells::clear(ui.ctx());
        win.ui(ui);
    });
    out.textures_delta.clear();
    out.shapes
}

/// The window's shapes laid back onto cells, in painting order (10 §5: what it painted, not the
/// buffers it was given): an opaque rectangle is a background on the cells whose centres it
/// covers inside its clip, their glyphs gone; a text is a glyph at the cell its corner is on — on
/// the grid, or the window shows what no terminal can — a wide one taking the next cell too.
fn painted(shapes: &[ClippedShape], cols: u16, rows: u16) -> Vec<Painted> {
    let p = chrome::PALETTE;
    let blank = Painted {
        symbol: " ".into(),
        fg: p.text,
        bg: p.back,
        covered: false,
    };
    let (w, h) = (usize::from(cols), usize::from(rows));
    let mut out = vec![blank; w * h];
    let centre = |x: usize, y: usize| {
        Pos2::new(
            (x as f32 + 0.5) * chrome::CELL.x,
            (y as f32 + 0.5) * chrome::CELL.y,
        )
    };
    fn flat<'a>(shape: &'a egui::Shape, all: &mut Vec<&'a egui::Shape>) {
        match shape {
            egui::Shape::Vec(v) => v.iter().for_each(|s| flat(s, all)),
            other => all.push(other),
        }
    }
    for c in shapes {
        let mut all = vec![];
        flat(&c.shape, &mut all);
        for shape in all {
            match shape {
                egui::Shape::Rect(r) if r.fill.a() == 255 => {
                    let shown = r.rect.intersect(c.clip_rect);
                    for y in 0..h {
                        for x in 0..w {
                            if shown.contains(centre(x, y)) {
                                out[y * w + x].symbol = " ".into();
                                out[y * w + x].bg = r.fill;
                                out[y * w + x].covered = true;
                            }
                        }
                    }
                }
                egui::Shape::Text(t) if !t.galley.text().trim().is_empty() => {
                    let text = t.galley.text();
                    assert!(
                        t.pos.x % chrome::CELL.x == 0.0 && t.pos.y % chrome::CELL.y == 0.0,
                        "a text off the grid at {:?}: {text:?}",
                        t.pos
                    );
                    let (x, y) = (
                        (t.pos.x / chrome::CELL.x) as usize,
                        (t.pos.y / chrome::CELL.y) as usize,
                    );
                    if x >= w || y >= h || !c.clip_rect.contains(centre(x, y)) {
                        continue;
                    }
                    let fg = (t.galley.job.sections.first())
                        .map_or(t.fallback_color, |s| s.format.color);
                    let fg = t.override_text_color.unwrap_or(fg);
                    out[y * w + x].symbol = text.to_string();
                    out[y * w + x].fg = fg;
                    out[y * w + x].covered = true;
                    if cells::width(text) == 2 && x + 1 < w {
                        out[y * w + x + 1].symbol = String::new();
                        out[y * w + x + 1].fg = fg;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Every cell where `window` and the terminal's `screen` differ — character and background
/// everywhere, the foreground where a glyph is painted — at most 20, said.
fn differences(window: &[Painted], screen: &crate::gui::term::Screen) -> Vec<String> {
    let shown = Layer {
        origin: Pos2::ZERO,
        layer: egui::LayerId::background(),
        clip: Rect::EVERYTHING,
        buf: screen.buf.clone(),
        caret: None,
    };
    let cols = usize::from(screen.buf.area.width);
    let mut out = vec![];
    for (y, row) in shown.seen().iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            let win = &window[y * cols + x];
            let glyph = !cell.symbol.trim().is_empty();
            let fg = cell.fg.unwrap_or(chrome::PALETTE.text);
            let bg = cell.bg.unwrap_or(chrome::PALETTE.back);
            if win.symbol != cell.symbol || win.bg != bg || (glyph && win.fg != fg) {
                out.push(format!(
                    "({x},{y}): window {win:?}, terminal {:?} {fg:?} {bg:?}",
                    cell.symbol
                ));
            }
        }
    }
    out.truncate(20);
    out
}

/// TS-1 (10 §5, §15): a text preview — a Hangul name, Hangul rows — and a hex one: what the
/// window paints, laid back onto cells, is what a terminal shows of the same preview, cell for
/// cell; one cell of the window's changed is one difference.
#[test]
fn ts1_the_window_paints_the_cells_a_terminal_shows_of_a_text_or_hex_preview() {
    let tmp = folder(&[
        ("한글 노트.txt", hangul_rows()),
        ("data.bin", (0..=255u8).cycle().take(4096).collect()),
    ]);
    for (name, shown) in [
        ("한글 노트.txt", "00 한글 줄 text 안녕하세요"),
        ("data.bin", "00 01 02 03"),
    ] {
        // The window: its passes, then the shapes of one more.
        let mut window = pane_in(&tmp, name, all);
        let mut drv = Driver::new(SIZE.x, SIZE.y);
        drv.until(&mut window, 20, |p| p.ready());
        drv.idle(&mut window, 2);
        let painted = painted(&shapes_of(&drv.ctx, &mut window, 100, 30, 1000.0), 100, 30);
        assert!(
            painted.iter().all(|c| c.covered),
            "{name}: every cell is painted"
        );
        // The terminal: the same file previewed in a context of its own, its layers laid out.
        let mut terminal = pane_in(&tmp, name, all);
        let mut term = Driver::new(SIZE.x, SIZE.y);
        crate::gui::mark_terminal(&term.ctx);
        term.until(&mut terminal, 20, |p| p.ready());
        term.idle(&mut terminal, 2);
        let screen = terminal_screen(&term);
        let text = (0..30)
            .map(|y| cells::row_text(&screen.buf, y).0)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains(name) && text.contains(shown),
            "{name}: {shown:?} in the terminal:\n{text}"
        );
        let d = differences(&painted, &screen);
        assert!(
            d.is_empty(),
            "{name}: the window's cells are the terminal's:\n{}",
            d.join("\n")
        );
        let mut changed = painted.clone();
        changed[100 * 5 + 10].symbol = "X".into();
        assert_eq!(
            differences(&changed, &screen).len(),
            1,
            "{name}: the control"
        );
    }
}
