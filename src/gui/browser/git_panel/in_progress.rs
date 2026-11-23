//! The in-progress banner (03 §8.5): the git panel's top row, in the warning colour, while git
//! has an operation in progress — `git::actions::Banner`'s text and its Continue, Skip and Abort
//! (bisect: Reset).
//!
//! [`BannerView`] is what the banner shows: its text, one row a line (bisect has a line of its
//! own beside any operation), and the steps it offers, in order — made from a
//! `git::actions::Banner` by [`BannerView::of`], or from plain text in a test. [`draw`] paints
//! it in the top rows the panel gives it, the offered steps as buttons at the right of its first
//! row. Its keys (03 §9.2) are Cmd/Ctrl+Shift+C, S, A and B — Continue, Skip, Abort, Bisect Reset
//! — read by [`keys`] where the git context's rows are read, never while a field has the typing.
//! A key or a click gives [`Pressed::Proceed`] with `Action::Proceed`, which goes through its
//! dialog (`ActionDialog::confirm`); a step the state does not offer gives
//! [`Pressed::NotOffered`], the status row's words, and does nothing (Skip in a merge).

// Nothing uses these screens until S3-C wires them into the panel (as `mod.rs`'s own
// allowance says of S2-F); S3-C removes this line when it does.
#![allow(dead_code)]

use crate::git::actions::{Action, Banner, Proceed};
use crate::git::model::{InProgress, Status};
use crate::gui::browser::keys::{self, Mods};
use crate::gui::chrome::{self, PALETTE};
use eframe::egui::{self, Key, Pos2, Rect, Vec2};

/// What the banner shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerView {
    /// Its text, a row a line (`REBASE 3/7 · stopped at abc12345 fix · 2 conflicts`,
    /// `BISECT · Reset`).
    lines: Vec<String>,
    /// The steps the state offers, in order.
    offers: Vec<Proceed>,
}

/// What a banner key or button asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pressed {
    /// The step, through its dialog (03 §9.2).
    Proceed(Action),
    /// A step the state does not offer: nothing happens, and the status row says this.
    NotOffered(String),
}

/// The banner's steps, in their keys' order.
const STEPS: [Proceed; 4] = [
    Proceed::Continue,
    Proceed::Skip,
    Proceed::Abort,
    Proceed::BisectReset,
];

/// What a step's button says.
pub fn word(p: Proceed) -> &'static str {
    match p {
        Proceed::Continue => "Continue",
        Proceed::Skip => "Skip",
        Proceed::Abort => "Abort",
        Proceed::BisectReset => "Reset",
    }
}

/// A step's key, with Cmd/Ctrl+Shift (03 §9.2).
pub fn key(p: Proceed) -> Key {
    match p {
        Proceed::Continue => Key::C,
        Proceed::Skip => Key::S,
        Proceed::Abort => Key::A,
        Proceed::BisectReset => Key::B,
    }
}

/// A step's key as the help writes it: `Cmd+Shift+C`.
pub fn key_text(p: Proceed) -> String {
    format!("{}{}", Mods::CommandShift.prefix(), key(p).name())
}

impl BannerView {
    /// A banner of `text` (its lines, the rows) offering `offers`.
    pub fn new(text: impl Into<String>, offers: Vec<Proceed>) -> BannerView {
        let text = text.into();
        BannerView {
            lines: text.lines().map(str::to_string).collect(),
            offers,
        }
    }

    /// What `Banner` says: its text and its offers.
    pub fn of(banner: &Banner) -> BannerView {
        BannerView::new(banner.text(), banner.offers())
    }

    /// The banner of a refresh (03 §5, §8.5): the probe's state and the status read's
    /// conflicts, through `Banner::of`; `None` while nothing is in progress.
    pub fn probe(in_progress: &InProgress, status: &Status) -> Option<BannerView> {
        Banner::of(in_progress, status).map(|b| BannerView::of(&b))
    }

    /// The rows it takes at the panel's top.
    pub fn rows(&self) -> usize {
        self.lines.len().max(1)
    }

    pub fn offers(&self) -> &[Proceed] {
        &self.offers
    }

    /// Step `p`, by its key or its button.
    pub fn press(&self, p: Proceed) -> Pressed {
        if self.offers.contains(&p) {
            return Pressed::Proceed(Action::Proceed(p));
        }
        let state = self.lines.first().map_or("", String::as_str);
        Pressed::NotOffered(format!("{}: not offered while {state}", word(p)))
    }
}

/// The banner's keys this frame (03 §9.2), with exact modifiers: Cmd/Ctrl+Shift+C, S, A, B.
/// Read only where the git context's rows are read (no field has the typing).
pub fn keys(ctx: &egui::Context, banner: &BannerView) -> Option<Pressed> {
    STEPS.into_iter().find_map(|p| {
        ctx.input_mut(|i| keys::take(i, key(p), Mods::CommandShift))
            .then(|| banner.press(p))
    })
}

/// The banner in `rect` (the panel's top rows, [`BannerView::rows`] of them): its lines in the
/// warning colour, the offered steps as buttons at the right of the first row — a click is the
/// step's key.
pub fn draw(ui: &mut egui::Ui, rect: Rect, banner: &BannerView) -> Option<Pressed> {
    let ctx = ui.ctx().clone();
    let font = chrome::mono(ui);
    let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let row_h = text_h.ceil();
    let painter = ui.painter_at(rect);
    // The buttons, right to left from the first row's end.
    let mut right = rect.right();
    let mut pressed = None;
    for &p in banner.offers.iter().rev() {
        let w = cw * (word(p).chars().count() + 2) as f32;
        let at = Rect::from_min_size(Pos2::new(right - w, rect.top()), Vec2::new(w, row_h));
        let button = ui
            .put(at, egui::Button::new(word(p)))
            .on_hover_text(key_text(p));
        if button.clicked() {
            pressed = Some(banner.press(p));
        }
        right -= w + cw;
    }
    for (k, line) in banner.lines.iter().enumerate() {
        let y = rect.top() + row_h * k as f32;
        let end = if k == 0 { right } else { rect.right() };
        let shown = chrome::fit_start(&ctx, line, &font, (end - rect.left()).max(0.0));
        painter.text(
            Pos2::new(rect.left(), y),
            egui::Align2::LEFT_TOP,
            shown,
            font.clone(),
            PALETTE.warn,
        );
    }
    pressed
}
