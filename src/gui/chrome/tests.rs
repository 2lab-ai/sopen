//! The skin: what `install` puts into a window, the code page 437 table, the F-key rows.

use super::*;
use crate::gui::testkit::Driver;
use crate::gui::Window;

/// A window made of the chrome's parts: a title row, a framed window with a label in it, the
/// F-key row.
struct Parts {
    active: bool,
}

impl Window for Parts {
    fn ui(&mut self, ui: &mut egui::Ui) {
        title_panel(ui, "parts-title", "box:/srv/data");
        key_panel(ui, "parts-keys", HEX_KEYS);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let outer = ui.max_rect();
                let inner = frame(&ui.painter_at(outer), outer, "File Edit", self.active);
                ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
                    ui.label("plain words");
                });
            });
    }

    fn ready(&self) -> bool {
        true
    }
}

/// Whether the last pass filled a rectangle of `fill` around `p`.
fn filled(drv: &Driver, p: Pos2, fill: Color32) -> bool {
    drv.rects()
        .iter()
        .any(|(r, f, _)| *f == fill && r.contains(p))
}

/// What `install` puts into a window is the palette: egui's own colours (text, the highlight,
/// the body, errors), no rounding, the shadow two cells right and one down, and every text
/// style at the VGA font's size.
#[test]
fn installed_visuals_are_the_palette() {
    let drv = Driver::new(640.0, 400.0);
    let style = drv.ctx.global_style();
    let v = &style.visuals;
    let p = PALETTE;
    assert_eq!(Palette::of(v), p);
    assert_eq!(v.text_color(), p.text);
    assert_eq!(v.weak_text_color(), p.dim);
    assert_eq!(v.selection.bg_fill, p.bar);
    assert_eq!(v.selection.stroke.color, p.bar_text);
    assert_eq!(v.panel_fill, p.back);
    assert_eq!(v.error_fg_color, p.error);
    assert_eq!(v.warn_fg_color, p.warn);
    assert_eq!(v.extreme_bg_color, cga::BLACK);
    assert!(v.dark_mode);
    assert_eq!(v.window_corner_radius, egui::CornerRadius::ZERO);
    assert_eq!(v.widgets.inactive.corner_radius, egui::CornerRadius::ZERO);
    let colours = |w: &egui::style::WidgetVisuals| (w.weak_bg_fill, w.fg_stroke.color);
    assert_eq!(colours(&v.widgets.inactive), (cga::CYAN, cga::BLACK));
    assert_eq!(colours(&v.widgets.hovered), (cga::BLACK, cga::CYAN));
    assert_eq!(v.window_shadow.offset, [16, 16]);
    assert_eq!(v.window_shadow.blur, 0);
    assert!(style.text_styles.values().all(|f| f.size == FONT_SIZE));
    // Whatever theme the system asks for.
    let light = drv.ctx.style_of(egui::Theme::Light);
    assert_eq!(light.visuals, style.visuals);
    assert_eq!(light.text_styles, style.text_styles);
}

/// The font is the VGA font on its 8×16 grid: every glyph a cell wide, a row a cell tall.
#[test]
fn text_sits_on_the_vga_cell_grid() {
    let mut drv = Driver::new(640.0, 400.0);
    drv.idle(&mut Parts { active: true }, 1);
    drv.ctx.fonts_mut(|f| {
        assert_eq!(f.row_height(&font()), CELL.y);
        for c in ['0', 'W', 'i', '.', '…', '—', '×', '░', '═', '☺'] {
            assert_eq!(f.glyph_width(&font(), c), CELL.x, "{c:?}");
        }
        let proportional = FontId::proportional(FONT_SIZE);
        assert_eq!(f.glyph_width(&proportional, 'i'), CELL.x, "both families");
    });
}

