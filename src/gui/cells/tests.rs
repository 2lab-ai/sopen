//! The cell core: the grid's geometry, what the painter leaves on screen laid back onto cells (a
//! glyph per cell, background runs, a wide glyph's second cell, the caret), the cell API's texts
//! and frames, the read-back, the 256-colour table.

use super::*;
use crate::gui::chrome::{cga, PALETTE};
use eframe::egui::epaint::ClippedShape;

/// A context in the skin, as the test kit's driver makes one, recording what is painted in cells.
fn skin() -> egui::Context {
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    set_recording(&ctx, true);
    ctx
}

/// One headless pass of `ctx` in a window `size` points, its ui painting with `draw` (no
/// renderer: the shapes are inspected): what it left on screen and the layers it recorded.
fn pass(
    ctx: &egui::Context,
    size: Vec2,
    mut draw: impl FnMut(&egui::Painter),
) -> (Vec<ClippedShape>, Vec<Arc<Layer>>) {
    clear(ctx);
    let raw = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
        time: Some(0.0),
        max_texture_side: Some(8192),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| draw(ui.painter()));
    out.textures_delta.clear();
    (std::mem::take(&mut out.shapes), layers(ctx))
}

/// `buf` painted alone with its cell (0, 0) at `origin`: the shapes and its layer.
fn painted(buf: &Buffer, origin: Pos2, caret: Option<Position>) -> (Vec<ClippedShape>, Arc<Layer>) {
    let (shapes, mut layers) = pass(&skin(), Vec2::new(800.0, 600.0), |p| {
        paint(p, origin, buf, caret)
    });
    assert_eq!(layers.len(), 1);
    (shapes, layers.remove(0))
}

/// Every glyph the shapes paint: its text, where it is drawn and the clip it is cut to.
fn glyphs(shapes: &[ClippedShape]) -> Vec<(String, Pos2, Rect)> {
    shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos, c.clip_rect)),
            _ => None,
        })
        .collect()
}

/// Every filled rectangle the shapes paint, in order.
fn fills(shapes: &[ClippedShape]) -> Vec<(Rect, Color32)> {
    shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(r) => Some((r.rect, r.fill)),
            _ => None,
        })
        .collect()
}

/// A cell as the painted shapes show it.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    symbol: String,
    fg: Option<Color32>,
    bg: Option<Color32>,
    caret: bool,
}

/// The shapes a pass painted laid back onto the cells of `area` (cell (0, 0) at `origin`) — what
/// the window painted, not the buffer it was given, as the parity oracle reads it (10 §5): a
/// rectangle of whole cells is their background (the later over the earlier), one cell wide and
/// two points tall at a cell's foot the caret, a text the glyph of the cell at its corner and of
/// as many cells as a terminal gives it ([`terminal_cells`], not the core's `width`), all of what
/// can be seen of it inside them. Anything else, or anything off the grid, fails.
fn laid_back(shapes: &[ClippedShape], origin: Pos2, area: Cells) -> Vec<Vec<Shown>> {
    let blank = Shown {
        symbol: " ".to_string(),
        fg: None,
        bg: None,
        caret: false,
    };
    let mut cells = vec![vec![blank; usize::from(area.width)]; usize::from(area.height)];
    let whole = rect_at(origin, area);
    let corner = |p: Pos2| {
        let d = (p - origin) / CELL;
        assert!(
            d.x.fract() == 0.0 && d.y.fract() == 0.0,
            "{p:?} is not a cell's corner"
        );
        (
            d.x as usize - usize::from(area.x),
            d.y as usize - usize::from(area.y),
        )
    };
    for c in shapes {
        match &c.shape {
            egui::Shape::Rect(r) => {
                assert!(whole.contains_rect(r.rect), "off the cells: {:?}", r.rect);
                let size = r.rect.size();
                if size == Vec2::new(CELL.x, 2.0) {
                    let (x, y) = corner(r.rect.max - CELL);
                    cells[y][x].caret = true;
                    continue;
                }
                let n = size.x / CELL.x;
                assert!(
                    size.y == CELL.y && n.fract() == 0.0,
                    "a background of whole cells in a row: {:?}",
                    r.rect
                );
                let (x, y) = corner(r.rect.min);
                for cell in &mut cells[y][x..x + n as usize] {
                    cell.bg = Some(r.fill);
                }
            }
            egui::Shape::Text(t) => {
                let (x, y) = corner(t.pos);
                let text = t.galley.text();
                let n = terminal_cells(text).clamp(1, usize::from(area.width) - x);
                let own = Rect::from_min_size(t.pos, Vec2::new(n as f32 * CELL.x, CELL.y));
                let seen = Rect::from_min_size(t.pos, t.galley.size()).intersect(c.clip_rect);
                assert!(
                    own.contains_rect(seen),
                    "{text:?} seen at {seen:?}, outside its cells {own:?}"
                );
                let colours: Vec<Color32> = t
                    .galley
                    .job
                    .sections
                    .iter()
                    .map(|s| s.format.color)
                    .collect();
                assert!(
                    colours.windows(2).all(|w| w[0] == w[1]),
                    "one colour a glyph"
                );
                for (k, cell) in cells[y][x..x + n].iter_mut().enumerate() {
                    cell.symbol = if k == 0 {
                        text.to_string()
                    } else {
                        String::new()
                    };
                    cell.fg = colours.first().copied();
                }
            }
            other => panic!("the painter drew {other:?}"),
        }
    }
    cells
}

