//! The skin: every sopen window looks like PC Shell, the file manager of PC Tools Deluxe 5.5 / 6
//! (Central Point Software) — the VGA text font on a grid of 8×16 cells, light grey text on DOS
//! blue, windows framed in box lines (a white double line around the active one, a light grey
//! single line around the others) with their title in the top border, the white-on-light-red
//! highlight bar, the red program block on a cyan title row with the clock at its right end, the
//! F-key row (the digit on black, the label black on cyan, ten slots across), the scroll bar on a
//! window's right edge (a ░ track with a red thumb) and cyan dialogs with a see-through shadow two
//! cells right and one down. The logic never picks a colour, a font or a frame itself: they all
//! come from here — and so does the syntax theme of the text viewer.
//!
//! [`install`] puts the look into a window's context: the shell does it before the first frame
//! of every window, and the test kit's driver does the same, so tests lay out what the owner sees.
//!
//! The font is "PxPlus IBM VGA 8x16" from The Ultimate Oldschool PC Font Pack by VileR
//! (<https://int10h.org/oldschool-pc-fonts/>), CC BY-SA 4.0 (`assets/fonts/`). Its outlines are
//! the VGA glyphs' pixels, so it is crisp at 16 points: one font pixel is one point (one or two
//! screen pixels). It has the 256 CP437 glyphs at their Unicode points — box lines, `░`, the
//! ☺♥▲ control glyphs the hex window shows — and no Hangul: those come from the installed font
//! eframe's system font provider finds (see [`super::fonts`]).

use super::Status;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Stroke, Vec2};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

/// The sixteen CGA / EGA text-mode colours, by their canonical names.
pub mod cga {
    use eframe::egui::Color32;
    pub const BLACK: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
    pub const BLUE: Color32 = Color32::from_rgb(0x00, 0x00, 0xAA);
    pub const GREEN: Color32 = Color32::from_rgb(0x00, 0xAA, 0x00);
    pub const CYAN: Color32 = Color32::from_rgb(0x00, 0xAA, 0xAA);
    pub const RED: Color32 = Color32::from_rgb(0xAA, 0x00, 0x00);
    pub const MAGENTA: Color32 = Color32::from_rgb(0xAA, 0x00, 0xAA);
    pub const BROWN: Color32 = Color32::from_rgb(0xAA, 0x55, 0x00);
    pub const LIGHT_GRAY: Color32 = Color32::from_rgb(0xAA, 0xAA, 0xAA);
    pub const DARK_GRAY: Color32 = Color32::from_rgb(0x55, 0x55, 0x55);
    pub const LIGHT_BLUE: Color32 = Color32::from_rgb(0x55, 0x55, 0xFF);
    pub const LIGHT_GREEN: Color32 = Color32::from_rgb(0x55, 0xFF, 0x55);
    pub const LIGHT_CYAN: Color32 = Color32::from_rgb(0x55, 0xFF, 0xFF);
    pub const LIGHT_RED: Color32 = Color32::from_rgb(0xFF, 0x55, 0x55);
    pub const LIGHT_MAGENTA: Color32 = Color32::from_rgb(0xFF, 0x55, 0xFF);
    pub const YELLOW: Color32 = Color32::from_rgb(0xFF, 0xFF, 0x55);
    pub const WHITE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
}

use cga::*;

/// One character cell, in points: the VGA font's 8×16 at [`FONT_SIZE`].
pub const CELL: Vec2 = Vec2::new(8.0, 16.0);

/// Every text sopen shows is this size: the font's own pixel grid, so it is never blurred.
pub const FONT_SIZE: f32 = 16.0;

/// The VGA font (see the module docs for its source and licence).
const VGA: &[u8] = include_bytes!("../../assets/fonts/PxPlus_IBM_VGA_8x16.ttf");

/// The name the VGA font goes by in egui's font definitions.
pub const VGA_FONT: &str = "PxPlus IBM VGA 8x16";

/// An F-key the bottom row lists: its number and what it does here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FKey {
    pub n: u8,
    pub label: &'static str,
}

/// The file list's row in slice S1 (02 §7, 04 §2): only the keys that work, F9's label naming
/// the mode it switches to from the default (preview) mode. It is the browser key table's
/// (`browser::keys::row`, a test holds them equal). The browser draws the table's row, by its
/// mode; this constant is what the tests hold it to.
pub const LIST_KEYS: &[FKey] = &[
    HELP_KEY,
    FKey {
        n: 2,
        label: "Rename",
    },
    FKey {
        n: 3,
        label: "View",
    },
    FKey {
        n: 4,
        label: "Edit",
    },
    FKey {
        n: 5,
        label: "Copy",
    },
    FKey {
        n: 6,
        label: "Move",
    },
    FKey {
        n: 9,
        label: "Dual",
    },
];

