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