/// The cells a terminal gives one painted glyph of the tests' texts, by hand from East Asian
/// Width, not by unicode-width (whose version the core's widths depend on): Hangul, CJK, the
/// ideographic space and emoji 2, a skin tone 0 (it joins the emoji before it), every other
/// character 1 — halfwidth katakana and its sound marks (ﾊﾟ) one each.
fn terminal_cells(glyph: &str) -> usize {
    glyph
        .chars()
        .map(|c| match c {
            '\u{1F3FB}'..='\u{1F3FF}' => 0,
            '\u{1100}'..='\u{115F}'
            | '\u{3000}'..='\u{303E}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{AC00}'..='\u{D7A3}'
            | '\u{1F300}'..='\u{1FAFF}' => 2,
            _ => 1,
        })
        .sum()
}

/// The layer's read-back is what was painted: every cell's character, background and caret,
/// and the foreground of every glyph painted.
fn assert_painted_as_seen(shapes: &[ClippedShape], layer: &Layer) {
    let shown = laid_back(shapes, layer.origin, layer.buf.area);
    let seen = layer.seen();
    assert_eq!(shown.len(), seen.len());
    for (y, (seen, shown)) in seen.iter().zip(&shown).enumerate() {
        assert_eq!(shown.len(), seen.len());
        // A blank glyph paints nothing: its cells show as unpainted, both of a wide one (U+3000).
        let mut blank = false;
        for (x, (s, p)) in seen.iter().zip(shown).enumerate() {
            if !s.symbol.is_empty() {
                blank = s.symbol.trim().is_empty();
            }
            let symbol = if blank { " " } else { &s.symbol };
            assert_eq!(p.symbol, symbol, "cell ({x}, {y})");
            assert_eq!(p.bg, s.bg, "cell ({x}, {y}) {symbol:?}");
            assert_eq!(p.caret, s.caret, "cell ({x}, {y})");
            if p.fg.is_some() {
                assert_eq!(p.fg, Some(s.fg.unwrap_or(PALETTE.text)), "cell ({x}, {y})");
            }
        }
    }
}

fn body() -> Pen {
    Pen::new(PALETTE.text, PALETTE.back)
}

fn bar() -> Pen {
    Pen::new(PALETTE.bar_text, PALETTE.bar)
}

fn empty(w: u16, h: u16) -> Buffer {
    Buffer::empty(Cells::new(0, 0, w, h))
}

// ---------------------------------------------------------------------------------------------
// The grid.

/// `Grid::over` counts the whole cells from the area's corner rounded to a point, the rest of
/// the area left over; `rect` is a cell rect in points; `at` the cell under a point.
#[test]
fn the_grid_is_the_whole_cells_from_the_areas_corner() {
    let area = Rect::from_min_max(Pos2::new(10.4, 20.0), Pos2::new(1013.4, 507.0));
    let g = Grid::over(area);
    assert_eq!(g.origin, Pos2::new(10.0, 20.0));
    assert_eq!((g.cols, g.rows), (125, 30));
    assert_eq!(g.all(), Cells::new(0, 0, 125, 30));
    assert!(area.expand(0.5).contains_rect(g.rect(g.all())));
    assert_eq!(
        g.rect(Cells::new(2, 3, 4, 5)),
        Rect::from_min_size(Pos2::new(26.0, 68.0), Vec2::new(32.0, 80.0))
    );
    // Less than a cell, an empty area, an upside-down one: no cells.
    for nothing in [
        Rect::from_min_size(Pos2::ZERO, Vec2::new(7.9, 15.9)),
        Rect::from_min_size(Pos2::new(5.0, 5.0), Vec2::ZERO),
        Rect::NOTHING,
    ] {
        assert_eq!(Grid::over(nothing).all().area(), 0, "{nothing:?}");
    }
    assert_eq!(g.at(Pos2::new(10.0, 20.0)), Some(Position::new(0, 0)));
    assert_eq!(g.at(Pos2::new(17.9, 35.9)), Some(Position::new(0, 0)));
    assert_eq!(g.at(Pos2::new(18.0, 36.0)), Some(Position::new(1, 1)));
    let end = g.rect(g.all()).max;
    assert_eq!(g.at(end - Vec2::splat(0.5)), Some(Position::new(124, 29)));
    for off in [Pos2::new(9.9, 20.0), Pos2::new(10.0, 19.9), end] {
        assert_eq!(g.at(off), None, "{off:?}");
    }
}

/// Two sides on one grid give the odd column to the right one: the browser's 1,000-point area
/// is 125 cells, 62 on the left and 63 (504 points) on the right, where the old half was 496
/// (10 §4); the sides touch.
#[test]
fn the_odd_column_goes_to_the_right_side() {
    let g = Grid::over(Rect::from_min_size(
        Pos2::new(0.0, 16.0),
        Vec2::new(1000.0, 640.0),
    ));
    let [left, right] = halves(g.all());
    assert_eq!(left, Cells::new(0, 0, 62, 40));
    assert_eq!(right, Cells::new(62, 0, 63, 40));
    assert_eq!(g.rect(right).width(), 504.0);
    assert_eq!(g.rect(left).max.x, g.rect(right).min.x);
    assert_eq!(
        halves(Cells::new(3, 1, 124, 2)),
        [Cells::new(3, 1, 62, 2), Cells::new(65, 1, 62, 2)]
    );
    assert_eq!(
        halves(Cells::new(0, 0, 1, 1)),
        [Cells::new(0, 0, 0, 1), Cells::new(0, 0, 1, 1)]
    );
}

// ---------------------------------------------------------------------------------------------
// The painter.