/// The keys the browser adds while a viewer is open in it (the viewer's own come with them):
/// the key table's viewer row.
pub const VIEWER_KEYS: &[FKey] = &[
    HELP_KEY,
    FKey {
        n: 3,
        label: "Exit",
    },
    FKey {
        n: 4,
        label: "Edit",
    },
];

/// F1 everywhere: the key help of where the keyboard is (04 §2 slot 1, `gui::help`).
pub const HELP_KEY: FKey = FKey {
    n: 1,
    label: "Help",
};

/// F3 in a window of its own: it closes, as Cmd/Ctrl+W does (through the unsaved-changes prompt).
pub const EXIT_KEY: FKey = FKey {
    n: 3,
    label: "Exit",
};

/// F7 in the hex, image and media windows: Save As, the same as Alt+Cmd/Ctrl+S — the edited
/// result to a new local file (07 §7, `gui::save_as`). The image window's row lists it
/// ([`IMAGE_KEYS`]); the hex and media windows add it to theirs, with the help line, in the
/// change that wires the modal (ED4, ED6), so a slot never names a key that does nothing. The
/// text viewer is read-only and has none.
pub const SAVE_AS_KEY: FKey = FKey {
    n: 7,
    label: "SaveAs",
};

/// The hex window's own keys — PC Shell's Sector Edit row (`5Save`, `8Asc/hx`) with File Edit's
/// `6` for going somewhere: F5 is Cmd/Ctrl+S, F6 is Cmd/Ctrl+G, F8 is Tab (the typing column).
pub const HEX_KEYS: &[FKey] = &[
    FKey {
        n: 5,
        label: "Save",
    },
    FKey {
        n: 6,
        label: "Goto",
    },
    FKey {
        n: 8,
        label: "Asc/hx",
    },
];

/// The image window's own keys: F5 is Save, the same as Cmd/Ctrl+S (as in the hex window); F7
/// is Save As, as Alt+Cmd/Ctrl+S.
pub const IMAGE_KEYS: &[FKey] = &[
    FKey {
        n: 5,
        label: "Save",
    },
    SAVE_AS_KEY,
];

/// The media window's own key: F5 is Save, as in the image and hex windows (message 13 made
/// video and audio savable).
pub const MEDIA_KEYS: &[FKey] = &[FKey {
    n: 5,
    label: "Save",
}];

/// The text viewer's own key while lines wrap: F2 (or W) shows one row per line instead, as
/// Norton Commander's and Far's viewers name it.
pub const TEXT_WRAPPED_KEYS: &[FKey] = &[FKey {
    n: 2,
    label: "Unwrap",
}];

/// The text viewer's own key while lines do not wrap: F2 (or W) wraps them again.
pub const TEXT_UNWRAPPED_KEYS: &[FKey] = &[FKey {
    n: 2,
    label: "Wrap",
}];

/// The key of slot `n` (1–10): F1…F10.
pub fn fkey(n: u8) -> Option<egui::Key> {
    use egui::Key::*;
    [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10]
        .get(usize::from(n).wrapping_sub(1))
        .copied()
}

/// The slot of F1…F10 (1–10); `None` for any other key.
pub fn fkey_number(key: egui::Key) -> Option<u8> {
    (1..=10).find(|&n| fkey(n) == Some(key))
}

/// `keys` and `more` as one row, by number (a number in both keeps the first).
pub fn merged(keys: &[FKey], more: &[FKey]) -> Vec<FKey> {
    let mut all: Vec<FKey> = keys.to_vec();
    for key in more {
        if !all.iter().any(|k| k.n == key.n) {
            all.push(*key);
        }
    }
    all.sort_by_key(|k| k.n);
    all
}

/// The program's name, in the red block of the title row.
pub const PROGRAM: &str = "sopen";

/// What each part of a window is painted in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// A window's text (light grey on blue).
    pub text: Color32,
    /// Hidden (dot) files, line numbers, offsets, column heads, comments.
    pub dim: Color32,
    pub folder: Color32,
    pub error: Color32,
    pub warn: Color32,
    /// The highlight bar on the active panel, and its text.
    pub bar: Color32,
    pub bar_text: Color32,
    /// The highlight on the panel that is not active.
    pub bar_idle: Color32,
    /// A selected entry's text, under the bar too: PC Shell draws selected files yellow (04 §4).
    pub selected: Color32,
    /// The active window's double line, the others' single line.
    pub frame_active: Stroke,
    pub frame_idle: Stroke,
    /// A window's body.
    pub back: Color32,
    /// The title row, and the program's block at its left end.
    pub title_back: Color32,
    pub title_text: Color32,
    pub program_back: Color32,
    pub program_text: Color32,
    /// The F-key row: the digit on black, the label black on cyan.
    pub key_digit: Color32,
    pub key_digit_back: Color32,
    pub key_back: Color32,
    pub key_text: Color32,
    /// A dialog: black on cyan, its weak and error text, its frame and buttons' lines.
    pub dialog_back: Color32,
    pub dialog_text: Color32,
    pub dialog_weak: Color32,
    pub dialog_error: Color32,
    /// What is typed into a field (a black bar).
    pub field_text: Color32,
    /// The key column of the key help (F1), beside its words in `text`.
    pub help_key: Color32,
    /// The scroll bar: the track's cell, its `░` and arrows, the thumb.
    pub track_back: Color32,
    pub track: Color32,
    pub thumb: Color32,
    /// A dialog's shadow: the cells under it show through, darkened.
    pub shadow: Color32,
    /// Around a picture or a video frame.
    pub picture_back: Color32,
    /// A changed byte in the hex window, and the cell behind it.
    pub changed: Color32,
    pub changed_back: Color32,
    /// The media timeline: its track, the In–Out span, the waveform, the marks, the playhead.
    pub timeline: Color32,
    pub span: Color32,
    pub wave: Color32,
    pub mark_in: Color32,
    pub mark_out: Color32,
    pub playhead: Color32,
    /// The rectangle drawn on a picture, its handles and readout; what lies outside it.
    pub select: Color32,
    pub select_text: Color32,
    pub select_shade: Color32,
}

