//! Drives a window's real `ui` with synthetic input — pointer presses, moves and releases,
//! wheel events, keys with modifiers — through a plain `egui::Context`, one pass per call, as
//! eframe would. No renderer: shapes are inspected instead (to find a button by its label).

use super::Window;
use eframe::egui::{
    self, Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, RawInput,
    Rect, TouchPhase, Vec2,
};

pub struct Driver {
    pub ctx: egui::Context,
    size: Vec2,
    time: f64,
    pointer: Pos2,
    /// What the last pass painted (to find a button by its label).
    shapes: Vec<egui::epaint::ClippedShape>,
}

impl Driver {
    /// A `w`×`h`-point window at 1 pixel per point.
    pub fn new(w: f32, h: f32) -> Self {
        Self {
            ctx: egui::Context::default(),
            size: Vec2::new(w, h),
            time: 0.0,
            pointer: Pos2::new(w / 2.0, h / 2.0),
            shapes: Vec::new(),
        }
    }

    /// One pass with `events`, `modifiers` held (sent first, as a backend does).
    pub fn frame<W: Window>(&mut self, win: &mut W, events: Vec<Event>, modifiers: Modifiers) {
        self.time += 1.0 / 60.0;
        let mut all = vec![Event::ModifiersChanged(modifiers)];
        all.extend(events);
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            max_texture_side: Some(8192),
            events: all,
            focused: true,
            ..Default::default()
        };
        let mut out = self.ctx.run_ui(raw, |ui| win.ui(ui));
        self.shapes = std::mem::take(&mut out.shapes);
        // No renderer: the texture uploads are dropped on purpose.
        out.textures_delta.clear();
    }

    /// `n` passes without input.
    pub fn idle<W: Window>(&mut self, win: &mut W, n: usize) {
        for _ in 0..n {
            self.frame(win, Vec::new(), Modifiers::NONE);
        }
    }

    /// Passes until `done` (sleeping between them: decoders and save workers run on threads),
    /// or panics after `secs`.
    pub fn until<W: Window>(&mut self, win: &mut W, secs: u64, mut done: impl FnMut(&W) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while !done(win) {
            assert!(
                std::time::Instant::now() < deadline,
                "condition not met in {secs} s"
            );
            self.frame(win, Vec::new(), Modifiers::NONE);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    pub fn move_to<W: Window>(&mut self, win: &mut W, to: Pos2) {
        self.pointer = to;
        self.frame(win, vec![Event::PointerMoved(to)], Modifiers::NONE);
    }

    fn button<W: Window>(&mut self, win: &mut W, button: PointerButton, pressed: bool) {
        let event = Event::PointerButton {
            pos: self.pointer,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        };
        self.frame(win, vec![event], Modifiers::NONE);
    }

    /// Press at `from`, move to `to` in steps, release there.
    pub fn drag<W: Window>(&mut self, win: &mut W, button: PointerButton, from: Pos2, to: Pos2) {
        self.move_to(win, from);
        self.button(win, button, true);
        for k in 1..=8 {
            self.move_to(win, from + (to - from) * (k as f32 / 8.0));
        }
        self.button(win, button, false);
        self.idle(win, 1);
    }

    pub fn click<W: Window>(&mut self, win: &mut W, at: Pos2) {
        self.move_to(win, at);
        self.button(win, PointerButton::Primary, true);
        self.button(win, PointerButton::Primary, false);
        self.idle(win, 1);
    }

    /// A mouse wheel (line units) over `at`.
    pub fn wheel<W: Window>(&mut self, win: &mut W, at: Pos2, lines: f32) {
        self.move_to(win, at);
        let event = Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: Vec2::new(0.0, lines),
            phase: TouchPhase::Move,
            source: MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        };
        self.frame(win, vec![event], Modifiers::NONE);
        self.idle(win, 1);
    }

    /// Press and release `key` with `modifiers` held.
    pub fn key<W: Window>(&mut self, win: &mut W, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            let event = Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            };
            self.frame(win, vec![event], modifiers);
        }
        self.idle(win, 1);
    }

    /// Where the last pass drew `text` (a button's label), if it did.
    pub fn find_text(&self, text: &str) -> Option<Rect> {
        fn walk(shape: &egui::Shape, text: &str) -> Option<Rect> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == text => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, text)),
                _ => None,
            }
        }
        self.shapes.iter().find_map(|c| walk(&c.shape, text))
    }

    /// Clicks the control labelled `text`.
    pub fn click_text<W: Window>(&mut self, win: &mut W, text: &str) {
        let rect = self
            .find_text(text)
            .unwrap_or_else(|| panic!("nothing labelled {text:?} on screen"));
        self.click(win, rect.center());
    }
}

/// Cmd on macOS, Ctrl elsewhere — how egui reports the platform's command key.
pub fn command(shift: bool) -> Modifiers {
    if shift {
        Modifiers::COMMAND | Modifiers::SHIFT
    } else {
        Modifiers::COMMAND
    }
}