/// Each glyph is a text of its own at its cell's corner, (col × 8, row × 16) points from the
/// origin, cut to its cells — never laid out with its neighbours.
#[test]
fn each_glyph_is_painted_at_its_own_cell() {
    let mut buf = empty(10, 2);
    put(&mut buf, 0, 0, "AB한C", 10, body());
    put(&mut buf, 9, 1, "x", 1, body());
    let origin = Pos2::new(24.0, 32.0);
    let (shapes, layer) = painted(&buf, origin, None);
    let at = |x: f32, y: f32| origin + Vec2::new(x, y);
    // Each within its cells: none needs a clip of its own.
    assert_eq!(
        glyphs(&shapes),
        vec![
            ("A".to_string(), at(0.0, 0.0), layer.clip),
            ("B".to_string(), at(8.0, 0.0), layer.clip),
            ("한".to_string(), at(16.0, 0.0), layer.clip),
            ("C".to_string(), at(32.0, 0.0), layer.clip),
            ("x".to_string(), at(72.0, 16.0), layer.clip),
        ]
    );
    assert_painted_as_seen(&shapes, &layer);
}

/// Only a glyph wider than its cells is cut to them — `⇧` is 9.6 points in egui's own font, a
/// cell wide by unicode-width (an emoji drawn over two cells is the same case) — so a full
/// 100×30 screen of text, Hangul and emoji is one draw call, and with that glyph three; nothing
/// of it is seen outside its cell.
#[test]
fn only_a_glyph_wider_than_its_cells_is_cut_to_them() {
    let ctx = skin();
    let size = Vec2::new(800.0, 480.0);
    let mut buf = empty(100, 30);
    for y in 0..30 {
        put(&mut buf, 0, y, &"abcdefghij".repeat(10), 100, body());
    }
    put(&mut buf, 0, 5, "한글 문서 초안.txt \u{1F3B5}", 100, bar());
    let (shapes, _) = pass(&ctx, size, |p| paint(p, Pos2::ZERO, &buf, None));
    assert!(glyphs(&shapes).len() > 2900);
    assert_eq!(ctx.tessellate(shapes, 1.0).len(), 1, "one draw call");

    let arrow = ctx.fonts_mut(|f| {
        f.layout_no_wrap("⇧".to_string(), chrome::font(), Color32::WHITE)
            .size()
    });
    assert_eq!((width("⇧"), arrow.x > CELL.x), (1, true), "{arrow:?}");
    put(&mut buf, 40, 7, "⇧", 1, body());
    let (shapes, layers) = pass(&ctx, size, |p| paint(p, Pos2::ZERO, &buf, None));
    let cell = layers[0].rect(Cells::new(40, 7, 1, 1));
    let own: Vec<Rect> = glyphs(&shapes)
        .into_iter()
        .filter(|g| g.2 != layers[0].clip)
        .map(|g| g.2)
        .collect();
    assert_eq!(own, vec![cell]);
    assert_painted_as_seen(&shapes, &layers[0]);
    assert_eq!(ctx.tessellate(shapes, 1.0).len(), 3);
}

/// The backgrounds of a row are one rectangle per run of a colour; a cell never written (no RGB
/// background) is not painted.
#[test]
fn backgrounds_are_painted_in_runs() {
    let mut buf = empty(6, 2);
    fill(&mut buf, Cells::new(0, 0, 6, 1), body());
    fill(&mut buf, Cells::new(3, 0, 2, 1), bar());
    put(&mut buf, 2, 1, "z", 1, Pen::new(cga::WHITE, cga::BLACK));
    let (shapes, _) = painted(&buf, Pos2::ZERO, None);
    let run = |x: f32, y: f32, w: f32| {
        Rect::from_min_size(
            Pos2::new(x * CELL.x, y * CELL.y),
            Vec2::new(w * CELL.x, CELL.y),
        )
    };
    assert_eq!(
        fills(&shapes),
        vec![
            (run(0.0, 0.0, 3.0), PALETTE.back),
            (run(3.0, 0.0, 2.0), PALETTE.bar),
            (run(5.0, 0.0, 1.0), PALETTE.back),
            (run(2.0, 1.0, 1.0), cga::BLACK),
        ]
    );
}

/// A wide glyph's second cell has the glyph's colours, in the buffer and on screen: the bar on
/// `사진.png` is `ff5555` in every cell over the `0000aa` body (10 §4: before the spike's fix the
/// second cells of 사 and 진 were the body's). The control: ratatui-core alone resets them, and
/// the painter still paints such a cell in its glyph's colours, as a terminal does.
#[test]
fn a_wide_glyphs_second_cell_has_the_glyphs_colours() {
    assert_eq!(PALETTE.bar, Color32::from_rgb(0xff, 0x55, 0x55));
    assert_eq!(PALETTE.back, Color32::from_rgb(0x00, 0x00, 0xaa));
    let row = |raw: bool| {
        let mut buf = empty(12, 1);
        let all = buf.area;
        fill(&mut buf, all, body());
        fill(&mut buf, Cells::new(0, 0, 10, 1), bar());
        if raw {
            buf.set_stringn(0, 0, "사진.png", 10, bar().style());
        } else {
            assert_eq!(put(&mut buf, 0, 0, "사진.png", 10, bar()), 8);
        }
        buf
    };
    let raw = row(true);
    assert_eq!(
        (raw[(1, 0)].bg, raw[(3, 0)].bg),
        (Color::Reset, Color::Reset)
    );
    let fixed = row(false);
    for x in 0..10 {
        assert_eq!(fixed[(x, 0)].bg, rgb(PALETTE.bar), "cell {x}");
        assert_eq!(fixed[(x, 0)].fg, rgb(PALETTE.bar_text), "cell {x}");
    }
    assert_eq!(row_text(&fixed, 0).0, "사진.png    ");
    for buf in [raw, fixed] {
        let (shapes, layer) = painted(&buf, Pos2::new(8.0, 16.0), None);
        let shown = laid_back(&shapes, layer.origin, buf.area);
        let backs: Vec<Option<Color32>> = shown[0].iter().map(|c| c.bg).collect();
        let mut want = vec![Some(PALETTE.bar); 10];
        want.extend([Some(PALETTE.back); 2]);
        assert_eq!(backs, want);
        assert_eq!(
            (shown[0][1].fg, shown[0][3].fg),
            (Some(cga::WHITE), Some(cga::WHITE))
        );
        assert_painted_as_seen(&shapes, &layer);
    }
}