/// PC Shell 5.5 / 6's default colours (measured cell by cell on its screens).
pub const PALETTE: Palette = Palette {
    text: LIGHT_GRAY,
    dim: CYAN,
    folder: WHITE,
    error: LIGHT_RED,
    warn: YELLOW,
    bar: LIGHT_RED,
    bar_text: WHITE,
    bar_idle: BLACK,
    selected: YELLOW,
    frame_active: Stroke {
        width: 1.0,
        color: WHITE,
    },
    frame_idle: Stroke {
        width: 1.0,
        color: LIGHT_GRAY,
    },
    back: BLUE,
    title_back: CYAN,
    title_text: BLACK,
    program_back: RED,
    program_text: LIGHT_GRAY,
    key_digit: LIGHT_GRAY,
    key_digit_back: BLACK,
    key_back: CYAN,
    key_text: BLACK,
    dialog_back: CYAN,
    dialog_text: BLACK,
    dialog_weak: BLUE,
    dialog_error: RED,
    field_text: WHITE,
    help_key: WHITE,
    track_back: BLACK,
    track: LIGHT_GRAY,
    thumb: RED,
    shadow: Color32::from_rgba_premultiplied(0, 0, 0, 170),
    picture_back: BLACK,
    changed: YELLOW,
    changed_back: BLACK,
    timeline: BLACK,
    span: Color32::from_rgba_premultiplied(0, 60, 60, 60),
    wave: LIGHT_GREEN,
    mark_in: LIGHT_GREEN,
    mark_out: LIGHT_RED,
    playhead: WHITE,
    select: YELLOW,
    select_text: BLACK,
    select_shade: Color32::from_rgba_premultiplied(0, 0, 0, 110),
};

impl Palette {
    /// The palette of the window whose visuals are `v` — the skin's, which [`install`] put
    /// there (the same colours, see the tests).
    pub fn of(_v: &egui::Visuals) -> Self {
        PALETTE
    }
}

/// The font of everything sopen writes: the VGA font at [`FONT_SIZE`].
pub fn font() -> FontId {
    FontId::monospace(FONT_SIZE)
}

/// The font of the list and the text viewer.
pub fn mono(ui: &egui::Ui) -> FontId {
    egui::TextStyle::Monospace.resolve(ui.style())
}

/// The fonts: the VGA font first in both families, then egui's own for anything it lacks; the
/// system font provider eframe adds comes after (Hangul).
pub fn fonts() -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    let vga = egui::FontData::from_static(VGA).tweak(egui::FontTweak {
        // Pixel outlines on whole points: nothing to grid-fit, no sub-pixel positions.
        hinting: Some(false),
        subpixel_binning: Some(false),
        ..Default::default()
    });
    defs.font_data.insert(VGA_FONT.to_string(), Arc::new(vga));
    for family in [egui::FontFamily::Monospace, egui::FontFamily::Proportional] {
        defs.families
            .entry(family)
            .or_default()
            .insert(0, VGA_FONT.to_string());
    }
    defs
}

/// A shadow `x` points right and `y` points down, not blurred.
fn shadow(x: i8, y: i8) -> egui::Shadow {
    let c = PALETTE.shadow;
    egui::Shadow {
        offset: [x, y],
        blur: 0,
        spread: 0,
        color: c,
    }
}

