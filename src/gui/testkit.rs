//! Drives a window's real `ui` with synthetic input — pointer presses, moves and releases,
//! wheel events, keys with modifiers — through a plain `egui::Context`, one pass per call, as
//! eframe would. No renderer: shapes are inspected instead (to find a button by its label).

mod meter;

pub use meter::Meter;

use super::Window;
use eframe::egui::{
    self, Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, RawInput,
    Rect, TouchPhase, Vec2,
};

/// 2026-11-11 09:24:00 UTC: a whole minute (seconds since the epoch).
pub const START_OF_A_MINUTE: i64 = 1_794_389_040;

pub struct Driver {
    pub ctx: egui::Context,
    size: Vec2,
    time: f64,
    pointer: Pos2,
    /// What the last pass painted (to find a button by its label).
    shapes: Vec<egui::epaint::ClippedShape>,
    /// What the passes so far asked the platform to do (copy to the clipboard, …), in order —
    /// what eframe hands to egui-winit, which writes the system clipboard.
    commands: Vec<egui::OutputCommand>,
    /// Every viewport command the passes sent (a close, a resize), oldest first, until taken.
    viewport: Vec<egui::ViewportCommand>,
    /// Passes run as the shell runs them ([`Driver::as_shell`]).
    shell: bool,
    /// A pass asked to close: the next one carries the close request, as eframe delivers it.
    close_pending: bool,
    /// Closes the window refused (the shell cancelled them) and whether one went through.
    pub refused_closes: usize,
    pub window_closed: bool,
}

impl Driver {
    /// A `w`×`h`-point window at 1 pixel per point, in the skin every window gets before its
    /// first frame ([`super::chrome::install`]), so tests lay out what the owner sees.
    pub fn new(w: f32, h: f32) -> Self {
        let ctx = egui::Context::default();
        super::chrome::install(&ctx);
        // The title row's clock at the start of a minute: its next repaint is a minute away,
        // out of any test's way (a test that wants another second sets it after this).
        super::chrome::set_test_now(Some(START_OF_A_MINUTE));
        Self {
            ctx,
            size: Vec2::new(w, h),
            time: 0.0,
            pointer: Pos2::new(w / 2.0, h / 2.0),
            shapes: Vec::new(),
            commands: Vec::new(),
            viewport: Vec::new(),
            shell: false,
            close_pending: false,
            refused_closes: 0,
            window_closed: false,
        }
    }

    /// From now on each pass is run as the shell runs a window of its own: a close asked for in
    /// one pass reaches the next as the platform's close request, the shell asks the window
    /// ([`crate::gui::closing`]), and its answer is counted ([`Driver::refused_closes`]) or
    /// closes it ([`Driver::window_closed`]).
    pub fn as_shell(&mut self) {
        self.shell = true;
    }

    /// The platform commands of the passes since the last call (see [`Driver::commands`]).
    pub fn take_commands(&mut self) -> Vec<egui::OutputCommand> {
        std::mem::take(&mut self.commands)
    }

    /// The window is now `w`×`h` points, from the next pass on (as when the owner resizes it).
    pub fn resize(&mut self, w: f32, h: f32) {
        self.size = Vec2::new(w, h);
    }