/// unicode-width's widths (10 §10): Hangul, CJK and emoji — a skin-toned hand one grapheme —
/// two cells; box lines, "…", "░", Latin one. A Hangul name takes the same cells as in a
/// terminal, its 6 Hangul in 12 (TC-1), each wide glyph painted at its first cell and nothing of
/// it seen outside its two.
#[test]
fn hangul_and_emoji_take_two_cells_each() {
    for wide in [
        "한",
        "글",
        "漢",
        "\u{1F600}",
        "\u{1F44D}\u{1F3FD}",
        "\u{1F3B5}",
    ] {
        assert_eq!(width(wide), 2, "{wide:?}");
    }
    for narrow in ["─", "…", "░", "A", "é"] {
        assert_eq!(width(narrow), 1, "{narrow:?}");
    }
    // Halfwidth katakana with a sound mark is a grapheme of two cells, as a terminal shows it:
    // ratatui-core adds a cell for each mark, which unicode-width 0.2.3 already counts (3 cells).
    assert_eq!(width("ﾊﾟｸﾞ"), 4);
    let mut buf = empty(24, 3);
    assert_eq!(put(&mut buf, 0, 0, "한글 문서 초안.txt", 24, body()), 18);
    assert_eq!(
        put(&mut buf, 0, 1, "\u{1F3B5} \u{1F44D}\u{1F3FD}!", 24, body()),
        6
    );
    assert_eq!(put(&mut buf, 0, 2, "ﾊﾟｸﾞ.txt", 20, body()), 8);
    assert_eq!(put(&mut buf, 10, 2, "\u{3000}z", 20, body()), 3);
    let (shapes, layer) = painted(&buf, Pos2::ZERO, None);
    let wide: Vec<(String, f32)> = glyphs(&shapes)
        .into_iter()
        .filter(|(t, _, _)| terminal_cells(t) == 2)
        .map(|(t, at, _)| (t, at.x / CELL.x))
        .collect();
    let want: Vec<(String, f32)> = [
        ("한", 0.0),
        ("글", 2.0),
        ("문", 5.0),
        ("서", 7.0),
        ("초", 10.0),
        ("안", 12.0),
        ("\u{1F3B5}", 0.0),
        ("\u{1F44D}\u{1F3FD}", 3.0),
        ("ﾊﾟ", 0.0),
        ("ｸﾞ", 2.0),
    ]
    .into_iter()
    .map(|(t, x)| (t.to_string(), x))
    .collect();
    assert_eq!(wide, want);
    assert_eq!(
        find(std::slice::from_ref(&layer), "z")[0].cells,
        Cells::new(12, 2, 1, 1)
    );
    assert_eq!(
        find(std::slice::from_ref(&layer), ".txt")[0].cells,
        Cells::new(14, 0, 4, 1)
    );
    assert_eq!(
        find(std::slice::from_ref(&layer), "!")[0].cells,
        Cells::new(5, 1, 1, 1)
    );
    assert_painted_as_seen(&shapes, &layer);
}

/// With the fonts a window has (the system font provider for Hangul), the name's glyphs still
/// stand at their cells: the font's Hangul fits a glyph's two cells (13.84 pt on the Mac, 14.72
/// on Linux, 10 §10), and laid out as one text — egui's own layout — the name would end before
/// its 18th cell, off the grid.
#[test]
fn the_grid_never_drifts_with_the_installed_fonts() {
    use egui_system_font_provider::SystemFontProvider;
    let ctx = skin();
    ctx.add_font_provider(Arc::new(SystemFontProvider::new()));
    let name = "한글 문서 초안.txt";
    let mut buf = empty(20, 1);
    put(&mut buf, 0, 0, name, 20, body());
    let size = Vec2::new(400.0, 100.0);
    let mut shapes = vec![];
    for _ in 0..3 {
        shapes = pass(&ctx, size, |p| paint(p, Pos2::ZERO, &buf, None)).0;
    }
    // Where the glyphs stand does not depend on the font: asserted with or without one.
    let starts: Vec<f32> = glyphs(&shapes)
        .iter()
        .map(|(_, at, _)| at.x / CELL.x)
        .collect();
    assert_eq!(
        starts,
        vec![0.0, 2.0, 5.0, 7.0, 10.0, 12.0, 14.0, 15.0, 16.0, 17.0]
    );
    let f = chrome::font();
    let han = ctx.fonts_mut(|fonts| {
        fonts
            .has_glyph(&f, '한')
            .then(|| fonts.glyph_width(&f, '한'))
    });
    let Some(han) = han else {
        eprintln!("skipping the font's measures: no installed font with Hangul");
        return;
    };
    // A font whose Hangul is a full em (16 points) may measure a hair over it.
    assert!(han <= 2.0 * CELL.x + 0.05, "한 is {han} pt");
    let one_text = ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(name.to_string(), f.clone(), Color32::WHITE)
            .size()
            .x
    });
    if han < 2.0 * CELL.x {
        assert!(
            one_text < 18.0 * CELL.x,
            "laid out as one text: {one_text} pt"
        );
    }
}

