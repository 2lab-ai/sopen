//! Fonts for text sopen draws into pixels itself — pasted text in the image window — kept
//! small so a later window (the file browser) can use the same faces.
//!
//! The windows' own labels need nothing from here: eframe's default `system_fonts` feature puts
//! `egui_system_font_provider` into every sopen window (`NativeOptions::system_font_fallback`,
//! on by default), and it loads an installed font the first time egui meets a character its
//! built-in fonts lack — Hangul included.
//!
//! [`hangul`] asks that same provider for a font with Hangul, so pasted text is drawn in the
//! font the window shows; [`fallback`] is egui's built-in proportional font (Latin, no Hangul),
//! for a system that has no such font.
//!
//! The one exception is sopen's own `⌘` ([`SYMBOLS`]), which no built-in font has: the face a
//! lazy fallback brings depends on what a window met first (the browser on Korean names drew
//! it from the Hangul face, the hex window from another), so every window pins one face for
//! it before the fallbacks ([`symbols`], [`pin_symbols`]).

use eframe::egui::{FallbackRequest, FontData, FontDefinitions, FontFamily, FontProvider as _};
use egui_system_font_provider::SystemFontProvider;
use std::sync::OnceLock;

/// One font face: a `.ttf` / `.otf` / `.ttc` file's bytes (memory-mapped for a system font)
/// and which face in it.
pub struct Face {
    /// `system:<family>:…` for an installed font, egui's name for its built-in one.
    pub name: String,
    pub data: FontData,
}

impl Face {
    pub fn bytes(&self) -> &[u8] {
        self.data.bytes()
    }

    pub fn index(&self) -> u32 {
        self.data.index
    }
}

fn provider() -> &'static SystemFontProvider {
    static PROVIDER: OnceLock<SystemFontProvider> = OnceLock::new();
    PROVIDER.get_or_init(SystemFontProvider::new)
}

/// Starts enumerating the installed fonts on a background thread (a few hundred milliseconds)
/// so that [`hangul`] does not wait later. Cheap; called when a window opens.
pub fn prefetch() {
    provider();
}

/// An installed font that has Hangul, or `None` when there is none. The first call waits
/// for the enumeration [`prefetch`] started.
pub fn hangul() -> Option<&'static Face> {
    static HANGUL: OnceLock<Option<Face>> = OnceLock::new();
    HANGUL
        .get_or_init(|| {
            let request = FallbackRequest {
                cluster: "한",
                family: &FontFamily::Proportional,
            };
            provider().font_for(&request).map(|found| Face {
                name: found.name,
                data: found.data,
            })
        })
        .as_ref()
}

/// The one character sopen writes itself (the key help's `Ctrl/⌘`) that the VGA font and
/// egui's built-in fonts lack; the rest of its symbols (`… · ← ↑ → ↓ ░ —`, the box glyphs) are
/// the VGA font's.
pub const SYMBOLS: &str = "⌘";

/// The face [`SYMBOLS`] are drawn in, in every window: of the installed faces a window loads
/// anyway — the one for Hangul ([`hangul`]: the browser on Korean names) and the one the system
/// gives for `⌘` itself — the one whose `⌘` is narrowest, as a cell is 8 points wide and the
/// cell painter cuts a glyph to its cell (on a Mac: Apple SD Gothic Neo's 11.6 points against
/// the system font's 14.7). `None` where no installed font has it. The first call waits for
/// the enumeration [`prefetch`] started.
pub fn symbols() -> Option<&'static Face> {
    static SYMBOLS_FACE: OnceLock<Option<Face>> = OnceLock::new();
    SYMBOLS_FACE
        .get_or_init(|| {
            let asked = FallbackRequest {
                cluster: SYMBOLS,
                family: &FontFamily::Monospace,
            };
            let system = provider().font_for(&asked).map(|found| Face {
                name: found.name,
                data: found.data,
            });
            let hangul = hangul().map(|f| Face {
                name: f.name.clone(),
                data: f.data.clone(),
            });
            [hangul, system]
                .into_iter()
                .flatten()
                .filter_map(|face| widest(&face).map(|w| (w, face)))
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, face)| face)
        })
        .as_ref()
}

/// The widest advance of [`SYMBOLS`] in `face`, in ems; `None` when it lacks one of them.
fn widest(face: &Face) -> Option<f32> {
    use ab_glyph::Font as _;
    let font = ab_glyph::FontRef::try_from_slice_and_index(face.bytes(), face.index()).ok()?;
    let em = font.units_per_em()?;
    SYMBOLS
        .chars()
        .map(|c| {
            let id = font.glyph_id(c);
            (id.0 != 0).then(|| font.h_advance_unscaled(id) / em)
        })
        .try_fold(0.0f32, |most, w| w.map(|w| most.max(w)))
}