    /// One pass with `events`, `modifiers` held (sent first, as a backend does).
    pub fn frame<W: Window>(&mut self, win: &mut W, events: Vec<Event>, modifiers: Modifiers) {
        self.time += 1.0 / 60.0;
        let mut all = vec![Event::ModifiersChanged(modifiers)];
        all.extend(events);
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            max_texture_side: Some(8192),
            events: all,
            focused: true,
            ..Default::default()
        };
        let delivered = self.shell && std::mem::take(&mut self.close_pending);
        if delivered {
            raw.viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .events
                .push(egui::ViewportEvent::Close);
        }
        let shell = self.shell;
        let mut out = self.ctx.run_ui(raw, |ui| {
            if shell {
                super::closing(ui.ctx(), win, false);
            }
            win.ui(ui)
        });
        self.shapes = std::mem::take(&mut out.shapes);
        self.commands.append(&mut out.platform_output.commands);
        let mut sent = Vec::new();
        for (_, viewport) in std::mem::take(&mut out.viewport_output) {
            sent.extend(viewport.commands);
        }
        if self.shell {
            let cancelled = sent.contains(&egui::ViewportCommand::CancelClose);
            if delivered && cancelled {
                self.refused_closes += 1;
            } else if delivered {
                self.window_closed = true;
            }
            self.close_pending |= sent.contains(&egui::ViewportCommand::Close);
        }
        self.viewport.extend(sent);
        // No renderer: the texture uploads are dropped on purpose.
        out.textures_delta.clear();
    }

    /// The viewport commands sent since the last call (a close, a resize).
    pub fn take_viewport_commands(&mut self) -> Vec<egui::ViewportCommand> {
        std::mem::take(&mut self.viewport)
    }

    /// Whether a pass since the last [`Driver::take_viewport_commands`] asked the viewport to
    /// close.
    pub fn closed(&self) -> bool {
        self.viewport
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::Close))
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

    /// Presses `button` where the pointer is (it stays held through later passes).
    pub fn press<W: Window>(&mut self, win: &mut W, button: PointerButton) {
        self.button(win, button, true);
    }

    pub fn release<W: Window>(&mut self, win: &mut W, button: PointerButton) {
        self.button(win, button, false);
    }

    /// Press at `from`, move to `to` in steps, release there.
    pub fn drag<W: Window>(&mut self, win: &mut W, button: PointerButton, from: Pos2, to: Pos2) {
        self.drag_watching(win, button, from, to, |_| {});
    }

    /// As [`Driver::drag`], with `held` called after the press and after every move while the
    /// button is down.
    pub fn drag_watching<W: Window>(
        &mut self,
        win: &mut W,
        button: PointerButton,
        from: Pos2,
        to: Pos2,
        mut held: impl FnMut(&W),
    ) {
        self.move_to(win, from);
        self.press(win, button);
        held(win);
        for k in 1..=8 {
            self.move_to(win, from + (to - from) * (k as f32 / 8.0));
            held(win);
        }
        self.release(win, button);
        self.idle(win, 1);
    }

    pub fn click<W: Window>(&mut self, win: &mut W, at: Pos2) {
        self.move_to(win, at);
        self.press(win, PointerButton::Primary);
        self.release(win, PointerButton::Primary);
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

    /// Cmd/Ctrl+`key` as egui-winit 0.37 delivers a clipboard shortcut (its `src/lib.rs`,
    /// `State::on_keyboard_input`): each press becomes `event` alone — no pressed `Key`; none
    /// at all for a paste of empty text — and only the release arrives as a `Key`. It drops
    /// winit's repeat flag, so a key held until it repeats is `event` again, once per repeat:
    /// `presses` passes of it before the release. Cmd/Ctrl is let go after.
    pub fn shortcut<W: Window>(
        &mut self,
        win: &mut W,
        key: Key,
        event: Option<Event>,
        presses: usize,
    ) {
        let mods = command(false);
        for _ in 0..presses {
            self.frame(win, event.clone().into_iter().collect(), mods);
        }
        self.frame(win, vec![key_up(key)], mods);
        self.idle(win, 1);
    }

    /// Cmd/Ctrl+C.
    pub fn copy<W: Window>(&mut self, win: &mut W) {
        self.shortcut(win, Key::C, Some(Event::Copy), 1);
    }

    /// Cmd/Ctrl+X.
    pub fn cut<W: Window>(&mut self, win: &mut W) {
        self.shortcut(win, Key::X, Some(Event::Cut), 1);
    }

    /// Cmd/Ctrl+V with text on the clipboard (egui-winit sends nothing for empty text).
    pub fn paste_text<W: Window>(&mut self, win: &mut W, text: &str) {
        let event = (!text.is_empty()).then(|| Event::Paste(text.to_string()));
        self.shortcut(win, Key::V, event, 1);
    }

    /// Cmd/Ctrl+V with only an image on the clipboard: egui-winit reads it with arboard and
    /// hands it over as premultiplied [`egui::ColorImage`].
    pub fn paste_image<W: Window>(&mut self, win: &mut W, image: egui::ColorImage) {
        let event = Event::PasteImage(std::sync::Arc::new(image));
        self.shortcut(win, Key::V, Some(event), 1);
    }

    /// What the last pass drew where it can be seen: each shape `pick` takes, with its bounds on
    /// screen, when all of those bounds lie inside the shape's clip rect and the window.
    fn shown<T>(&self, pick: impl Fn(&egui::Shape) -> Option<(Rect, T)>) -> Vec<(Rect, T)> {
        fn walk<T>(
            shape: &egui::Shape,
            visible: Rect,
            pick: &dyn Fn(&egui::Shape) -> Option<(Rect, T)>,
            out: &mut Vec<(Rect, T)>,
        ) {
            if let egui::Shape::Vec(v) = shape {
                v.iter().for_each(|s| walk(s, visible, pick, out));
            } else if let Some((bounds, t)) = pick(shape) {
                // A hair of slack for float rounding at the edges.
                if visible.expand(0.5).contains_rect(bounds) {
                    out.push((bounds, t));
                }
            }
        }
        let viewport = Rect::from_min_size(Pos2::ZERO, self.size);
        let mut out = Vec::new();
        for c in &self.shapes {
            // The clip rect is in the space the shape's transform maps to: compare there.
            let visible = c.clip_rect.intersect(viewport);
            let moved = c.transform_after_tessellation;
            let mut found = Vec::new();
            walk(
                &c.shape,
                moved.inverse().mul_rect(visible),
                &pick,
                &mut found,
            );
            out.extend(found.into_iter().map(|(r, t)| (moved.mul_rect(r), t)));
        }
        out
    }

    /// Where the last pass drew `text` whole (a button's label, a disclosure), if it did:
    /// the galley holds all of it (a galley cut short with "…" keeps the full string but is
    /// `elided`), and all of its bounds lie inside the shape's clip rect and the window.
    pub fn find_text(&self, text: &str) -> Option<Rect> {
        let found = self.shown(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == text && !t.galley.elided => {
                Some((t.galley.rect.translate(t.pos.to_vec2()), ()))
            }
            _ => None,
        });
        found.first().map(|(r, ())| *r)
    }

    /// The colour of each section of the galley the last pass drew for exactly `text`, where it
    /// can be seen (as for [`Driver::find_text`]) — a highlighted line has several.
    pub fn colors_of(&self, text: &str) -> Option<Vec<egui::Color32>> {
        let found = self.shown(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == text && !t.galley.elided => Some((
                t.galley.rect.translate(t.pos.to_vec2()),
                t.galley
                    .job
                    .sections
                    .iter()
                    .map(|s| s.format.color)
                    .collect::<Vec<_>>(),
            )),
            _ => None,
        });
        found.into_iter().next().map(|(_, colors)| colors)
    }

    /// The colours `text` is painted in on screen, where the last pass drew it whole (as for
    /// [`Driver::colors_of`]) — a widget's label is laid out in a placeholder colour and painted
    /// in its state's text colour, which this resolves.
    pub fn painted_colors_of(&self, text: &str) -> Option<Vec<egui::Color32>> {
        let found = self.shown(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == text && !t.galley.elided => {
                let colors = t
                    .galley
                    .job
                    .sections
                    .iter()
                    .map(|s| match (t.override_text_color, s.format.color) {
                        (Some(c), _) => c,
                        (None, c) if c == egui::Color32::PLACEHOLDER => t.fallback_color,
                        (None, c) => c,
                    })
                    .collect::<Vec<_>>();
                Some((t.galley.rect.translate(t.pos.to_vec2()), colors))
            }
            _ => None,
        });
        found.into_iter().next().map(|(_, colors)| colors)
    }

    /// The first text the last pass drew whole, where it can be seen, that starts with `prefix`.
    pub fn text_starting_with(&self, prefix: &str) -> Option<String> {
        let found = self.shown(|s| match s {
            egui::Shape::Text(t) if t.galley.text().starts_with(prefix) && !t.galley.elided => {
                Some((
                    t.galley.rect.translate(t.pos.to_vec2()),
                    t.galley.text().to_string(),
                ))
            }
            _ => None,
        });
        found.into_iter().next().map(|(_, text)| text)
    }

    /// Every text the last pass drew whole, where it can be seen, that starts with `prefix`,
    /// with where it is, in drawing order.
    pub fn texts_starting_with(&self, prefix: &str) -> Vec<(Rect, String)> {
        self.shown(|s| match s {
            egui::Shape::Text(t) if t.galley.text().starts_with(prefix) && !t.galley.elided => {
                Some((
                    t.galley.rect.translate(t.pos.to_vec2()),
                    t.galley.text().to_string(),
                ))
            }
            _ => None,
        })
    }

    /// Every galley the last pass drew, wherever it lies, with where it was drawn (its glyphs'
    /// positions are relative to that): the order glyphs were laid out in, left to right.
    pub fn galleys(&self) -> Vec<(Pos2, std::sync::Arc<egui::Galley>)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(Pos2, std::sync::Arc<egui::Galley>)>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(t) => out.push((t.pos, t.galley.clone())),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in &self.shapes {
            walk(&c.shape, &mut out);
        }
        out
    }

    /// Every text the last pass drew, wherever it lies: its bounds on screen, the clip rect it was
    /// drawn under (what runs out of it is cut there on screen) and its text, in drawing order.
    /// Unlike [`Driver::find_text`] it keeps the texts that run out of their clip.
    pub fn texts(&self) -> Vec<(Rect, Rect, String)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(Rect, String)>) {
            match shape {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                egui::Shape::Text(t) => out.push((
                    t.galley.rect.translate(t.pos.to_vec2()),
                    t.galley.text().to_string(),
                )),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for c in &self.shapes {
            let mut found = Vec::new();
            walk(&c.shape, &mut found);
            let moved = c.transform_after_tessellation;
            out.extend(
                found
                    .into_iter()
                    .map(|(r, t)| (moved.mul_rect(r), c.clip_rect, t)),
            );
        }
        out
    }

    /// Where the last pass drew a picture (a mesh on a texture of the window's own, not the font
    /// atlas) that can be seen.
    pub fn pictures(&self) -> Vec<Rect> {
        let found = self.shown(|s| match s {
            egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default() => {
                Some((m.calc_bounds(), ()))
            }
            _ => None,
        });
        found.into_iter().map(|(r, ())| r).collect()
    }

    /// How many rows the last pass laid `text` out in, whole or cut with "…", if it drew it
    /// where it can be seen (as for [`Driver::find_text`]: inside its clip rect and the window).
    pub fn rows_of(&self, text: &str) -> Option<usize> {
        let found = self.shown(|s| match s {
            egui::Shape::Text(t) if t.galley.text() == text => Some((
                t.galley.rect.translate(t.pos.to_vec2()),
                t.galley.rows.len(),
            )),
            _ => None,
        });
        found.first().map(|(_, rows)| *rows)
    }

    /// The bounds of every open stroked path the last pass drew where it can be seen (as for
    /// [`Driver::find_text`]) — a spinner's arc is one.
    pub fn strokes(&self) -> Vec<Rect> {
        let found = self.shown(|s| match s {
            egui::Shape::Path(p) if !p.closed && p.stroke.width > 0.0 => {
                Some((p.visual_bounding_rect(), ()))
            }
            _ => None,
        });
        found.into_iter().map(|(r, ())| r).collect()
    }

    /// Every straight line the last pass drew where it can be seen (as for
    /// [`Driver::find_text`]): its two ends and its colour — a waveform's columns are such lines.
    pub fn segments(&self) -> Vec<([Pos2; 2], egui::Color32)> {
        let found = self.shown(|s| match s {
            egui::Shape::LineSegment { points, stroke } if stroke.width > 0.0 => Some((
                Rect::from_two_pos(points[0], points[1]),
                (*points, stroke.color),
            )),
            _ => None,
        });
        found.into_iter().map(|(_, line)| line).collect()
    }

    /// Every rectangle the last pass drew where it can be seen (as for [`Driver::find_text`]):
    /// its bounds, fill colour and stroke width.
    pub fn rects(&self) -> Vec<(Rect, egui::Color32, f32)> {
        let found = self.shown(|s| match s {
            egui::Shape::Rect(r) => Some((r.rect, (r.fill, r.stroke.width))),
            _ => None,
        });
        found
            .into_iter()
            .map(|(rect, (fill, width))| (rect, fill, width))
            .collect()
    }

    /// Clicks the control labelled `text`.
    pub fn click_text<W: Window>(&mut self, win: &mut W, text: &str) {
        let rect = self
            .find_text(text)
            .unwrap_or_else(|| panic!("nothing labelled {text:?} on screen"));
        self.click(win, rect.center());
    }
}