/// The caret is the two scan lines at its cell's foot in the cell's foreground, after the
/// glyphs; the layer keeps its cell and the read-back has it on that cell only. A caret off the
/// buffer is not painted.
#[test]
fn the_caret_is_two_scan_lines_at_its_cells_foot() {
    let field = Pen::new(PALETTE.field_text, cga::BLACK);
    let mut buf = empty(6, 1);
    let all = buf.area;
    fill(&mut buf, all, field);
    put(&mut buf, 0, 0, "ab", 6, field);
    let origin = Pos2::new(40.0, 48.0);
    let (shapes, layer) = painted(&buf, origin, Some(Position::new(2, 0)));
    let foot = Rect::from_min_size(origin + Vec2::new(16.0, 14.0), Vec2::new(8.0, 2.0));
    assert_eq!(fills(&shapes).last(), Some(&(foot, cga::WHITE)));
    assert!(matches!(
        shapes.last().map(|c| &c.shape),
        Some(egui::Shape::Rect(_))
    ));
    assert_eq!(layer.caret, Some(Position::new(2, 0)));
    let carets: Vec<bool> = layer.seen()[0].iter().map(|s| s.caret).collect();
    assert_eq!(carets, [false, false, true, false, false, false]);
    assert_painted_as_seen(&shapes, &layer);
    let (off, _) = painted(&buf, origin, Some(Position::new(6, 0)));
    assert_eq!(fills(&off).len(), 1, "only the field's background");
}

/// The read-back round trip: a window drawn with the cell API — its frame and title with Hangul,
/// names with an emoji, the bar, the rule and the foot, the scroll bar, a caret — painted and laid
/// back onto cells is the layer's read-back, cell for cell; the read-back says what was written.
#[test]
fn what_is_painted_is_what_the_read_back_says() {
    let mut buf = empty(40, 12);
    let all = buf.area;
    let inner = frame(&mut buf, all, "/srv/사진", true);
    assert_eq!(inner, Cells::new(1, 1, 38, 10));
    put(
        &mut buf,
        1,
        1,
        "Name",
        10,
        Pen::new(PALETTE.dim, PALETTE.back),
    );
    put(&mut buf, 1, 2, "노래 \u{1F3B5}.mp3", 30, body());
    fill(&mut buf, Cells::new(1, 3, 37, 1), bar());
    put(&mut buf, 1, 3, "사진.png", 30, bar());
    put_right(&mut buf, 38, 3, "12", bar());
    let all = buf.area;
    rule(&mut buf, all, 9, true);
    put(&mut buf, 1, 10, "1 of 3", 30, body());
    scroll_bar(&mut buf, 39, 1, 8, Some(0.5));
    let origin = Pos2::new(8.0, 16.0);
    let (shapes, layer) = painted(&buf, origin, Some(Position::new(9, 3)));
    assert_painted_as_seen(&shapes, &layer);

    let seen = layer.seen();
    let text = |y: usize| -> String { seen[y].iter().map(|s| s.symbol.as_str()).collect() };
    assert!(
        text(0).starts_with("╔═════════════ /srv/사진 "),
        "{}",
        text(0)
    );
    assert!(text(9).starts_with("╟───") && text(9).ends_with("─╢"));
    assert_eq!(seen[3][1].symbol, "사");
    assert_eq!(
        seen[3][2],
        Seen {
            symbol: String::new(),
            ..seen[3][1].clone()
        }
    );
    assert_eq!(seen[3][1].bg, Some(PALETTE.bar));
    assert_eq!(seen[3][36].symbol, "1");
    assert_eq!(seen[1][39].symbol, "↑");
    // Half way down the 5 cells the thumb can stand in: 2.5 rounds to the third.
    assert_eq!(seen[5][39].bg, Some(PALETTE.thumb));
    assert!(seen[3][9].caret);
}

/// What one paint drew is recorded for the pass when recording is on — its origin, the egui layer
/// and the clip it was painted under, the cells and the caret, in the order of the calls — and
/// nothing when it is off (the default); `clear` forgets the last pass's.
#[test]
fn the_layers_of_a_pass_are_recorded_when_recording_is_on() {
    let ctx = egui::Context::default();
    chrome::install(&ctx);
    let size = Vec2::new(200.0, 100.0);
    let (a, b) = (empty(3, 1), empty(2, 2));
    let clip = Rect::from_min_size(Pos2::new(16.0, 16.0), Vec2::new(40.0, 40.0));
    let over = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("over"));
    let two = |p: &egui::Painter| {
        paint(p, Pos2::ZERO, &a, None);
        let mut top = p.with_clip_rect(clip);
        top.set_layer_id(over);
        paint(&top, Pos2::new(16.0, 16.0), &b, Some(Position::new(1, 1)));
    };
    assert!(pass(&ctx, size, two).1.is_empty(), "off by default");
    set_recording(&ctx, true);
    let layers = pass(&ctx, size, two).1;
    assert_eq!(layers.len(), 2);
    assert_eq!(
        (layers[0].layer.order, layers[1].layer),
        (egui::Order::Background, over)
    );
    assert_eq!(
        (
            layers[0].origin,
            layers[0].clip,
            layers[0].buf.area,
            layers[0].caret
        ),
        (
            Pos2::ZERO,
            Rect::from_min_size(Pos2::ZERO, size),
            a.area,
            None
        )
    );
    assert_eq!(
        (
            layers[1].origin,
            layers[1].clip,
            layers[1].buf.area,
            layers[1].caret
        ),
        (
            Pos2::new(16.0, 16.0),
            clip,
            b.area,
            Some(Position::new(1, 1))
        )
    );
    assert_eq!(
        layers[1].rect(Cells::new(1, 1, 1, 1)).min,
        Pos2::new(24.0, 32.0)
    );
    assert!(
        pass(&ctx, size, |_| {}).1.is_empty(),
        "the last pass's forgotten"
    );
    set_recording(&ctx, false);
    assert!(pass(&ctx, size, two).1.is_empty());
}