/// egui's visuals in the palette: flat cyan buttons with black text (inverted under the pointer,
/// the highlight bar while pressed), black fields, no rounding, no soft shadows.
pub fn visuals() -> egui::Visuals {
    let p = PALETTE;
    let mut v = egui::Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = None;
    v.weak_text_color = Some(p.dim);
    v.hyperlink_color = LIGHT_CYAN;
    v.faint_bg_color = p.back;
    v.extreme_bg_color = BLACK;
    v.text_edit_bg_color = Some(BLACK);
    v.code_bg_color = BLACK;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.error;
    v.window_corner_radius = egui::CornerRadius::ZERO;
    v.menu_corner_radius = egui::CornerRadius::ZERO;
    v.window_shadow = shadow(2 * CELL.x as i8, CELL.y as i8);
    v.popup_shadow = shadow(CELL.x as i8, CELL.y as i8);
    v.window_fill = p.back;
    v.window_stroke = p.frame_idle;
    v.panel_fill = p.back;
    v.selection.bg_fill = p.bar;
    v.selection.stroke = Stroke::new(1.0, p.bar_text);
    v.text_cursor.stroke = Stroke::new(2.0, WHITE);
    v.slider_trailing_fill = true;
    v.striped = false;
    v.button_frame = true;
    let flat = |fill: Color32, text: Color32, line: Stroke| egui::style::WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: fill,
        bg_stroke: line,
        corner_radius: egui::CornerRadius::ZERO,
        fg_stroke: Stroke::new(1.0, text),
        expansion: 0.0,
    };
    // No lines between panels or between a bar's items: a text screen has none.
    v.widgets.noninteractive = flat(p.back, p.text, Stroke::NONE);
    v.widgets.inactive = flat(CYAN, BLACK, Stroke::NONE);
    v.widgets.hovered = flat(BLACK, CYAN, Stroke::NONE);
    v.widgets.active = flat(p.bar, p.bar_text, Stroke::NONE);
    v.widgets.open = flat(p.bar, p.bar_text, Stroke::NONE);
    v
}

/// The skin's sizes on top of egui's: every text style at [`FONT_SIZE`], buttons one cell tall
/// with half a cell between controls.
pub fn style(s: &mut egui::Style) {
    s.visuals = visuals();
    for font in s.text_styles.values_mut() {
        font.size = FONT_SIZE;
    }
    s.spacing.item_spacing.x = CELL.x / 2.0;
    s.spacing.button_padding = Vec2::new(2.0, 0.0);
    s.spacing.interact_size.y = CELL.y;
    s.spacing.slider_width = 60.0;
}

/// Puts the skin into a window's context: the fonts, the dark theme whatever the system's, the
/// style of both themes — and the F-key row's input and F1's help screen, the shell's input
/// plugin (`gui::FKeyInput`: a clicked slot and Cmd/Ctrl+1…0 become the F-key, 04 §6.1), so
/// every window and the test kit's driver run the same hook before every pass.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(style);
    ctx.add_plugin(super::FKeyInput::default());
}

/// A one-line text field: a black bar, what is typed in white (also inside a dialog, whose text
/// is black).
pub fn field(text: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(text).text_color(PALETTE.field_text)
}

/// One of a set of choices: a button that stays framed (cyan) when not chosen and turns into the
/// highlight bar when it is — never bare text on the blue body.
pub fn choice(ui: &mut egui::Ui, chosen: bool, label: &str) -> egui::Response {
    ui.add(egui::Button::selectable(chosen, label).frame_when_inactive(true))
}

/// Makes `ui` a dialog's inside: black text on cyan, framed buttons. The text colour is the
/// plain text's, not an override: a button keeps its own state's colours (the one under the
/// pointer is inverted, cyan on black).
pub fn dialog_visuals(ui: &mut egui::Ui) {
    let p = PALETTE;
    let v = ui.visuals_mut();
    v.widgets.noninteractive.fg_stroke.color = p.dialog_text;
    v.weak_text_color = Some(p.dialog_weak);
    v.error_fg_color = p.dialog_error;
    v.warn_fg_color = p.dialog_text;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.dialog_text);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.dialog_text);
}

/// The frame of a dialog box drawn by egui (a modal prompt): cyan, its double line a cell in
/// from the edge (painted by [`dialog_lines`]), the shadow two cells right and one down.
pub fn dialog_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(PALETTE.dialog_back)
        .inner_margin(egui::Margin::symmetric(
            (2.0 * CELL.x) as i8,
            (1.5 * CELL.y) as i8,
        ))
        .shadow(shadow(2 * CELL.x as i8, CELL.y as i8))
}

/// A dialog box's double line, half a cell inside `outer` (the box), with `title` centred in its
/// top line.
pub fn dialog_lines(painter: &egui::Painter, outer: Rect, title: &str) {
    let p = PALETTE;
    let line = Rect::from_min_max(outer.min + CELL / 2.0, outer.max - CELL / 2.0);
    double_line(painter, line, p.dialog_text);
    title_in_border(painter, outer, title, p.dialog_text, p.dialog_back);
}