/// `key` let go with Cmd/Ctrl still held.
pub fn key_up(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: command(false),
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

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "a complete label, never cut with an ellipsis";

    /// Draws one label: in the open, inside a 20×20 clip, or below the bottom of the window.
    struct One {
        place: &'static str,
    }

    impl Window for One {
        fn ui(&mut self, ui: &mut egui::Ui) {
            match self.place {
                "clipped" => {
                    let min = ui.max_rect().min;
                    ui.set_clip_rect(Rect::from_min_size(min, Vec2::splat(20.0)));
                }
                "offscreen" => ui.add_space(2000.0),
                _ => {}
            }
            ui.add(egui::Label::new(TEXT).extend());
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// Whether the last pass holds a complete galley of `text` at all, wherever it lies.
    fn painted(drv: &Driver, text: &str) -> bool {
        fn walk(shape: &egui::Shape, text: &str) -> bool {
            match shape {
                egui::Shape::Text(t) => t.galley.text() == text && !t.galley.elided,
                egui::Shape::Vec(v) => v.iter().any(|s| walk(s, text)),
                _ => false,
            }
        }
        drv.shapes.iter().any(|c| walk(&c.shape, text))
    }

    /// The "shown whole" oracle: a complete (not elided) galley that is clipped is not shown —
    /// the negative control is painted, and must fail the check.
    #[test]
    fn find_text_requires_the_text_inside_its_clip_and_the_viewport() {
        for (place, shown) in [("open", true), ("clipped", false)] {
            let mut drv = Driver::new(600.0, 400.0);
            let mut win = One { place };
            drv.idle(&mut win, 2);
            assert!(painted(&drv, TEXT), "{place}: the galley is there");
            assert_eq!(drv.find_text(TEXT).is_some(), shown, "{place}");
            assert_eq!(drv.rows_of(TEXT), shown.then_some(1), "{place}");
        }
    }

    /// Outside the viewport, with egui's own culling out of the way: the shapes are made here.
    /// A complete galley whose clip holds it is shown only where the window is — also after its
    /// transform.
    #[test]
    fn find_text_requires_the_text_inside_the_viewport() {
        let mut drv = Driver::new(600.0, 400.0);
        drv.idle(&mut One { place: "open" }, 1);
        let galley = drv.ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                TEXT.to_string(),
                egui::FontId::proportional(14.0),
                egui::Color32::WHITE,
            )
        });
        let shape = |pos: Pos2, moved: Vec2| {
            let mut c = egui::epaint::ClippedShape::new(
                Rect::EVERYTHING,
                egui::Shape::galley(pos, galley.clone(), egui::Color32::WHITE),
            );
            c.transform_after_tessellation = egui::emath::TSTransform::from_translation(moved);
            c
        };
        for (pos, moved, shown) in [
            (Pos2::new(10.0, 10.0), Vec2::ZERO, true),
            (Pos2::new(10.0, 1000.0), Vec2::ZERO, false),
            (Pos2::new(700.0, 10.0), Vec2::ZERO, false),
            (Pos2::new(10.0, 10.0), Vec2::new(0.0, 1000.0), false),
            (Pos2::new(10.0, 1000.0), Vec2::new(0.0, -990.0), true),
        ] {
            drv.shapes = vec![shape(pos, moved)];
            assert!(painted(&drv, TEXT));
            let case = format!("at {pos:?} moved {moved:?}");
            assert_eq!(drv.find_text(TEXT).is_some(), shown, "{case}");
            assert_eq!(drv.rows_of(TEXT), shown.then_some(1), "{case}");
        }
    }

    /// Says no to the first close it is asked for, then yes.
    struct Reluctant {
        asked: usize,
    }

    impl Window for Reluctant {
        fn ui(&mut self, ui: &mut egui::Ui) {
            if ui.input(|i| i.key_pressed(Key::F3)) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        fn ready(&self) -> bool {
            true
        }

        fn close_requested(&mut self) -> bool {
            self.asked += 1;
            self.asked > 1
        }
    }

    /// As the shell: a close a pass asks for reaches the window on the next pass; a refusal is
    /// counted and the window stays, a yes closes it. Without the shell nothing asks the window.
    #[test]
    fn as_shell_asks_the_window_on_the_next_pass() {
        let mut drv = Driver::new(200.0, 100.0);
        let mut win = Reluctant { asked: 0 };
        drv.key(&mut win, Key::F3, Modifiers::NONE);
        assert!(drv.closed());
        assert_eq!(
            (win.asked, drv.refused_closes, drv.window_closed),
            (0, 0, false)
        );
        drv.as_shell();
        drv.key(&mut win, Key::F3, Modifiers::NONE);
        assert_eq!(
            (win.asked, drv.refused_closes, drv.window_closed),
            (1, 1, false)
        );
        drv.key(&mut win, Key::F3, Modifiers::NONE);
        assert_eq!(
            (win.asked, drv.refused_closes, drv.window_closed),
            (2, 1, true)
        );
        // Cmd/Ctrl+W asks the same way.
        let mut again = Reluctant { asked: 0 };
        let mut drv = Driver::new(200.0, 100.0);
        drv.as_shell();
        drv.key(&mut again, Key::W, command(false));
        assert_eq!((again.asked, drv.refused_closes), (1, 1));
    }

    /// Draws one spinner: in the open, or with a 4×4 clip at its corner.
    struct Spin {
        clipped: bool,
    }

    impl Window for Spin {
        fn ui(&mut self, ui: &mut egui::Ui) {
            if self.clipped {
                let min = ui.max_rect().min;
                ui.set_clip_rect(Rect::from_min_size(min, Vec2::splat(4.0)));
            }
            ui.add(egui::Spinner::new());
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// A spinner's arc is found where it can be seen, not where its clip hides it — the clipped
    /// one is painted (egui skips only a spinner wholly outside its clip), and must fail.
    #[test]
    fn strokes_require_the_path_inside_its_clip() {
        for (clipped, shown) in [(false, 1), (true, 0)] {
            let mut drv = Driver::new(600.0, 400.0);
            drv.idle(&mut Spin { clipped }, 2);
            let arcs = drv
                .shapes
                .iter()
                .filter(|c| matches!(c.shape, egui::Shape::Path(_)))
                .count();
            assert_eq!(arcs, 1, "clipped={clipped}: the arc is painted");
            assert_eq!(drv.strokes().len(), shown, "clipped={clipped}");
        }
    }
}