// ---------------------------------------------------------------------------------------------
// The cell API.

/// Text is cut by cells and never splits a wide glyph: `put` stops before one that does not fit
/// (in its room or the buffer); `fit_start` and `fit_left` end a cell earlier with "…" and keep
/// a grapheme whole (a skin-toned hand is one); no room is no text.
#[test]
fn text_is_cut_by_cells_never_half_a_wide_glyph() {
    let mut buf = empty(8, 2);
    assert_eq!(put(&mut buf, 0, 0, "ab한", 3, body()), 2);
    assert_eq!(buf[(2, 0)], Cell::EMPTY);
    assert_eq!(put(&mut buf, 5, 1, "ab한", 9, body()), 2);
    assert_eq!(put(&mut buf, 8, 1, "a", 1, body()), 0, "off the buffer");
    assert_eq!(put_right(&mut buf, 8, 0, "Size", body()), 4);
    assert_eq!(row_text(&buf, 0).0, "ab  Size");

    assert_eq!(fit_start("abc", 3), "abc");
    assert_eq!(fit_start("ab한", 3), "ab…");
    assert_eq!(fit_start("한글문서", 5), "한글…");
    assert_eq!(fit_start("한글문서", 4), "한…");
    assert_eq!(
        fit_start("\u{1F44D}\u{1F3FD}\u{1F44D}\u{1F3FD}", 3),
        "\u{1F44D}\u{1F3FD}…"
    );
    assert_eq!(fit_start("abc", 0), "");
    assert_eq!(fit_left("/srv/한글", 9), "/srv/한글");
    assert_eq!(fit_left("/srv/한글", 5), "…한글");
    assert_eq!(fit_left("/srv/한글", 4), "…글");
    assert_eq!(fit_left("/srv/한글", 1), "…");
    assert_eq!(fit_left("/srv/한글", 0), "");
    for room in 0..12 {
        for text in ["/srv/한글", "ab한cd", "\u{1F44D}\u{1F3FD}x\u{1F3B5}"] {
            assert!(width(&fit_start(text, room)) <= room, "{text:?} in {room}");
            assert!(width(&fit_left(text, room)) <= room, "{text:?} in {room}");
        }
    }
}

/// Writing over half a wide glyph erases it — a space in its colours — as a terminal does; else
/// the glyph would still hide the cell written (the control: ratatui-core alone).
#[test]
fn writing_over_half_a_wide_glyph_erases_it() {
    let mut raw = empty(4, 1);
    put(&mut raw, 0, 0, "한", 4, bar());
    raw.set_stringn(1, 0, "x", 1, body().style());
    assert_eq!(row_text(&raw, 0).0, "한  ", "x hidden under 한");

    let mut buf = empty(4, 1);
    put(&mut buf, 0, 0, "한", 4, bar());
    put(&mut buf, 1, 0, "x", 1, body());
    assert_eq!(row_text(&buf, 0), (" x  ".to_string(), vec![0, 1, 2, 3]));
    assert_eq!(buf[(0, 0)].bg, rgb(PALETTE.bar));
    let mut filled = empty(4, 1);
    put(&mut filled, 0, 0, "한", 4, bar());
    fill(&mut filled, Cells::new(1, 0, 3, 1), body());
    assert_eq!(row_text(&filled, 0).0, "    ");
    // Nothing written, nothing erased: an empty text, a wide glyph with no room for it.
    let mut kept = empty(4, 1);
    put(&mut kept, 0, 0, "한", 4, bar());
    assert_eq!(put(&mut kept, 1, 0, "", 3, body()), 0);
    assert_eq!(put(&mut kept, 1, 0, "글", 1, body()), 0);
    assert_eq!(put_right(&mut kept, 1, 0, "", body()), 0);
    assert_eq!(row_text(&kept, 0).0, "한  ");
    let (shapes, layer) = painted(&buf, Pos2::ZERO, None);
    assert_eq!(glyphs(&shapes).len(), 1);
    assert_painted_as_seen(&shapes, &layer);
}