/// On screen, each part is in its colours: the title row's red block and cyan bar, the window's
/// blue body (a label sits on it) with its title in the frame colour (white when active, light
/// grey when not), the F-key row's digits light grey on black and labels black on cyan.
#[test]
fn the_parts_are_drawn_in_the_palette() {
    let p = PALETTE;
    for active in [true, false] {
        let mut drv = Driver::new(640.0, 400.0);
        let mut win = Parts { active };
        drv.idle(&mut win, 3);
        let frame_color = if active { cga::WHITE } else { cga::LIGHT_GRAY };
        assert_eq!(
            drv.colors_of("File Edit"),
            Some(vec![frame_color]),
            "active={active}"
        );
        assert_eq!(drv.colors_of(" sopen "), Some(vec![p.program_text]));
        assert_eq!(drv.colors_of("box:/srv/data"), Some(vec![p.title_text]));
        assert_eq!(drv.colors_of("Save   "), Some(vec![p.key_text]));
        assert_eq!(drv.colors_of("5"), Some(vec![p.key_digit]));
        let program = drv.find_text(" sopen ").unwrap();
        assert!(filled(&drv, program.center(), p.program_back));
        let place = drv.find_text("box:/srv/data").unwrap();
        assert!(filled(&drv, place.center(), p.title_back));
        let words = drv.find_text("plain words").unwrap();
        assert!(filled(&drv, words.center(), p.back));
        let label = drv.find_text("Save   ").unwrap();
        assert!(filled(
            &drv,
            label.left_center() + Vec2::new(2.0, 0.0),
            p.key_back
        ));
        let digit = drv.find_text("5").unwrap();
        assert!(filled(&drv, digit.center(), p.key_digit_back));
        // Rows are a cell tall: the title row at the top, the F-key row at the bottom.
        assert_eq!(program.height(), CELL.y);
        assert_eq!(label.bottom(), 400.0);
    }
}

/// Code page 437, as the hex window's ASCII column and the text viewer show bytes: printable
/// ASCII as itself, every other byte a glyph of its own that the VGA font has, one cell wide.
#[test]
fn cp437_gives_every_byte_a_glyph_of_the_font() {
    use ab_glyph::Font as _;
    let vga = ab_glyph::FontRef::try_from_slice(VGA).expect("the font parses");
    for b in 0x20..=0x7Eu8 {
        assert_eq!(cp437(b), char::from(b));
    }
    let known = [
        (0x00, ' '),
        (0x01, '☺'),
        (0x03, '♥'),
        (0x0A, '◙'),
        (0x1B, '←'),
        (0x1E, '▲'),
        (0x7F, '⌂'),
        (0x80, 'Ç'),
        (0x96, 'û'),
        (0xB0, '░'),
        (0xB3, '│'),
        (0xC9, '╔'),
        (0xCD, '═'),
        (0xDB, '█'),
        (0xE1, 'ß'),
        (0xFE, '■'),
        (0xFF, '\u{A0}'),
    ];
    for (b, c) in known {
        assert_eq!(cp437(b), c, "{b:#04X}");
    }
    let mut drv = Driver::new(100.0, 100.0);
    drv.idle(&mut Parts { active: true }, 1);
    let mut seen = std::collections::HashSet::new();
    for b in 0..=255u8 {
        let c = cp437(b);
        assert_ne!(vga.glyph_id(c).0, 0, "{b:#04X} {c:?} is in the font");
        let width = drv.ctx.fonts_mut(|f| f.glyph_width(&font(), c));
        assert_eq!(width, CELL.x, "{b:#04X} {c:?} is one cell");
        // Only 0x00 shares its glyph (a blank) with the space.
        if b != 0x00 {
            assert!(seen.insert(c), "{b:#04X} {c:?} is its own glyph");
        }
    }
    // The text viewer: control characters as their glyphs, everything else as itself.
    assert_eq!(shown_char('\u{1}'), '☺');
    assert_eq!(shown_char('\u{1b}'), '←');
    assert_eq!(shown_char('\u{7f}'), '⌂');
    for c in ['a', ' ', 'é', '한', '\u{A0}'] {
        assert_eq!(shown_char(c), c);
    }
}