/// A modal prompt in the dialog look: `title` in its frame, `add` lays out its inside (black on
/// cyan). Returns egui's modal response.
pub fn dialog<R>(
    ctx: &egui::Context,
    id: egui::Id,
    title: &str,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::ModalResponse<R> {
    egui::Modal::new(id)
        // It pops up at once, as a text screen's box does (no fade).
        .area(egui::Modal::default_area(id).fade_in(false))
        .frame(dialog_frame())
        .backdrop_color(Color32::TRANSPARENT)
        .show(ctx, |ui| {
            dialog_visuals(ui);
            let inner = add(ui);
            let m = dialog_frame().inner_margin;
            let content = ui.min_rect();
            let outer = Rect::from_min_max(
                content.min - Vec2::new(f32::from(m.left), f32::from(m.top)),
                content.max + Vec2::new(f32::from(m.right), f32::from(m.bottom)),
            );
            dialog_lines(&ui.painter().with_clip_rect(outer), outer, title);
            inner
        })
}

/// The text viewer's syntax theme: sixteen colours that read on DOS blue, in the manner of the
/// Borland editors of the time — keywords yellow, strings light cyan, numbers light green, types
/// and functions white, comments cyan (dark grey does not read on blue), the rest light grey.
pub fn syntax_theme() -> &'static syntect::highlighting::Theme {
    use std::str::FromStr as _;
    use syntect::highlighting::{
        Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
    };
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let rgb = |c: Color32| Color {
            r: c.r(),
            g: c.g(),
            b: c.b(),
            a: 0xFF,
        };
        let rules: &[(&str, Color32)] = &[
            ("comment, punctuation.definition.comment", CYAN),
            (
                "keyword, storage, variable.language, entity.name.tag",
                YELLOW,
            ),
            ("keyword.operator, punctuation", LIGHT_GRAY),
            (
                "string, constant.character, punctuation.definition.string",
                LIGHT_CYAN,
            ),
            ("constant.numeric", LIGHT_GREEN),
            (
                "constant.language, constant.other, constant.character.escape",
                LIGHT_MAGENTA,
            ),
            (
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
                 entity.name.trait, entity.name.union, support.type, support.class, \
                 storage.type.numeric, entity.other.inherited-class, entity.name.function, \
                 support.function",
                WHITE,
            ),
            (
                "support.macro, entity.name.macro, meta.annotation, meta.attribute, \
                 entity.other.attribute-name",
                LIGHT_MAGENTA,
            ),
            ("markup.heading, markup.changed", YELLOW),
            ("markup.bold", WHITE),
            (
                "markup.italic, markup.underline.link, markup.link",
                LIGHT_CYAN,
            ),
            ("markup.raw, markup.inserted", LIGHT_GREEN),
            ("markup.deleted, invalid", LIGHT_RED),
            ("meta.diff.header, meta.diff.range", CYAN),
        ];
        Theme {
            name: Some("PC Tools".to_string()),
            author: None,
            settings: ThemeSettings {
                foreground: Some(rgb(PALETTE.text)),
                background: Some(rgb(PALETTE.back)),
                caret: Some(rgb(WHITE)),
                selection: Some(rgb(PALETTE.bar)),
                ..Default::default()
            },
            scopes: rules
                .iter()
                .map(|(scope, color)| ThemeItem {
                    scope: ScopeSelectors::from_str(scope).expect("a scope selector"),
                    style: StyleModifier {
                        foreground: Some(rgb(*color)),
                        background: None,
                        font_style: None,
                    },
                })
                .collect(),
        }
    })
}

/// The glyphs of code page 437 for the bytes that are not printable ASCII, at their Unicode
/// points (the VGA font has them all): 0x00–0x1F, then 0x7F, then 0x80–0xFF.
const CP437_LOW: [char; 32] = [
    ' ', '☺', '☻', '♥', '♦', '♣', '♠', '•', '◘', '○', '◙', '♂', '♀', '♪', '♫', '☼', //
    '►', '◄', '↕', '‼', '¶', '§', '▬', '↨', '↑', '↓', '→', '←', '∟', '↔', '▲', '▼',
];
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', //
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ', //
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', //
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐', //
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', //
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀', //
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', //
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{A0}',
];