/// A window's frame: a white double line round the active one, a light grey single line round
/// the others, on the body; the title centred with a blank cell either side, cut as the egui
/// frame cuts it (`chrome::frame_title`); the room inside returned. The rule joins the sides
/// (`╟─╢` / `├─┤`); the scroll bar is ↑, ░ and ↓ with the red thumb where `at` puts it.
#[test]
fn frames_rules_and_the_scroll_bar() {
    for active in [true, false] {
        let mut buf = empty(20, 6);
        let all = buf.area;
        assert_eq!(
            frame(&mut buf, all, "File Edit", active),
            Cells::new(1, 1, 18, 4)
        );
        let all = buf.area;
        rule(&mut buf, all, 3, active);
        let rows: Vec<String> = (0..6).map(|y| row_text(&buf, y).0).collect();
        let want = if active {
            [
                "╔═══ File Edit ════╗",
                "║                  ║",
                "║                  ║",
                "╟──────────────────╢",
                "║                  ║",
                "╚══════════════════╝",
            ]
        } else {
            [
                "┌─── File Edit ────┐",
                "│                  │",
                "│                  │",
                "├──────────────────┤",
                "│                  │",
                "└──────────────────┘",
            ]
        };
        assert_eq!(rows, want);
        let line = if active { cga::WHITE } else { cga::LIGHT_GRAY };
        for (x, y) in [(0, 0), (4, 0), (5, 0), (14, 0), (19, 2), (7, 3)] {
            assert_eq!(buf[(x, y)].fg, rgb(line), "({x}, {y})");
            assert_eq!(buf[(x, y)].bg, rgb(PALETTE.back), "({x}, {y})");
        }
        assert_eq!(buf[(5, 2)].fg, rgb(PALETTE.text), "the body");
        let run = runs(&buf).into_iter().find(|r| r.text == "File Edit");
        assert_eq!(run.map(|r| r.cells), Some(Cells::new(5, 0, 9, 1)));
    }
    // Cut as the window's frame cuts its title, at every width.
    let ctx = skin();
    let _ = pass(&ctx, Vec2::new(100.0, 100.0), |_| {});
    let title = "/home/user/projects";
    // From a room of one cell: in none the window's frame still drew a lone "…", the cells none.
    for cols in 7..30u16 {
        let mut buf = empty(cols, 3);
        let all = buf.area;
        frame(&mut buf, all, title, true);
        let room = usize::from(cols).saturating_sub(6);
        let window = chrome::frame_title(&ctx, title, f32::from(cols) * CELL.x);
        assert_eq!(fit_left(title, room), window, "{cols} cells");
        if !window.is_empty() {
            assert!(
                row_text(&buf, 0).0.contains(&format!(" {window} ")),
                "{cols} cells"
            );
        }
    }

    let bar = |rows: u16, at: Option<f32>| {
        let mut buf = empty(1, rows + 2);
        scroll_bar(&mut buf, 0, 1, rows, at);
        let column: String = (0..rows + 2)
            .map(|y| buf[(0, y)].symbol().to_string())
            .collect();
        let thumb: Vec<u16> = (0..rows + 2)
            .filter(|&y| buf[(0, y)].bg == rgb(PALETTE.thumb))
            .collect();
        (column, thumb)
    };
    assert_eq!(bar(6, None), (" ↑ ░░░↓ ".to_string(), vec![2]));
    assert_eq!(bar(6, Some(1.0)), (" ↑░░░ ↓ ".to_string(), vec![5]));
    assert_eq!(bar(6, Some(0.5)).1, vec![4]);
    assert_eq!(bar(2, Some(0.5)), (" ↑↓ ".to_string(), vec![]));
    assert_eq!(bar(1, None), ("   ".to_string(), vec![]));
}

/// The cell API sets no modifiers (T37): every cell it writes has none, whatever was there; the
/// read-back reports a modifier set directly, for the oracle to refuse.
#[test]
fn the_cell_api_sets_no_modifiers() {
    let marks = Modifier::BOLD | Modifier::UNDERLINED | Modifier::REVERSED | Modifier::SLOW_BLINK;
    let marked = || {
        let mut buf = empty(12, 6);
        for cell in &mut buf.content {
            cell.modifier = marks;
        }
        buf
    };
    let plain = |buf: &Buffer, r: Cells| r.positions().all(|p| buf[(p.x, p.y)].modifier.is_empty());
    let mut buf = marked();
    fill(&mut buf, Cells::new(1, 1, 3, 2), body());
    assert!(plain(&buf, Cells::new(1, 1, 3, 2)));
    assert_eq!(buf[(0, 0)].modifier, marks, "the rest untouched");
    let mut buf = marked();
    put(&mut buf, 2, 0, "a한b", 12, body());
    put_right(&mut buf, 12, 1, "12", body());
    assert!(plain(&buf, Cells::new(2, 0, 4, 1)) && plain(&buf, Cells::new(10, 1, 2, 1)));
    // Each on marks of its own: one that fills first would hide what the next one writes.
    let mut buf = marked();
    let all = buf.area;
    frame(&mut buf, all, "t", true);
    assert!(plain(&buf, all));
    let mut buf = marked();
    rule(&mut buf, all, 2, true);
    assert!(plain(&buf, Cells::new(0, 2, 12, 1)));
    assert_eq!(buf[(0, 1)].modifier, marks, "the rest untouched");
    let mut buf = marked();
    scroll_bar(&mut buf, 11, 1, 4, Some(0.5));
    assert!(plain(&buf, Cells::new(11, 1, 1, 4)));
    assert_eq!(buf[(10, 1)].modifier, marks, "the rest untouched");

    let mut buf = marked();
    put(&mut buf, 0, 0, "x", 1, body());
    let layer = Layer {
        origin: Pos2::ZERO,
        layer: egui::LayerId::background(),
        clip: Rect::EVERYTHING,
        buf,
        caret: None,
    };
    let seen = layer.seen();
    assert!(seen[0][0].modifier.is_empty());
    assert_eq!(seen[0][1].modifier, marks);
}

// ---------------------------------------------------------------------------------------------
// The read-back.