/// The F-key rows label only keys that work there; a viewer's own keys join the browser's.
#[test]
fn the_f_key_tables() {
    let pairs =
        |keys: &[FKey]| -> Vec<(u8, &str)> { keys.iter().map(|k| (k.n, k.label)).collect() };
    assert_eq!(pairs(LIST_KEYS), [(3, "View"), (4, "Edit")]);
    assert_eq!(pairs(VIEWER_KEYS), [(3, "Exit"), (4, "Edit")]);
    assert_eq!(pairs(&[EXIT_KEY]), [(3, "Exit")]);
    assert_eq!(pairs(HEX_KEYS), [(5, "Save"), (6, "Goto"), (8, "Asc/hx")]);
    assert_eq!(
        pairs(&merged(VIEWER_KEYS, HEX_KEYS)),
        [
            (3, "Exit"),
            (4, "Edit"),
            (5, "Save"),
            (6, "Goto"),
            (8, "Asc/hx")
        ]
    );
    // A number in both keeps the first.
    assert_eq!(
        pairs(&merged(&[EXIT_KEY], LIST_KEYS)),
        [(3, "Exit"), (4, "Edit")]
    );
    // PC Shell's labels are six characters at most ("Disply", "Locate"); so are these.
    assert_eq!(pairs(TEXT_WRAPPED_KEYS), [(2, "Unwrap")]);
    assert_eq!(pairs(TEXT_UNWRAPPED_KEYS), [(2, "Wrap")]);
    let all = LIST_KEYS
        .iter()
        .chain(VIEWER_KEYS)
        .chain(HEX_KEYS)
        .chain(IMAGE_KEYS)
        .chain(MEDIA_KEYS);
    for key in all.chain(TEXT_WRAPPED_KEYS).chain(TEXT_UNWRAPPED_KEYS) {
        assert!(key.label.chars().count() <= 6, "{key:?}");
    }
}

/// Ten slots of whole cells across the row, each a digit then its label block; the tenth has
/// two digits and runs to the row's end.
#[test]
fn f_key_slots_split_the_row_in_ten() {
    for width in [480.0, 640.0, 1000.0, 1100.0] {
        let mut x = 0.0;
        for n in 1..=10u8 {
            let (digit, label) = key_slot(0.0, width, n);
            assert_eq!(digit.left(), x, "{width}: slot {n} follows the last");
            assert_eq!(digit.left() % CELL.x, 0.0, "on the cell grid");
            assert_eq!(digit.width(), CELL.x * if n == 10 { 2.0 } else { 1.0 });
            assert_eq!(label.left(), digit.right());
            assert!(label.width() >= 4.0 * CELL.x, "{width}: room for a label");
            x = label.right();
        }
        assert_eq!(x, width, "the last slot runs to the end");
    }
}

/// The clock reads as PC Shell's: seven characters, the hour without a leading zero.
#[test]
fn the_clock_reads_as_pc_shells() {
    assert_eq!(clock_text(3, 29), " 3:29am");
    assert_eq!(clock_text(0, 5), "12:05am");
    assert_eq!(clock_text(12, 0), "12:00pm");
    assert_eq!(clock_text(23, 59), "11:59pm");
    for h in 0..24 {
        assert_eq!(clock_text(h, 7).chars().count(), 7);
    }
}

/// The syntax theme reads on blue: every colour it gives is one of the sixteen, none is the
/// body's blue or dark grey, and keywords, strings, comments and types differ.
#[test]
fn the_syntax_theme_is_sixteen_colours_that_read_on_blue() {
    use syntect::highlighting::{HighlightIterator, HighlightState, Highlighter};
    use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};
    let all = [
        cga::BLACK,
        cga::BLUE,
        cga::GREEN,
        cga::CYAN,
        cga::RED,
        cga::MAGENTA,
        cga::BROWN,
        cga::LIGHT_GRAY,
        cga::DARK_GRAY,
        cga::LIGHT_BLUE,
        cga::LIGHT_GREEN,
        cga::LIGHT_CYAN,
        cga::LIGHT_RED,
        cga::LIGHT_MAGENTA,
        cga::YELLOW,
        cga::WHITE,
    ];
    let theme = syntax_theme();
    for item in &theme.scopes {
        let c = item.style.foreground.expect("a colour");
        let c = Color32::from_rgb(c.r, c.g, c.b);
        assert!(all.contains(&c), "{c:?}");
        assert_ne!(c, PALETTE.back);
        assert_ne!(c, cga::DARK_GRAY, "does not read on blue");
    }
    let set = SyntaxSet::load_defaults_nonewlines();
    let rust = set.find_syntax_by_extension("rs").unwrap();
    let line = "let s: String = \"x\"; // note 42";
    let ops = ParseState::new(rust).parse_line(line, &set).unwrap();
    let hl = Highlighter::new(theme);
    let mut state = HighlightState::new(&hl, ScopeStack::new());
    let runs: Vec<_> = HighlightIterator::new(&mut state, &ops, line, &hl).collect();
    let color_of = |part: &str| {
        let (style, _) = runs.iter().find(|(_, t)| t.contains(part)).expect(part);
        Color32::from_rgb(style.foreground.r, style.foreground.g, style.foreground.b)
    };
    assert_eq!(color_of("let"), cga::YELLOW);
    assert_eq!(color_of("String"), cga::WHITE);
    assert_eq!(color_of("x"), cga::LIGHT_CYAN);
    assert_eq!(color_of("note"), cga::CYAN);
}