/// The glyph a byte shows as, as on a DOS screen (code page 437): printable ASCII as itself,
/// the control bytes as ☺ ♥ ▲ …, 0x7F as ⌂, the upper half as Ç ü é … ░ ═ ╬ … α ß; 0x00 is a
/// blank cell (the VGA font's glyph 0 is empty), 0xFF a no-break space.
pub fn cp437(b: u8) -> char {
    match b {
        0x00..=0x1F => CP437_LOW[usize::from(b)],
        0x7F => '⌂',
        0x80..=0xFF => CP437_HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

/// A character of text as the text viewer shows it: a control character (a tab is already
/// spaces) as its code page 437 glyph, as DOS printed it; anything else as itself.
pub fn shown_char(c: char) -> char {
    match u8::try_from(u32::from(c)) {
        Ok(b) if b < 0x20 || b == 0x7F => cp437(b),
        _ => c,
    }
}

/// The local time of `secs` since the epoch, broken down.
pub fn local_tm(secs: i64) -> Option<libc::tm> {
    let secs = secs as libc::time_t;
    // SAFETY: an all-zero tm is a valid value to be overwritten.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; localtime_r is the thread-safe variant.
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return None;
    }
    Some(tm)
}

/// The clock as PC Shell's title row shows it: ` 3:29am`, `11:05pm` — always seven characters.
pub fn clock_text(hour: i32, minute: i32) -> String {
    let (h, half) = match hour {
        0 => (12, "am"),
        1..=11 => (hour, "am"),
        12 => (12, "pm"),
        _ => (hour - 12, "pm"),
    };
    format!("{h:>2}:{minute:02}{half}")
}

/// Seconds since the epoch, now (in tests, the time [`set_test_now`] gave this thread, if any).
fn now_secs() -> i64 {
    #[cfg(test)]
    if let Some(now) = TEST_NOW.with(std::cell::Cell::get) {
        return now;
    }
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
thread_local! {
    static TEST_NOW: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// Tests: the clock on the title row reads `now` (seconds since the epoch) on this thread —
/// the test kit's driver sets the start of a minute, so the clock asks to be repainted only a
/// minute later, never in the second a test watches (`None`: the real time).
#[cfg(test)]
pub(crate) fn set_test_now(now: Option<i64>) {
    TEST_NOW.with(|t| t.set(now));
}

/// A row one cell tall across `ui`.
fn row(ui: &mut egui::Ui) -> Rect {
    let width = ui.available_width();
    ui.allocate_exact_size(Vec2::new(width, CELL.y), egui::Sense::hover())
        .0
}

/// The top row: the program's name in its red block, then where the browser is, black on cyan,
/// and the clock at the right end after a `│`.
pub fn title_row(ui: &mut egui::Ui, place: &str) {
    let p = PALETTE;
    let rect = row(ui);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, p.title_back);
    let program = format!(" {PROGRAM} ");
    let block = Rect::from_min_size(
        rect.min,
        Vec2::new(CELL.x * program.chars().count() as f32, CELL.y),
    );
    painter.rect_filled(block, 0.0, p.program_back);
    painter.text(
        block.min,
        egui::Align2::LEFT_TOP,
        program,
        font(),
        p.program_text,
    );
    let now = now_secs();
    let clock = local_tm(now).map_or(String::new(), |tm| {
        format!("│{}", clock_text(tm.tm_hour, tm.tm_min))
    });
    let clock_x = rect.right() - CELL.x * (clock.chars().count() + 1) as f32;
    painter.text(
        Pos2::new(clock_x, rect.top()),
        egui::Align2::LEFT_TOP,
        clock,
        font(),
        p.title_text,
    );
    let x = block.right() + CELL.x;
    let shown = fit_left(ui.ctx(), place, &font(), (clock_x - CELL.x - x).max(0.0));
    painter.text(
        Pos2::new(x, rect.top()),
        egui::Align2::LEFT_TOP,
        &shown,
        font(),
        p.title_text,
    );
    if shown != place {
        ui.interact(rect, ui.scope_id().with("title"), egui::Sense::hover())
            .on_hover_text(place);
    }
    // The clock turns over at the next minute.
    let rest = 60 - now.rem_euclid(60) as u64;
    ui.ctx().request_repaint_after(Duration::from_secs(rest));
}

/// The title row as a panel at the top of `ui`.
pub fn title_panel(ui: &mut egui::Ui, id: &'static str, place: &str) {
    egui::Panel::top(id)
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .resizable(false)
        .exact_size(CELL.y)
        .show(ui, |ui| title_row(ui, place));
}

/// A one-line bar as a panel: a cell tall and a little, whatever it holds — so it has its height
/// from the first frame (egui places a panel it has not measured yet by a default height, and
/// what is below would move one frame later).
pub fn bar_panel(panel: egui::Panel) -> egui::Panel {
    panel
        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(CELL.x as i8, 2)))
        .show_separator_line(false)
        .resizable(false)
        .exact_size(CELL.y + 4.0)
}

/// The status row: progress, results, what the keys do.
pub fn status_row(ui: &mut egui::Ui, status: &Status) {
    ui.horizontal(|ui| status.show(ui));
}

/// Where the F-key row puts key `n` (1–10) in a row `width` points wide from `left`: its digit
/// and its label block. Ten slots of whole cells; the last one runs to the end.
pub fn key_slot(left: f32, width: f32, n: u8) -> (Rect, Rect) {
    let slot = ((width / 10.0 / CELL.x).floor() * CELL.x).max(3.0 * CELL.x);
    let x = left + slot * f32::from(n.saturating_sub(1));
    let digits = if n >= 10 { 2.0 } else { 1.0 };
    let end = if n >= 10 { left + width } else { x + slot };
    let digit = Rect::from_min_size(Pos2::new(x, 0.0), Vec2::new(CELL.x * digits, CELL.y));
    let label = Rect::from_min_max(
        Pos2::new(digit.right(), 0.0),
        Pos2::new(end.max(digit.right()), CELL.y),
    );
    (digit, label)
}

