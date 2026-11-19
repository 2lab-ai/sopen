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
    for key in LIST_KEYS.iter().chain(VIEWER_KEYS).chain(HEX_KEYS) {
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