/// A scroll bar `rows` cells tall, at `at`.
struct Bar {
    rows: f32,
    at: Option<f32>,
}

impl Window for Bar {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let column =
            Rect::from_min_size(Pos2::new(8.0, 16.0), Vec2::new(CELL.x, CELL.y * self.rows));
        scroll_bar(ui.painter(), column, self.at);
    }

    fn ready(&self) -> bool {
        true
    }
}

/// The thumb sits on the track between the arrows, a cell of it: at its top, at its end, and
/// not at all when no track cell fits (two rows are just the arrows).
#[test]
fn the_scroll_bar_thumb_stays_on_its_track() {
    let thumbs = |rows: f32, at: Option<f32>| -> Vec<Rect> {
        let mut drv = Driver::new(100.0, 400.0);
        drv.idle(&mut Bar { rows, at }, 1);
        drv.rects()
            .into_iter()
            .filter(|(_, f, _)| *f == PALETTE.thumb)
            .map(|(r, _, _)| r)
            .collect()
    };
    assert!(thumbs(2.0, Some(0.5)).is_empty(), "two rows: arrows only");
    assert!(thumbs(1.0, None).is_empty());
    let one = thumbs(3.0, Some(1.0));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].min, Pos2::new(8.0, 32.0), "the only track cell");
    let top = thumbs(10.0, None);
    assert_eq!(
        top[0].min.y, 32.0,
        "nothing to scroll: at the top of the track"
    );
    let end = thumbs(10.0, Some(1.0));
    assert_eq!(
        end[0].max.y,
        16.0 + 9.0 * CELL.y,
        "at the end: just above ↓"
    );
}

/// Code page 437 as Unicode publishes it, all 256 bytes: 0x20–0x7E and 0x80–0xFF from
/// `VENDORS/MICSFT/PC/CP437.TXT`, the screen glyphs of 0x01–0x1F and 0x7F from
/// `VENDORS/MISC/IBMGRAPH.TXT` (unicode.org/Public/MAPPINGS), and 0x00 a blank (the VGA font's
/// glyph 0 is empty). Generated from those files, not from `cp437`.
const CP437_UNICODE: [u32; 256] = [
    0x0020, 0x263A, 0x263B, 0x2665, 0x2666, 0x2663, 0x2660, 0x2022, 0x25D8, 0x25CB, 0x25D9, 0x2642,
    0x2640, 0x266A, 0x266B, 0x263C, 0x25BA, 0x25C4, 0x2195, 0x203C, 0x00B6, 0x00A7, 0x25AC, 0x21A8,
    0x2191, 0x2193, 0x2192, 0x2190, 0x221F, 0x2194, 0x25B2, 0x25BC, 0x0020, 0x0021, 0x0022, 0x0023,
    0x0024, 0x0025, 0x0026, 0x0027, 0x0028, 0x0029, 0x002A, 0x002B, 0x002C, 0x002D, 0x002E, 0x002F,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x003A, 0x003B,
    0x003C, 0x003D, 0x003E, 0x003F, 0x0040, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047,
    0x0048, 0x0049, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F, 0x0050, 0x0051, 0x0052, 0x0053,
    0x0054, 0x0055, 0x0056, 0x0057, 0x0058, 0x0059, 0x005A, 0x005B, 0x005C, 0x005D, 0x005E, 0x005F,
    0x0060, 0x0061, 0x0062, 0x0063, 0x0064, 0x0065, 0x0066, 0x0067, 0x0068, 0x0069, 0x006A, 0x006B,
    0x006C, 0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077,
    0x0078, 0x0079, 0x007A, 0x007B, 0x007C, 0x007D, 0x007E, 0x2302, 0x00C7, 0x00FC, 0x00E9, 0x00E2,
    0x00E4, 0x00E0, 0x00E5, 0x00E7, 0x00EA, 0x00EB, 0x00E8, 0x00EF, 0x00EE, 0x00EC, 0x00C4, 0x00C5,
    0x00C9, 0x00E6, 0x00C6, 0x00F4, 0x00F6, 0x00F2, 0x00FB, 0x00F9, 0x00FF, 0x00D6, 0x00DC, 0x00A2,
    0x00A3, 0x00A5, 0x20A7, 0x0192, 0x00E1, 0x00ED, 0x00F3, 0x00FA, 0x00F1, 0x00D1, 0x00AA, 0x00BA,
    0x00BF, 0x2310, 0x00AC, 0x00BD, 0x00BC, 0x00A1, 0x00AB, 0x00BB, 0x2591, 0x2592, 0x2593, 0x2502,
    0x2524, 0x2561, 0x2562, 0x2556, 0x2555, 0x2563, 0x2551, 0x2557, 0x255D, 0x255C, 0x255B, 0x2510,
    0x2514, 0x2534, 0x252C, 0x251C, 0x2500, 0x253C, 0x255E, 0x255F, 0x255A, 0x2554, 0x2569, 0x2566,
    0x2560, 0x2550, 0x256C, 0x2567, 0x2568, 0x2564, 0x2565, 0x2559, 0x2558, 0x2552, 0x2553, 0x256B,
    0x256A, 0x2518, 0x250C, 0x2588, 0x2584, 0x258C, 0x2590, 0x2580, 0x03B1, 0x00DF, 0x0393, 0x03C0,
    0x03A3, 0x03C3, 0x00B5, 0x03C4, 0x03A6, 0x0398, 0x03A9, 0x03B4, 0x221E, 0x03C6, 0x03B5, 0x2229,
    0x2261, 0x00B1, 0x2265, 0x2264, 0x2320, 0x2321, 0x00F7, 0x2248, 0x00B0, 0x2219, 0x00B7, 0x221A,
    0x207F, 0x00B2, 0x25A0, 0x00A0,
];

