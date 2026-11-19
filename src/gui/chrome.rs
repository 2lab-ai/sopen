//! The file browser's chrome: the title row, the status row and the F-key row around it, a
//! panel's frame with its title in the top border, the highlight bar — and every colour the
//! browser and its text viewer use (the syntax theme too). The logic never picks a colour or
//! draws a frame itself, so a skin (the PC Tools look) replaces this module alone.
//!
//! Where things sit follows PC Shell: the program's name on the top row, the path in the panel
//! frame's top border, `N listed = N bytes` at the panel's foot, the F-keys on the bottom row.

use super::Status;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Stroke, Vec2};

/// An F-key the bottom row lists: its number and what it does here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FKey {
    pub n: u8,
    pub label: &'static str,
}

/// The keys that work on the file list.
pub const LIST_KEYS: &[FKey] = &[
    FKey {
        n: 3,
        label: "View",
    },
    FKey {
        n: 4,
        label: "Edit",
    },
];

/// The keys that work while a viewer is open.
pub const VIEWER_KEYS: &[FKey] = &[
    FKey {
        n: 3,
        label: "Exit",
    },
    FKey {
        n: 4,
        label: "Edit",
    },
];

/// The program's name on the title row.
pub const PROGRAM: &str = "sopen";

/// The colours, from the egui theme in use.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub text: Color32,
    /// Hidden (dot) files, line numbers.
    pub dim: Color32,
    pub folder: Color32,
    pub error: Color32,
    /// The highlight bar on the active panel, and its text.
    pub bar: Color32,
    pub bar_text: Color32,
    /// The highlight on the panel that is not active.
    pub bar_idle: Color32,
    pub frame_active: Stroke,
    pub frame_idle: Stroke,
    pub back: Color32,
    pub title_back: Color32,
    pub title_text: Color32,
    pub key_digit: Color32,
    pub key_back: Color32,
    pub key_text: Color32,
}

impl Palette {
    pub fn of(v: &egui::Visuals) -> Self {
        Self {
            text: v.text_color(),
            dim: v.weak_text_color(),
            folder: v.strong_text_color(),
            error: v.error_fg_color,
            bar: v.selection.bg_fill,
            bar_text: v.strong_text_color(),
            bar_idle: v.widgets.inactive.bg_fill,
            frame_active: Stroke::new(2.0, v.selection.stroke.color),
            frame_idle: Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
            back: v.extreme_bg_color,
            title_back: v.selection.bg_fill,
            title_text: v.strong_text_color(),
            key_digit: v.text_color(),
            key_back: v.widgets.inactive.bg_fill,
            key_text: v.strong_text_color(),
        }
    }
}

/// The text viewer's syntax theme (a theme bundled with syntect).
pub fn syntax_theme(dark: bool) -> &'static str {
    if dark {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    }
}

/// The font of the list and the text viewer.
pub fn mono(ui: &egui::Ui) -> FontId {
    egui::TextStyle::Monospace.resolve(ui.style())
}

/// The top row: the program's name in its block, then where the browser is.
pub fn title_row(ui: &mut egui::Ui, place: &str) {
    let p = Palette::of(ui.visuals());
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(" {PROGRAM} "))
                .monospace()
                .color(p.title_text)
                .background_color(p.title_back),
        );
        super::one_line(ui, place, p.text);
    });
}

/// The status row: progress, results, what the keys do.
pub fn status_row(ui: &mut egui::Ui, status: &Status) {
    ui.horizontal(|ui| status.show(ui));
}

/// The bottom row: each working F-key as its number and its label.
pub fn fkey_row(ui: &mut egui::Ui, keys: &[FKey]) {
    let p = Palette::of(ui.visuals());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for key in keys {
            ui.label(
                egui::RichText::new(key.n.to_string())
                    .monospace()
                    .color(p.key_digit),
            );
            ui.label(
                egui::RichText::new(format!("{:<7}", key.label))
                    .monospace()
                    .color(p.key_text)
                    .background_color(p.key_back),
            );
            ui.add_space(8.0);
        }
    });
}

/// A panel's frame — double width when it is the active one — with `title` centred in its top
/// border. Returns the room inside.
pub fn panel_frame(painter: &egui::Painter, rect: Rect, title: &str, active: bool) -> Rect {
    let p = Palette::of(&painter.ctx().global_style().visuals);
    painter.rect_filled(rect, 0.0, p.back);
    let stroke = if active { p.frame_active } else { p.frame_idle };
    let font = FontId::monospace(13.0);
    let row = painter.ctx().fonts_mut(|f| f.row_height(&font));
    let frame = Rect::from_min_max(rect.min + Vec2::new(0.0, row / 2.0), rect.max);
    painter.rect_stroke(frame, 0.0, stroke, egui::StrokeKind::Inside);
    let room = (rect.width() - 24.0).max(0.0);
    let galley = painter.layout_no_wrap(fit_left(painter.ctx(), title, &font, room), font, p.text);
    let width = galley.size().x.min(room);
    let at = Pos2::new(rect.center().x - width / 2.0, rect.top());
    painter.rect_filled(
        Rect::from_min_size(at - Vec2::new(4.0, 0.0), Vec2::new(width + 8.0, row)),
        0.0,
        p.back,
    );
    painter.with_clip_rect(rect).galley(at, galley, p.text);
    Rect::from_min_max(
        Pos2::new(frame.left() + 4.0, rect.top() + row + 2.0),
        Pos2::new(frame.right() - 4.0, frame.bottom() - 4.0),
    )
}

/// `text` as it fits in `room` points of `font` (monospace): whole, or its end after "…" — a
/// path keeps the folder it ends in.
pub fn fit_left(ctx: &egui::Context, text: &str, font: &FontId, room: f32) -> String {
    let cw = ctx.fonts_mut(|f| f.glyph_width(font, '0')).max(1.0);
    let fits = (room / cw).floor() as usize;
    let n = text.chars().count();
    if n <= fits {
        return text.to_string();
    }
    let keep = fits.saturating_sub(1);
    let tail: String = text.chars().skip(n - keep).collect();
    format!("…{tail}")
}

/// The highlight bar behind a row.
pub fn bar(painter: &egui::Painter, row: Rect, active: bool) {
    let p = Palette::of(&painter.ctx().global_style().visuals);
    painter.rect_filled(row, 0.0, if active { p.bar } else { p.bar_idle });
}

/// A line across a panel (above its footer).
pub fn rule(painter: &egui::Painter, left: f32, right: f32, y: f32) {
    let p = Palette::of(&painter.ctx().global_style().visuals);
    painter.hline(left..=right, y, p.frame_idle);
}