/// The bottom row: ten slots, each the key's digit on black and its label black on cyan; a
/// slot whose key does nothing here has an empty label, as PC Shell shows it. A click on a
/// slot with a label — its digit or its label — is that F-key pressed (04 §6.1): the shell's
/// input hook (`gui::FKeyInput`) hands it to the window's next pass as the plain key, so it does
/// exactly what the key does there.
pub fn fkey_row(ui: &mut egui::Ui, keys: &[FKey]) {
    let p = PALETTE;
    let rect = row(ui);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, p.key_digit_back);
    for n in 1..=10u8 {
        let (digit, label) = key_slot(rect.left(), rect.width(), n);
        let (digit, label) = (
            digit.translate(Vec2::new(0.0, rect.top())),
            label.translate(Vec2::new(0.0, rect.top())),
        );
        if digit.left() >= rect.right() {
            break;
        }
        painter.text(
            digit.min,
            egui::Align2::LEFT_TOP,
            n.to_string(),
            font(),
            p.key_digit,
        );
        painter.rect_filled(label, 0.0, p.key_back);
        let Some(key) = keys.iter().find(|k| k.n == n && !k.label.is_empty()) else {
            continue;
        };
        painter.with_clip_rect(label.intersect(rect)).text(
            label.min,
            egui::Align2::LEFT_TOP,
            format!("{:<7}", key.label),
            font(),
            p.key_text,
        );
        // Not focusable: a click on the row leaves the keyboard where it was.
        let slot = digit.union(label).intersect(rect);
        if ui
            .interact(
                slot,
                ui.scope_id().with(("sopen-fkey", n)),
                egui::Sense::CLICK,
            )
            .clicked()
        {
            press_fkey(ui.ctx(), n);
        }
    }
}

/// A slot clicked, for the window's next pass.
#[derive(Debug, Clone, Copy, Default)]
struct Clicked(u8);

fn clicked_id() -> egui::Id {
    egui::Id::unique("sopen-fkey-clicked")
}

/// F`n` is pressed in the window's next pass (a click on its slot).
pub fn press_fkey(ctx: &egui::Context, n: u8) {
    ctx.data_mut(|d| d.insert_temp(clicked_id(), Clicked(n)));
    ctx.request_repaint();
}

/// The slot clicked since the last pass, taken (the shell's input hook).
pub(crate) fn take_clicked(ctx: &egui::Context) -> Option<u8> {
    ctx.data_mut(|d| d.remove_temp::<Clicked>(clicked_id()))
        .map(|c| c.0)
}

/// The F-key row as a panel at the bottom of `ui`.
pub fn key_panel(ui: &mut egui::Ui, id: &'static str, keys: &[FKey]) {
    egui::Panel::bottom(id)
        .frame(egui::Frame::NONE)
        .show_separator_line(false)
        .resizable(false)
        .exact_size(CELL.y)
        .show(ui, |ui| fkey_row(ui, keys));
}

/// The four lines of `r`.
fn outline(painter: &egui::Painter, r: Rect, color: Color32) {
    let s = Stroke::new(1.0, color);
    painter.line_segment([r.left_top(), r.right_top()], s);
    painter.line_segment([r.left_bottom(), r.right_bottom()], s);
    painter.line_segment([r.left_top(), r.left_bottom()], s);
    painter.line_segment([r.right_top(), r.right_bottom()], s);
}

/// A double box line along `r` (one line a point outside it, one a point inside), as ╔═╗║╚╝.
fn double_line(painter: &egui::Painter, r: Rect, color: Color32) {
    outline(painter, r.expand(1.0), color);
    outline(painter, r.shrink(1.0), color);
}

/// `title` as a window `width` points wide shows it in its top border: whole, or its end after
/// "…" (a path keeps the folder it ends in).
pub fn frame_title(ctx: &egui::Context, title: &str, width: f32) -> String {
    fit_left(ctx, title, &font(), (width - 6.0 * CELL.x).max(0.0))
}

/// `title` centred in the top row of `outer`, a blank cell either side, over the border line.
fn title_in_border(
    painter: &egui::Painter,
    outer: Rect,
    title: &str,
    color: Color32,
    back: Color32,
) {
    let shown = frame_title(painter.ctx(), title, outer.width());
    if shown.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(shown, font(), color);
    let w = galley.size().x;
    let x = (outer.center().x - w / 2.0).round();
    painter.rect_filled(
        Rect::from_min_size(
            Pos2::new(x - CELL.x, outer.top()),
            Vec2::new(w + 2.0 * CELL.x, CELL.y),
        ),
        0.0,
        back,
    );
    painter
        .with_clip_rect(outer)
        .galley(Pos2::new(x, outer.top()), galley, color);
}