/// The test kit's queries on a layer: a row's text and the cell of each character, where a text
/// stands (across a wide glyph), its colours merged, and the runs — one pair of colours, cut at
/// two spaces and at frame glyphs.
#[test]
fn the_read_back_finds_text_its_cells_and_colours() {
    let mut buf = empty(24, 3);
    let all = buf.area;
    frame(&mut buf, all, "", true);
    put(&mut buf, 2, 1, "abc", 3, body());
    put(
        &mut buf,
        7,
        1,
        "def",
        3,
        Pen::new(PALETTE.dim, PALETTE.back),
    );
    put(&mut buf, 11, 1, "사진.png", 8, bar());
    let layer = Arc::new(Layer {
        origin: Pos2::new(8.0, 16.0),
        layer: egui::LayerId::background(),
        clip: Rect::EVERYTHING,
        buf,
        caret: None,
    });
    let (text, at) = row_text(&layer.buf, 1);
    assert_eq!(text, "║ abc  def 사진.png    ║");
    assert_eq!(at.len(), text.chars().count());
    assert_eq!((at[11], at[12], at[13]), (11, 13, 15));
    let layers = [layer.clone()];
    let hits = find(&layers, "진.p");
    assert_eq!(
        hits,
        vec![Hit {
            layer: 0,
            cells: Cells::new(13, 1, 4, 1)
        }]
    );
    assert_eq!(
        layer.rect(hits[0].cells),
        Rect::from_min_size(Pos2::new(8.0 + 13.0 * 8.0, 32.0), Vec2::new(32.0, 16.0))
    );
    assert!(find(&layers, "").is_empty());
    assert_eq!(find(&layers, "c  d").len(), 1);
    assert_eq!(
        colors(&layer, Cells::new(4, 1, 9, 1)),
        vec![PALETTE.text, PALETTE.dim, PALETTE.text, PALETTE.bar_text]
    );
    let runs: Vec<(Cells, String)> = runs(&layer.buf)
        .into_iter()
        .map(|r| (r.cells, r.text))
        .collect();
    assert_eq!(
        runs,
        vec![
            (Cells::new(2, 1, 3, 1), "abc".to_string()),
            (Cells::new(7, 1, 3, 1), "def".to_string()),
            (Cells::new(11, 1, 8, 1), "사진.png".to_string()),
        ]
    );
    assert!(frame_glyph('║') && frame_glyph('░') && !frame_glyph('↑') && !frame_glyph('…'));
}

/// An arrow is a frame glyph only where a scroll bar draws it, in the scroll bar's colours: a
/// status row's "↑ ↓ choose" is one text, and a text in the scroll bar's colours beside its ↓ is
/// not joined to it.
#[test]
fn an_arrow_is_text_outside_a_scroll_bar() {
    let p = PALETTE;
    let all = Cells::new(0, 0, 16, 3);
    let mut buf = Buffer::empty(all);
    fill(&mut buf, all, Pen::new(p.text, p.back));
    scroll_bar(&mut buf, 0, 0, 3, None);
    put(&mut buf, 2, 0, "↑ ↓ choose", 14, Pen::new(p.text, p.back));
    put(&mut buf, 1, 2, "ab", 2, Pen::new(p.track, p.track_back));
    let runs: Vec<(Cells, String)> = (runs(&buf).into_iter())
        .map(|r| (r.cells, r.text))
        .collect();
    assert_eq!(
        runs,
        vec![
            (Cells::new(2, 0, 10, 1), "↑ ↓ choose".to_string()),
            (Cells::new(1, 2, 2, 1), "ab".to_string()),
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// Colours.

/// In a 256-colour terminal the skin's 16 colours are §5's fixed table — the nearest of 16–255,
/// never 0–15 — and every opaque colour of the palette is one of the 16, so each colour the cell
/// API writes has its index.
#[test]
fn the_skins_colours_in_a_256_colour_terminal() {
    use cga::*;
    let table = [
        (BLACK, 16),
        (BLUE, 19),
        (GREEN, 34),
        (CYAN, 37),
        (RED, 124),
        (MAGENTA, 127),
        (BROWN, 130),
        (LIGHT_GRAY, 248),
        (DARK_GRAY, 240),
        (LIGHT_BLUE, 63),
        (LIGHT_GREEN, 83),
        (LIGHT_CYAN, 87),
        (LIGHT_RED, 203),
        (LIGHT_MAGENTA, 207),
        (YELLOW, 227),
        (WHITE, 231),
    ];
    for (colour, index) in table {
        assert_eq!(index256(colour), index, "{colour:?}");
    }
    let p = PALETTE;
    let used = [
        p.text,
        p.dim,
        p.folder,
        p.error,
        p.warn,
        p.bar,
        p.bar_text,
        p.bar_idle,
        p.selected,
        p.frame_active.color,
        p.frame_idle.color,
        p.back,
        p.title_back,
        p.title_text,
        p.program_back,
        p.program_text,
        p.key_digit,
        p.key_digit_back,
        p.key_back,
        p.key_text,
        p.dialog_back,
        p.dialog_text,
        p.dialog_weak,
        p.dialog_error,
        p.field_text,
        p.help_key,
        p.track_back,
        p.track,
        p.thumb,
        p.picture_back,
        p.changed,
        p.changed_back,
        p.timeline,
        p.wave,
        p.mark_in,
        p.mark_out,
        p.playhead,
        p.select,
        p.select_text,
    ];
    for c in used {
        assert!(table.iter().any(|&(t, _)| t == c), "{c:?} is a skin colour");
    }
    for c in [
        Color32::from_rgb(1, 2, 3),
        Color32::from_rgb(0x80, 0, 0),
        Color32::from_rgb(0xc0, 0xc0, 0xc0),
    ] {
        assert!(index256(c) >= 16, "{c:?}");
    }
}