/// The whole table against Unicode's.
#[test]
fn cp437_is_unicodes_table() {
    for (b, &want) in CP437_UNICODE.iter().enumerate() {
        let b = b as u8;
        assert_eq!(u32::from(cp437(b)), want, "{b:#04X}");
    }
}

/// `fit_start` keeps a text's start: whole when it fits, else the longest start that fits
/// before "…" (and "…" alone when nothing does).
#[test]
fn fit_start_keeps_the_start() {
    let mut drv = Driver::new(100.0, 100.0);
    drv.idle(&mut Parts { active: true }, 1);
    let text = "12 Listed = 594,004 bytes";
    let f = font();
    assert_eq!(fit_start(&drv.ctx, text, &f, 400.0), text);
    // 8 cells: seven characters and "…".
    assert_eq!(fit_start(&drv.ctx, text, &f, 8.0 * CELL.x), "12 List…");
    assert_eq!(fit_start(&drv.ctx, text, &f, 8.5 * CELL.x), "12 List…");
    assert_eq!(fit_start(&drv.ctx, text, &f, 1.0 * CELL.x), "…");
    assert_eq!(fit_start(&drv.ctx, text, &f, 0.0), "…");
}

/// Lays out `names` (so a font provider is asked for their glyphs).
struct Names(&'static [&'static str]);

impl Window for Names {
    fn ui(&mut self, ui: &mut egui::Ui) {
        for name in self.0 {
            ui.label(egui::RichText::new(*name).font(font()));
        }
    }

    fn ready(&self) -> bool {
        true
    }
}

const WIDE_NAMES: &[&str] = &[
    "ab한",
    "가나다라마",
    "가나다라마.txt",
    "사진 폴더",
    "a한b글c",
    "日本語のファイル名",
    "😀🎉 party",
    "ab\u{3000}",
    "x",
];