/// A window's fonts beside the skin's (`chrome::install`): [`symbols`]'s face after the skin's
/// faces and before the system fallbacks eframe adds as a window meets characters, so `⌘` is
/// one glyph in every window, whatever the window met first. Not in the terminal (no font is
/// loaded on its path, 10 §4) nor in the tests' contexts.
pub fn pin_symbols(ctx: &eframe::egui::Context) {
    use eframe::egui::{FontInsert, FontPriority, InsertFontFamily};
    let Some(face) = symbols() else {
        return;
    };
    ctx.add_font(FontInsert {
        name: face.name.clone(),
        data: face.data.clone(),
        families: [FontFamily::Monospace, FontFamily::Proportional]
            .map(|family| InsertFontFamily {
                family,
                priority: FontPriority::Lowest,
            })
            .to_vec(),
    });
}

/// egui's built-in proportional font — always there; it has no Hangul.
pub fn fallback() -> &'static Face {
    static FALLBACK: OnceLock<Face> = OnceLock::new();
    FALLBACK.get_or_init(|| {
        let defs = FontDefinitions::default();
        let name = defs.families[&FontFamily::Proportional][0].clone();
        let data = defs.font_data[&name].as_ref().clone();
        Face { name, data }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    /// What eframe does for every window (`add_font_provider(SystemFontProvider::new())` in
    /// its native integration): egui then has a glyph for Hangul — on a machine with a Hangul
    /// font, which is also what [`hangul`] finds.
    #[test]
    fn a_window_draws_hangul_with_the_font_hangul_finds() {
        let Some(face) = hangul() else {
            eprintln!("skipping: no installed font with Hangul");
            return;
        };
        assert!(face.name.starts_with("system:"), "{}", face.name);
        let font = ab_glyph::FontRef::try_from_slice_and_index(face.bytes(), face.index())
            .expect("the face parses");
        use ab_glyph::Font as _;
        assert_ne!(font.glyph_id('한').0, 0, "{} has 한", face.name);

        let font_id = egui::FontId::proportional(14.0);
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.fonts_mut(|f| assert!(!f.has_glyph(&font_id, '한'), "built-in fonts lack it"));
        });
        out.textures_delta.clear();
        ctx.add_font_provider(std::sync::Arc::new(SystemFontProvider::new()));
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.fonts_mut(|f| assert!(f.has_glyph(&font_id, '한'), "found on demand"));
        });
        out.textures_delta.clear();
    }

    /// The width of `⌘` in the skin's font in a context set up as a window's is — the skin
    /// ([`crate::gui::chrome::install`]), eframe's system fallback, [`pin_symbols`] — after the
    /// window has laid out `first`: what a lazy fallback adds must not decide it.
    fn command_width(first: &str) -> f32 {
        let ctx = egui::Context::default();
        crate::gui::chrome::install(&ctx);
        ctx.add_font_provider(std::sync::Arc::new(SystemFontProvider::new()));
        pin_symbols(&ctx);
        let font = crate::gui::chrome::font();
        let mut width = 0.0;
        for text in [first, first, "⌘", "⌘"] {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                width = (ui.fonts_mut(|f| {
                    f.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE)
                }))
                .size()
                .x;
            });
            out.textures_delta.clear();
        }
        width
    }

    /// sopen's own symbol the VGA font lacks (`⌘`, every help's `Ctrl/⌘`) is the same glyph in
    /// every window: a window that met Hangul first (the browser on Korean names) and one that
    /// met `⌘` first (the hex window) draw it alike — the system's fallback picked by what came
    /// first gave the one a 11.6 pt glyph and the other a 14.7 pt one on a Mac.
    #[test]
    fn every_window_draws_the_command_symbol_in_one_face() {
        if hangul().is_none() || symbols().is_none() {
            eprintln!("skipping: no installed font with Hangul, or none with ⌘");
            return;
        }
        let (after_hangul, alone) = (command_width("한"), command_width("⌘"));
        assert!(alone > 0.0);
        assert_eq!(after_hangul, alone, "one face for ⌘ whatever came first");
    }

    #[test]
    fn the_fallback_is_egui_s_built_in_font_without_hangul() {
        let face = fallback();
        let font = ab_glyph::FontRef::try_from_slice_and_index(face.bytes(), face.index())
            .expect("the face parses");
        use ab_glyph::Font as _;
        assert_ne!(font.glyph_id('a').0, 0);
        assert_eq!(font.glyph_id('한').0, 0);
    }
}