/// A window: `rect` filled with the body colour, its border a cell wide all round — a double
/// line when it is the `active` one, a single line otherwise — with `title` centred in the top
/// border. Returns the room inside the border.
pub fn frame(painter: &egui::Painter, rect: Rect, title: &str, active: bool) -> Rect {
    let p = PALETTE;
    let rect = Rect::from_min_max(rect.min.round(), rect.max.round());
    painter.rect_filled(rect, 0.0, p.back);
    let stroke = if active { p.frame_active } else { p.frame_idle };
    let line = Rect::from_min_max(rect.min + CELL / 2.0, rect.max - CELL / 2.0);
    if active {
        double_line(painter, line, stroke.color);
    } else {
        outline(painter, line, stroke.color);
    }
    title_in_border(painter, rect, title, stroke.color, p.back);
    inside(rect)
}

/// The room inside a window [`frame`] draws in `rect`.
pub fn inside(rect: Rect) -> Rect {
    let rect = Rect::from_min_max(rect.min.round(), rect.max.round());
    Rect::from_min_max(rect.min + CELL, (rect.max - CELL).max(rect.min + CELL))
}

/// A file panel's frame (see [`frame`]).
pub fn panel_frame(painter: &egui::Painter, rect: Rect, title: &str, active: bool) -> Rect {
    frame(painter, rect, title, active)
}

/// The scroll bar on a window's right border, beside the rows: `column` is that border's cell
/// column from the first row to the last. ↑ and ↓ at its ends, the `░` track between them, and
/// the red thumb one cell tall where `at` (0 at the top, 1 at the end) puts it — at the top when
/// there is nothing to scroll (`None`), and nowhere when no track cell fits between the arrows.
/// Returns the track (between the arrows).
pub fn scroll_bar(painter: &egui::Painter, column: Rect, at: Option<f32>) -> Rect {
    let p = PALETTE;
    painter.rect_filled(column, 0.0, p.track_back);
    let rows = (column.height() / CELL.y).floor() as usize;
    if rows < 2 {
        return column;
    }
    let track = Rect::from_min_max(
        column.min + Vec2::new(0.0, CELL.y),
        Pos2::new(column.right(), column.top() + CELL.y * (rows - 1) as f32),
    );
    let mut text = String::from("↑");
    for _ in 1..rows - 1 {
        text.push_str("\n░");
    }
    text.push_str("\n↓");
    painter.text(column.min, egui::Align2::LEFT_TOP, text, font(), p.track);
    if rows < 3 {
        // The arrows alone: no cell for a thumb.
        return track;
    }
    let room = (track.height() - CELL.y).max(0.0);
    let y = (track.top() + room * at.unwrap_or(0.0).clamp(0.0, 1.0)).round();
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(column.left(), y), Vec2::new(CELL.x, CELL.y)),
        0.0,
        p.thumb,
    );
    track
}

/// `text` as it fits in `room` points of `font`: whole, or its longest end that fits after "…"
/// — a path keeps the folder it ends in. Measured as egui lays it out (wide glyphs too).
pub fn fit_left(ctx: &egui::Context, text: &str, font: &FontId, room: f32) -> String {
    let width = |t: &str| {
        ctx.fonts_mut(|f| {
            f.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    // Nothing to cut in an empty text (whatever the room, a negative one too).
    if text.is_empty() || width(text) <= room {
        return text.to_string();
    }
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    // The fewest characters dropped from the front for "…" and the rest to fit.
    let (mut lo, mut hi) = (1, starts.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if width(&format!("…{}", &text[starts[mid]..])) <= room {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    match starts.get(lo) {
        Some(&at) => format!("…{}", &text[at..]),
        None => "…".to_string(),
    }
}

/// `text` as it fits in `room` points of `font`: whole, or its longest start that fits before
/// "…" — a count keeps its beginning. Measured as egui lays it out.
pub fn fit_start(ctx: &egui::Context, text: &str, font: &FontId, room: f32) -> String {
    let width = |t: &str| {
        ctx.fonts_mut(|f| {
            f.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    // Nothing to cut in an empty text (whatever the room, a negative one too).
    if text.is_empty() || width(text) <= room {
        return text.to_string();
    }
    // Where each character starts: keeping `k` characters is `text[..ends[k]]`, for `k` up to
    // all but the last (the whole text was measured above). Not empty: returned above.
    let ends: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    // The most characters kept from the front with "…" after them.
    let (mut lo, mut hi) = (0, ends.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&format!("{}…", &text[..ends[mid]])) <= room {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    format!("{}…", &text[..ends[lo]])
}

/// The highlight bar behind a row.
pub fn bar(painter: &egui::Painter, row: Rect, active: bool) {
    let p = PALETTE;
    painter.rect_filled(row, 0.0, if active { p.bar } else { p.bar_idle });
}

/// A line across a window at `y`, from its left border to its right one (above its footer).
pub fn rule(painter: &egui::Painter, left: f32, right: f32, y: f32, active: bool) {
    let p = PALETTE;
    let stroke = if active { p.frame_active } else { p.frame_idle };
    painter.hline(left..=right, y.round(), stroke);
}

#[cfg(test)]
mod tests;