/// `fit_start` / `fit_left` over every width from nothing to the whole text, for `names`: never
/// a panic; the whole text where it fits, else a start + "…" (an end after "…") that fits —
/// "…" alone when nothing more does.
fn sweep(drv: &Driver, names: &[&str]) {
    let f = font();
    let width = |t: &str| {
        drv.ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(t.to_string(), f.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    for name in names {
        let whole = width(name);
        let mut room = 0.0;
        while room <= whole + CELL.x {
            for (cut, start) in [
                (fit_start(&drv.ctx, name, &f, room), true),
                (fit_left(&drv.ctx, name, &f, room), false),
            ] {
                let case = format!("{name:?} in {room}: {cut:?}");
                if cut == *name {
                    assert!(whole <= room, "{case}: whole only where it fits");
                    continue;
                }
                assert!(whole > room, "{case}: cut only where it does not fit");
                if start {
                    let kept = cut.strip_suffix('…').expect(&case);
                    assert!(name.starts_with(kept), "{case}");
                } else {
                    let kept = cut.strip_prefix('…').expect(&case);
                    assert!(name.ends_with(kept), "{case}");
                }
                assert!(cut == "…" || width(&cut) <= room, "{case}: it fits");
            }
            room += 0.5;
        }
    }
}

/// The cut name's last character wider than "…" (an ideographic space here, Hangul in a
/// system font): `fit_start` keeps all but it — and never reads past the text.
#[test]
fn fit_start_keeps_all_but_a_wide_last_character() {
    let mut drv = Driver::new(100.0, 100.0);
    drv.idle(&mut Parts { active: true }, 1);
    let f = font();
    let room = drv.ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap("ab…".to_string(), f.clone(), Color32::WHITE)
            .size()
            .x
    });
    assert_eq!(fit_start(&drv.ctx, "ab\u{3000}", &f, room), "ab…");
    // An empty text is never cut, whatever the room; a room below nothing leaves "…".
    for room in [-8.0, 0.0, 8.0] {
        assert_eq!(fit_start(&drv.ctx, "", &f, room), "");
        assert_eq!(fit_left(&drv.ctx, "", &f, room), "");
    }
    assert_eq!(fit_start(&drv.ctx, "ab", &f, -8.0), "…");
    assert_eq!(fit_left(&drv.ctx, "ab", &f, -8.0), "…");
    sweep(&drv, WIDE_NAMES);
}

/// The same with the installed fonts a window gets (eframe's system font provider): Hangul,
/// CJK and emoji drawn by a system font, wider than a cell.
#[test]
fn fit_start_and_fit_left_with_system_fonts() {
    use egui_system_font_provider::SystemFontProvider;
    let mut drv = Driver::new(400.0, 400.0);
    drv.ctx
        .add_font_provider(std::sync::Arc::new(SystemFontProvider::new()));
    drv.idle(&mut Names(WIDE_NAMES), 3);
    let f = font();
    let han = drv.ctx.fonts_mut(|fonts| {
        fonts
            .has_glyph(&f, '한')
            .then(|| fonts.glyph_width(&f, '한'))
    });
    let Some(han) = han else {
        eprintln!("skipping: no installed font with Hangul");
        return;
    };
    let ellipsis = drv.ctx.fonts_mut(|fonts| fonts.glyph_width(&f, '…'));
    assert!(
        han > ellipsis,
        "the case: 한 ({han}) wider than … ({ellipsis})"
    );
    let room = drv.ctx.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap("ab…".to_string(), f.clone(), Color32::WHITE)
            .size()
            .x
    });
    assert_eq!(fit_start(&drv.ctx, "ab한", &f, room), "ab…");
    sweep(&drv, WIDE_NAMES);
}

/// The title row's clock asks for a repaint when the minute turns: a minute away at its start
/// (where the test kit's driver puts the time), a second away in its last second. Tests that
/// watch repaint delays must count their own requests (see `text::follows`), not delays.
#[test]
fn the_clock_asks_for_a_repaint_when_the_minute_turns() {
    use crate::gui::testkit::START_OF_A_MINUTE;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    let asked = |at: Option<i64>| {
        let mut drv = Driver::new(640.0, 400.0);
        if at.is_some() {
            set_test_now(at);
        }
        let delays = Arc::new(Mutex::new(Vec::new()));
        let seen = delays.clone();
        drv.ctx
            .set_repaint_observer(move |info| seen.lock().unwrap().push(info.delay));
        drv.idle(&mut Parts { active: true }, 1);
        let delays = delays.lock().unwrap().clone();
        delays
    };
    let start = asked(None);
    assert!(start.contains(&Duration::from_secs(60)), "{start:?}");
    assert!(!start.contains(&Duration::from_secs(1)), "{start:?}");
    let last = asked(Some(START_OF_A_MINUTE + 59));
    assert!(last.contains(&Duration::from_secs(1)), "{last:?}");
}
