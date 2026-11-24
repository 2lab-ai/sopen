//! Drives a window's real `ui` with synthetic input — pointer presses, moves and releases,
//! wheel events, keys with modifiers — through a plain `egui::Context`, one pass per call, as
//! eframe would. No renderer: shapes are inspected instead (to find a button by its label).
//!
//! What a pass paints in cells ([`super::cells`]) is read from its cells (10 §15): the text queries
//! answer from the galleys egui drew and from the cell layers alike, in painting order, and leave
//! out the glyphs the cell painter drew one per cell. A cell text is a stretch of one row of a
//! layer ([`cells::find`]): looser than a galley's whole text, as part of a longer text is found too,
//! and never across two layers. [`Driver::column`] reads cells by where they stand.

mod meter;

pub use meter::Meter;

use super::cells::{self, Cells, Layer};
use super::Window;
use eframe::egui::{
    self, Color32, Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2,
    RawInput, Rect, TouchPhase, Vec2,
};
use std::sync::Arc;

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
    /// Whether the platform says the window has the keyboard (`RawInput::focused`): `false` is
    /// a window behind another one — a receipt window that came up without focus, or the owner
    /// in another application for a moment.
    pub focused: bool,
    /// What the last pass painted in cells, in the order it was painted.
    layers: Vec<Arc<Layer>>,
}

/// The last pass's cell layers as they reached the screen.
struct Placed {
    /// Per layer, the shape its first glyph is among the pass's shapes — its place in painting
    /// order, and the transform it was painted under; `None` when it has no glyph or was painted
    /// invisible.
    at: Vec<Option<usize>>,
    /// Per shape, whether the cell painter drew it as a glyph of a layer (empty: no layers).
    glyph: Vec<bool>,
}

/// The glyphs [`cells::paint`] draws for `l`, in its order: row by row, each at its cell's corner.
fn glyphs(l: &Layer) -> Vec<(Pos2, String)> {
    let a = l.buf.area;
    let mut out = vec![];
    for (y, row) in (a.top()..).zip(l.seen()) {
        for (x, s) in (a.left()..).zip(row) {
            if !s.symbol.trim().is_empty() {
                out.push((l.rect(Cells::new(x, y, 1, 1)).min, s.symbol));
            }
        }
    }
    out
}

/// How many shapes [`cells::paint`] adds for `l`: per row a rectangle per run of one background
/// (none where it has no RGB one), a text per glyph, and the caret's rectangle — each of them
/// nothing ([`egui::Shape::Noop`]) from a painter that paints invisibly.
fn shapes_of(l: &Layer) -> usize {
    let fills: usize = (l.seen().iter())
        .map(|row| {
            let starts = row.iter().enumerate();
            let starts =
                starts.filter(|&(i, s)| s.bg.is_some() && (i == 0 || row[i - 1].bg != s.bg));
            starts.count()
        })
        .sum();
    let caret = l.caret.is_some_and(|p| l.buf.area.contains(p));
    fills + glyphs(l).len() + usize::from(caret)
}

/// Answers from the shapes and from the cells, each with its place in painting order, as one list
/// in painting order (what is drawn on top comes last).
fn merged<T>(mut shapes: Vec<(usize, T)>, cells: Vec<(usize, T)>) -> Vec<T> {
    shapes.extend(cells);
    shapes.sort_by_key(|&(at, _)| at);
    shapes.into_iter().map(|(_, t)| t).collect()
}

impl Driver {
    /// A `w`×`h`-point window at 1 pixel per point, in the skin every window gets before its
    /// first frame ([`super::chrome::install`]), so tests lay out what the owner sees.
    pub fn new(w: f32, h: f32) -> Self {
        let ctx = egui::Context::default();
        super::chrome::install(&ctx);
        cells::set_recording(&ctx, true);
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
            focused: true,
            layers: Vec::new(),
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
            focused: self.focused,
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
            // Only the cells of the pass whose shapes come out: egui runs this again after a
            // pass it discards.
            cells::clear(ui.ctx());
            if shell {
                super::closing(ui.ctx(), win, false);
            }
            win.ui(ui)
        });
        self.shapes = std::mem::take(&mut out.shapes);
        self.layers = cells::layers(&self.ctx);
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
        let found = self.shown_at(&[], pick);
        found.into_iter().map(|(_, rt)| rt).collect()
    }

    /// As [`Driver::shown`], each with its place in painting order (its clipped shape's index),
    /// leaving out the clipped shapes `skip` marks.
    fn shown_at<T>(
        &self,
        skip: &[bool],
        pick: impl Fn(&egui::Shape) -> Option<(Rect, T)>,
    ) -> Vec<(usize, (Rect, T))> {
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
        for (i, c) in self.shapes.iter().enumerate() {
            if skip.get(i) == Some(&true) {
                continue;
            }
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
            out.extend(found.into_iter().map(|(r, t)| (i, (moved.mul_rect(r), t))));
        }
        out
    }

    /// Where each cell layer of the last pass is among its shapes, and which shapes are the cell
    /// painter's glyphs: a layer's glyphs are the run of texts, in its order and at its cells,
    /// that [`cells::paint`] adds — matched whole, so two layers never share one. A layer painted
    /// invisibly left as many nothings in a row as it has shapes (the layers with the most take
    /// theirs first); one that left neither panics, naming its origin: a layer transform moved
    /// its glyphs (`set_transform_layer`, a `Scene`, a drag), and its cells would drop out of
    /// every answer while its glyphs read as one-character texts.
    fn placed(&self) -> Placed {
        let mut placed = Placed {
            at: vec![None; self.layers.len()],
            glyph: Vec::new(),
        };
        if self.layers.is_empty() {
            return placed;
        }
        placed.glyph = vec![false; self.shapes.len()];
        let texts: Vec<(usize, Pos2, &str)> = (self.shapes.iter().enumerate())
            .filter_map(|(i, c)| match &c.shape {
                egui::Shape::Text(t) => Some((i, t.pos, t.galley.text())),
                _ => None,
            })
            .collect();
        let mut taken = vec![false; texts.len()];
        let mut unread = vec![];
        for (at, l) in placed.at.iter_mut().zip(&self.layers) {
            let want = glyphs(l);
            if want.is_empty() {
                continue;
            }
            let start = texts.windows(want.len()).enumerate().find_map(|(k, w)| {
                let same = (w.iter().zip(&want)).all(|(&(_, p, t), (q, s))| p == *q && t == s);
                (same && !taken[k..k + w.len()].contains(&true)).then_some(k)
            });
            let Some(k) = start else {
                unread.push((shapes_of(l), l, want));
                continue;
            };
            *at = Some(texts[k].0);
            let run = k..k + want.len();
            for (t, &(i, _, _)) in taken[run.clone()].iter_mut().zip(&texts[run]) {
                *t = true;
                placed.glyph[i] = true;
            }
        }
        unread.sort_by_key(|&(n, _, _)| std::cmp::Reverse(n));
        let mut nothing = vec![false; self.shapes.len()];
        for (n, l, want) in unread {
            let invisible = (0..=self.shapes.len().saturating_sub(n)).find(|&k| {
                let run = &self.shapes[k..k + n];
                let noop = |c: &egui::epaint::ClippedShape| matches!(c.shape, egui::Shape::Noop);
                run.iter().all(noop) && !nothing[k..k + n].contains(&true)
            });
            let Some(k) = invisible else {
                panic!(
                    "the cell layer at {:?} painted {} glyphs (the first {:?}) that are not among \
                     the pass's shapes: a layer transform moved them (set_transform_layer, a \
                     Scene, a drag), and the kit cannot read them",
                    l.origin,
                    want.len(),
                    want[0].1
                );
            };
            nothing[k..k + n].fill(true);
        }
        placed
    }

    /// Where cells `r` of layer `li` are on screen, with the layer's place in painting order, and
    /// whether they lie wholly inside its clip and the window (as [`Driver::shown`] asks of a
    /// shape); `None` for a layer that did not reach the screen.
    fn on_screen(&self, placed: &Placed, li: usize, r: Cells) -> Option<(usize, Rect, bool)> {
        let at = placed.at[li]?;
        let moved = self.shapes[at].transform_after_tessellation;
        let l = &self.layers[li];
        let viewport = Rect::from_min_size(Pos2::ZERO, self.size);
        let visible = l.clip.intersect(moved.inverse().mul_rect(viewport));
        let rect = l.rect(r);
        Some((
            at,
            moved.mul_rect(rect),
            visible.expand(0.5).contains_rect(rect),
        ))
    }

    /// Every place the cells of the last pass show `text` where it can be seen, with its place in
    /// painting order and its rect on screen.
    fn cell_hits(&self, placed: &Placed, text: &str) -> Vec<(usize, cells::Hit, Rect)> {
        let hits = cells::find(&self.layers, text).into_iter();
        hits.filter_map(|hit| {
            let (at, rect, seen) = self.on_screen(placed, hit.layer, hit.cells)?;
            seen.then_some((at, hit, rect))
        })
        .collect()
    }

    /// Where the last pass drew `text` whole (a button's label, a disclosure), if it did:
    /// the galley holds all of it (a galley cut short with "…" keeps the full string but is
    /// `elided`), and all of its bounds lie inside the shape's clip rect and the window — or the
    /// cells show it there (a stretch of a row: see the module's header).
    pub fn find_text(&self, text: &str) -> Option<Rect> {
        self.labelled(text).first().copied()
    }

    /// Every place the last pass drew `text` whole (as for [`Driver::find_text`]), in the order
    /// it was painted: what is drawn on top — a modal's — comes last.
    fn labelled(&self, text: &str) -> Vec<Rect> {
        let placed = self.placed();
        let shapes = self.shown_at(&placed.glyph, |s| match s {
            egui::Shape::Text(t) if t.galley.text() == text && !t.galley.elided => {
                Some((t.galley.rect.translate(t.pos.to_vec2()), ()))
            }
            _ => None,
        });
        let cells = self.cell_hits(&placed, text);
        merged(
            shapes.into_iter().map(|(at, (r, ()))| (at, r)).collect(),
            cells.into_iter().map(|(at, _, r)| (at, r)).collect(),
        )
    }

    /// The first of `shapes` (a colour query's answers from the shapes) and the colours of the
    /// cells that show `text`, in painting order.
    fn first_colors(
        &self,
        placed: &Placed,
        text: &str,
        shapes: Vec<(usize, (Rect, Vec<Color32>))>,
    ) -> Option<Vec<Color32>> {
        let cells = self.cell_hits(placed, text).into_iter();
        let cells =
            cells.map(|(at, hit, _)| (at, cells::colors(&self.layers[hit.layer], hit.cells)));
        let shapes = shapes.into_iter().map(|(at, (_, c))| (at, c)).collect();
        merged(shapes, cells.collect()).into_iter().next()
    }

    /// The colour of each section of the galley the last pass drew for exactly `text`, where it
    /// can be seen (as for [`Driver::find_text`]) — a highlighted line has several; in cells, the
    /// foregrounds of its cells, repeats merged.
    pub fn colors_of(&self, text: &str) -> Option<Vec<egui::Color32>> {
        let placed = self.placed();
        let found = self.shown_at(&placed.glyph, |s| match s {
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
        self.first_colors(&placed, text, found)
    }

    /// The colours `text` is painted in on screen, where the last pass drew it whole (as for
    /// [`Driver::colors_of`]) — a widget's label is laid out in a placeholder colour and painted
    /// in its state's text colour, which this resolves (a cell's colours are what is painted).
    pub fn painted_colors_of(&self, text: &str) -> Option<Vec<egui::Color32>> {
        let placed = self.placed();
        let found = self.shown_at(&placed.glyph, |s| match s {
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
        self.first_colors(&placed, text, found)
    }

    /// The first text the last pass drew whole, where it can be seen, that starts with `prefix`
    /// (as for [`Driver::texts_starting_with`]).
    pub fn text_starting_with(&self, prefix: &str) -> Option<String> {
        let found = self.texts_starting_with(prefix);
        found.into_iter().next().map(|(_, text)| text)
    }

    /// Every text the last pass drew whole, where it can be seen, that starts with `prefix`,
    /// with where it is, in drawing order. In cells, the text from where `prefix` stands on to the
    /// end of its run ([`cells::runs`]: in the colours of `prefix`'s last cell, up to a frame glyph
    /// or two blanks in a row, the blanks at its end cut) — `prefix` itself always whole.
    pub fn texts_starting_with(&self, prefix: &str) -> Vec<(Rect, String)> {
        let placed = self.placed();
        let shapes = self.shown_at(&placed.glyph, |s| match s {
            egui::Shape::Text(t) if t.galley.text().starts_with(prefix) && !t.galley.elided => {
                Some((
                    t.galley.rect.translate(t.pos.to_vec2()),
                    t.galley.text().to_string(),
                ))
            }
            _ => None,
        });
        let cells = self.cell_texts_from(&placed, prefix);
        merged(shapes, cells)
    }

    /// The cells' answers to [`Driver::texts_starting_with`], each with its place in painting
    /// order: for no prefix, every run where it can be seen.
    fn cell_texts_from(&self, placed: &Placed, prefix: &str) -> Vec<(usize, (Rect, String))> {
        let mut out = vec![];
        if prefix.is_empty() {
            for (li, l) in self.layers.iter().enumerate() {
                for run in cells::runs(&l.buf) {
                    if let Some((at, rect, true)) = self.on_screen(placed, li, run.cells) {
                        out.push((at, (rect, run.text)));
                    }
                }
            }
            return out;
        }
        for hit in cells::find(&self.layers, prefix) {
            let l = &self.layers[hit.layer];
            let a = l.buf.area;
            let row = &l.seen()[usize::from(hit.cells.y - a.y)];
            let at = |x: u16| &row[usize::from(x - a.x)];
            let blank = |x: u16| at(x).symbol.trim().is_empty();
            // The cells the glyph at `x` takes: a wide glyph's second has no symbol.
            let wide = |x: u16| {
                let second = (x + 1..a.right()).take_while(|&k| at(k).symbol.is_empty());
                1 + second.count() as u16
            };
            let mut last = hit.cells.right() - 1;
            while last > hit.cells.x && at(last).symbol.is_empty() {
                last -= 1;
            }
            let colours = (at(last).fg, at(last).bg);
            let (mut text, mut gap) = (prefix.to_string(), String::new());
            let (mut x, mut end) = (hit.cells.right(), hit.cells.right());
            while x < a.right() {
                let (s, w) = (at(x), wide(x));
                let two_blanks = blank(x) && x + w < a.right() && blank(x + w);
                let framed = s.symbol.chars().any(cells::frame_glyph);
                if framed || (s.fg, s.bg) != colours || two_blanks {
                    break;
                }
                if blank(x) {
                    gap.push_str(&s.symbol);
                } else {
                    text.push_str(&std::mem::take(&mut gap));
                    text.push_str(&s.symbol);
                    end = x + w;
                }
                x += w;
            }
            let run = Cells::new(hit.cells.x, hit.cells.y, end - hit.cells.x, 1);
            if let Some((at, rect, true)) = self.on_screen(placed, hit.layer, run) {
                out.push((at, (rect, text)));
            }
        }
        out
    }

    /// Every galley the last pass drew, wherever it lies, with where it was drawn (its glyphs'
    /// positions are relative to that): the order glyphs were laid out in, left to right — the
    /// cell painter's glyphs too, one per cell.
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
    /// Unlike [`Driver::find_text`] it keeps the texts that run out of their clip. In cells, a text
    /// is a run ([`cells::runs`]: one pair of colours, stopped by a frame glyph or two blanks).
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
        let placed = self.placed();
        let mut out = Vec::new();
        for (i, c) in self.shapes.iter().enumerate() {
            if placed.glyph.get(i) == Some(&true) {
                continue;
            }
            let mut found = Vec::new();
            walk(&c.shape, &mut found);
            let moved = c.transform_after_tessellation;
            out.extend(
                found
                    .into_iter()
                    .map(|(r, t)| (i, (moved.mul_rect(r), c.clip_rect, t))),
            );
        }
        let mut runs = vec![];
        for (l, at) in self.layers.iter().zip(&placed.at) {
            let Some(at) = *at else { continue };
            let moved = self.shapes[at].transform_after_tessellation;
            for run in cells::runs(&l.buf) {
                let rect = moved.mul_rect(l.rect(run.cells));
                runs.push((at, (rect, moved.mul_rect(l.clip), run.text)));
            }
        }
        merged(out, runs)
    }

    /// The cells of the last pass inside `area` (on screen), a row at a time: per row of each
    /// layer, in painting order and from the top, the rect of its cells that lie wholly inside
    /// `area` where they can be seen, and what they show — blanks kept, a wide glyph whose first
    /// cell is inside whole (its second cell, read alone, shows nothing). Cells keep no draw-call
    /// identity: two columns one blank apart are told apart by where their cells stand.
    pub fn column(&self, area: Rect) -> Vec<(Rect, String)> {
        let placed = self.placed();
        let mut order: Vec<(usize, usize)> = (placed.at.iter().enumerate())
            .filter_map(|(li, at)| Some(((*at)?, li)))
            .collect();
        order.sort_unstable();
        let viewport = Rect::from_min_size(Pos2::ZERO, self.size);
        let mut out = vec![];
        for (at, li) in order {
            let (l, moved) = (
                &self.layers[li],
                self.shapes[at].transform_after_tessellation,
            );
            let inside = l
                .clip
                .intersect(moved.inverse().mul_rect(area.intersect(viewport)));
            let a = l.buf.area;
            for (y, row) in (a.top()..).zip(l.seen()) {
                let xs: Vec<u16> = (a.left()..a.right())
                    .filter(|&x| (inside.expand(0.5)).contains_rect(l.rect(Cells::new(x, y, 1, 1))))
                    .collect();
                let (Some(&first), Some(&last)) = (xs.first(), xs.last()) else {
                    continue;
                };
                let text = xs
                    .iter()
                    .map(|&x| row[usize::from(x - a.x)].symbol.as_str());
                let cells = Cells::new(first, y, last + 1 - first, 1);
                out.push((moved.mul_rect(l.rect(cells)), text.collect()));
            }
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
    /// where it can be seen (as for [`Driver::find_text`]: inside its clip rect and the window) —
    /// one, where the cells show it.
    pub fn rows_of(&self, text: &str) -> Option<usize> {
        let placed = self.placed();
        let found = self.shown_at(&placed.glyph, |s| match s {
            egui::Shape::Text(t) if t.galley.text() == text => Some((
                t.galley.rect.translate(t.pos.to_vec2()),
                t.galley.rows.len(),
            )),
            _ => None,
        });
        let shapes = found
            .into_iter()
            .map(|(at, (_, rows))| (at, rows))
            .collect();
        let cells = self.cell_hits(&placed, text).into_iter();
        merged(shapes, cells.map(|(at, _, _)| (at, 1)).collect())
            .first()
            .copied()
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
    /// its bounds, fill colour and stroke width — once a screen paints cells, their background
    /// runs and the caret's two scan lines too.
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

    /// Clicks the control labelled `text` that is drawn on top: a prompt's button, not one of the
    /// same name in a row under it (a click there lands on the prompt's backdrop, which closes
    /// the prompt).
    pub fn click_text_on_top<W: Window>(&mut self, win: &mut W, text: &str) {
        let rect = self
            .labelled(text)
            .last()
            .copied()
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
    use crate::gui::chrome::PALETTE;
    use cells::{Buffer, Pen};

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

    // -----------------------------------------------------------------------------------------
    // Text queries over cells.

    /// A screen drawn in cells, as a window draws one: each layer a buffer painted through
    /// [`cells::paint`] at its origin and under its clip (all of it faded out when `faded`), an
    /// egui label laid out before or after them.
    #[derive(Default)]
    struct Screen {
        layers: Vec<(Pos2, Rect, Buffer)>,
        before: Option<&'static str>,
        after: Option<&'static str>,
        faded: bool,
    }

    impl Screen {
        fn of(layers: Vec<(Pos2, Rect, Buffer)>) -> Screen {
            Screen {
                layers,
                ..Screen::default()
            }
        }
    }

    impl Window for Screen {
        fn ui(&mut self, ui: &mut egui::Ui) {
            if let Some(text) = self.before {
                ui.label(text);
            }
            for (origin, clip, buf) in &self.layers {
                let mut painter = ui.painter().with_clip_rect(*clip);
                if self.faded {
                    painter.set_opacity(0.0);
                }
                cells::paint(&painter, *origin, buf, None);
            }
            if let Some(text) = self.after {
                ui.label(text);
            }
        }

        fn ready(&self) -> bool {
            true
        }
    }

    fn body() -> Pen {
        Pen::new(PALETTE.text, PALETTE.back)
    }

    /// A buffer `w`×`h` cells in the body's colours, as a panel is, with `texts` put in it:
    /// (column, row, text, pen).
    fn buffer(w: u16, h: u16, texts: &[(u16, u16, &str, Pen)]) -> Buffer {
        let all = Cells::new(0, 0, w, h);
        let mut buf = Buffer::empty(all);
        cells::fill(&mut buf, all, body());
        for &(x, y, text, pen) in texts {
            cells::put(&mut buf, x, y, text, usize::from(w), pen);
        }
        buf
    }

    /// The layers' cell (0, 0) in these tests: below where a label is laid out.
    const ORIGIN: Pos2 = Pos2::new(16.0, 96.0);

    /// Cells from column `x` in row `y`, `w` wide, of a layer at [`ORIGIN`], in points.
    fn at(x: u16, y: u16, w: u16) -> Rect {
        Rect::from_min_size(
            ORIGIN + Vec2::new(f32::from(x) * 8.0, f32::from(y) * 16.0),
            Vec2::new(f32::from(w) * 8.0, 16.0),
        )
    }

    /// The text queries answer from the cells: a text's cell rect, its colours (one per change,
    /// as a highlighted galley's sections), its one row, the text a prefix starts and a click at
    /// its middle. The texts are the runs, never the glyphs the painter drew one per cell, which
    /// only the galleys keep.
    #[test]
    fn the_text_queries_answer_from_the_cells() {
        let dim = Pen::new(PALETTE.dim, PALETTE.back);
        let bar = Pen::new(PALETTE.bar_text, PALETTE.bar);
        let buf = buffer(
            20,
            2,
            &[
                (1, 1, "abc", body()),
                (4, 1, "def", dim),
                (8, 1, "사진.png", bar),
            ],
        );
        let mut drv = Driver::new(600.0, 400.0);
        let mut win = Screen::of(vec![(ORIGIN, Rect::EVERYTHING, buf)]);
        drv.idle(&mut win, 1);
        assert_eq!(drv.layers.len(), 1);
        assert_eq!(drv.find_text("사진.png"), Some(at(8, 1, 8)));
        assert_eq!(drv.colors_of("사진.png"), Some(vec![PALETTE.bar_text]));
        assert_eq!(
            drv.painted_colors_of("사진.png"),
            Some(vec![PALETTE.bar_text])
        );
        assert_eq!(drv.rows_of("사진.png"), Some(1));
        assert_eq!(
            drv.colors_of("abcdef"),
            Some(vec![PALETTE.text, PALETTE.dim])
        );
        assert_eq!(drv.text_starting_with("사진"), Some("사진.png".to_string()));
        assert_eq!(
            drv.texts_starting_with("de"),
            vec![(at(4, 1, 3), "def".to_string())]
        );
        let clip = drv.layers[0].clip;
        let texts = [(1, 3, "abc"), (4, 3, "def"), (8, 8, "사진.png")];
        let texts: Vec<_> = (texts.iter())
            .map(|&(x, w, t)| (at(x, 1, w), clip, t.to_string()))
            .collect();
        assert_eq!(drv.texts(), texts);
        let every: Vec<_> = texts.iter().map(|(r, _, t)| (*r, t.clone())).collect();
        assert_eq!(drv.texts_starting_with(""), every, "no prefix: every text");
        assert_eq!(drv.galleys().len(), 12, "a glyph per cell");
        for absent in ["abd", "x", "png!"] {
            assert_eq!(drv.find_text(absent), None, "{absent:?}");
            assert_eq!(drv.colors_of(absent), None, "{absent:?}");
            assert_eq!(drv.rows_of(absent), None, "{absent:?}");
        }
        drv.click_text(&mut win, "사진.png");
        assert_eq!(drv.pointer, at(8, 1, 8).center());
    }

    /// A run stops at a frame glyph, though its colours go on, and at two blanks; a prefix comes
    /// back whole with the rest of its run in the colours of its last cell, across a change of
    /// colours inside it.
    #[test]
    fn a_run_stops_at_a_frame_glyph() {
        let dim = Pen::new(PALETTE.dim, PALETTE.back);
        let buf = buffer(
            24,
            2,
            &[
                (0, 0, "ab│cd x y  z", body()),
                (0, 1, "abc", body()),
                (3, 1, "defghi", dim),
            ],
        );
        let mut drv = Driver::new(600.0, 400.0);
        drv.idle(&mut Screen::of(vec![(ORIGIN, Rect::EVERYTHING, buf)]), 1);
        let texts: Vec<String> = drv.texts().into_iter().map(|(_, _, t)| t).collect();
        assert_eq!(texts, ["ab", "cd x y", "z", "abc", "defghi"]);
        assert_eq!(drv.text_starting_with("a"), Some("ab".to_string()));
        assert_eq!(drv.text_starting_with("cd"), Some("cd x y".to_string()));
        let crossing = drv.texts_starting_with("bcd");
        assert_eq!(crossing, vec![(at(1, 1, 8), "bcdefghi".to_string())]);
        assert_eq!(drv.find_text("ab│cd"), Some(at(0, 0, 5)));
    }

    /// The column query reads cells by where they stand: a list's Name and Ext columns one blank
    /// apart, which the runs join, come apart; blanks are kept, and a wide glyph that starts in
    /// the column is read whole there, its second cell — in the gap — showing nothing.
    #[test]
    fn a_column_query_reads_cells_by_where_they_stand() {
        let buf = buffer(
            10,
            2,
            &[
                (0, 0, "readme", body()),
                (7, 0, "txt", body()),
                (0, 1, "abcde한", body()),
                (7, 1, "rs", body()),
            ],
        );
        let mut drv = Driver::new(600.0, 400.0);
        drv.idle(&mut Screen::of(vec![(ORIGIN, Rect::EVERYTHING, buf)]), 1);
        let texts: Vec<String> = drv.texts().into_iter().map(|(_, _, t)| t).collect();
        assert_eq!(texts, ["readme txt", "abcde한rs"]);
        let column = |x: u16, w: u16| {
            let r = at(x, 0, w);
            drv.column(Rect::from_x_y_ranges(r.x_range(), 0.0..=400.0))
        };
        let rows = |x: u16, w: u16, texts: [&str; 2]| -> Vec<(Rect, String)> {
            let row = |y: u16, t: &str| (at(x, y, w), t.to_string());
            vec![row(0, texts[0]), row(1, texts[1])]
        };
        assert_eq!(column(0, 6), rows(0, 6, ["readme", "abcde한"]));
        assert_eq!(column(6, 1), rows(6, 1, [" ", ""]));
        assert_eq!(column(7, 3), rows(7, 3, ["txt", "rs "]));
        // A range of one row, and none of the cells.
        let one = drv.column(at(0, 1, 3));
        assert_eq!(one, vec![(at(0, 1, 3), "abc".to_string())]);
        assert!(drv
            .column(Rect::from_min_size(Pos2::ZERO, Vec2::splat(50.0)))
            .is_empty());
    }

    /// A text is found within one layer: two layers side by side show "abcd" on screen, and
    /// neither holds it — each half is found where its layer is.
    #[test]
    fn a_text_split_across_two_layers_is_not_found() {
        let next = ORIGIN + Vec2::new(16.0, 0.0);
        let mut drv = Driver::new(600.0, 400.0);
        drv.idle(
            &mut Screen::of(vec![
                (
                    ORIGIN,
                    Rect::EVERYTHING,
                    buffer(2, 1, &[(0, 0, "ab", body())]),
                ),
                (
                    next,
                    Rect::EVERYTHING,
                    buffer(2, 1, &[(0, 0, "cd", body())]),
                ),
            ]),
            1,
        );
        assert_eq!(drv.find_text("ab"), Some(at(0, 0, 2)));
        assert_eq!(drv.find_text("cd"), Some(at(2, 0, 2)));
        assert_eq!(drv.find_text("abcd"), None);
        assert_eq!(drv.text_starting_with("abc"), None);
        assert_eq!(drv.colors_of("bc"), None);
    }

    /// Two layers alike — the same cells at the same place, under two clips — each take their
    /// own glyphs: both are read, each under its clip, and no glyph is left over as a text of its
    /// own; only the one whose clip holds the text finds it.
    #[test]
    fn two_layers_alike_each_take_their_own_glyphs() {
        let buf = || buffer(4, 1, &[(0, 0, "abc", body())]);
        let narrow = at(0, 0, 1);
        let mut drv = Driver::new(600.0, 400.0);
        let layers = vec![(ORIGIN, Rect::EVERYTHING, buf()), (ORIGIN, narrow, buf())];
        drv.idle(&mut Screen::of(layers), 1);
        let placed = drv.placed();
        assert!(
            matches!(placed.at[..], [Some(a), Some(b)] if a < b),
            "{:?}",
            placed.at
        );
        assert_eq!(placed.glyph.iter().filter(|&&g| g).count(), 6);
        let wide = drv.layers[0].clip;
        let texts = drv.texts();
        let abc = |clip| (at(0, 0, 3), clip, "abc".to_string());
        assert_eq!(texts, vec![abc(wide), abc(narrow)]);
        assert_eq!(drv.labelled("abc"), vec![at(0, 0, 3)]);
    }

    /// A text that runs past its layer's clip is not found, as a galley that does is not; the
    /// part of its row inside the clip is.
    #[test]
    fn a_text_across_its_clips_edge_is_not_found() {
        let buf = buffer(10, 1, &[(3, 0, "abcdef", body())]);
        let mut drv = Driver::new(600.0, 400.0);
        drv.idle(&mut Screen::of(vec![(ORIGIN, at(0, 0, 5), buf)]), 1);
        assert_eq!(drv.find_text("abcdef"), None);
        assert_eq!(drv.colors_of("abcdef"), None);
        assert_eq!(drv.rows_of("abcdef"), None);
        assert_eq!(drv.text_starting_with("ab"), None, "its run goes on past");
        assert_eq!(drv.find_text("ab"), Some(at(3, 0, 2)));
    }

    /// Paints "abc" in cells on a layer of its own that egui moves by `by` (a layer transform
    /// moves the shapes themselves), invisibly when `faded` — and, when `beside`, a row of eight
    /// cells, each in a background of its own, invisibly where it is.
    #[derive(Default)]
    struct Moved {
        by: Vec2,
        faded: bool,
        beside: bool,
    }

    impl Window for Moved {
        fn ui(&mut self, ui: &mut egui::Ui) {
            let id = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("moved cells"));
            let moved = egui::emath::TSTransform::from_translation(self.by);
            ui.ctx().set_transform_layer(id, moved);
            let mut painter = ui.ctx().layer_painter(id);
            if self.faded {
                painter.set_opacity(0.0);
            }
            cells::paint(
                &painter,
                ORIGIN,
                &buffer(4, 1, &[(0, 0, "abc", body())]),
                None,
            );
            if self.beside {
                let bar = Pen::new(PALETTE.bar_text, PALETTE.bar);
                let texts: Vec<_> = (0..8)
                    .map(|x| (x, 0, "x", if x % 2 == 0 { bar } else { body() }))
                    .collect();
                let mut painter = ui.painter().clone();
                painter.set_opacity(0.0);
                cells::paint(&painter, Pos2::ZERO, &buffer(8, 1, &texts), None);
            }
        }

        fn ready(&self) -> bool {
            true
        }
    }

    /// A layer whose glyphs a layer transform moved is not read silently as nothing: every text
    /// query panics, naming the layer's origin — also beside an invisible layer, whose nothings
    /// are all its own. Unmoved it is read; painted invisibly, moved or not, it is not found and
    /// nothing panics.
    #[test]
    fn a_layer_the_kit_cannot_read_panics() {
        let mut drv = Driver::new(600.0, 400.0);
        let by = Vec2::new(40.0, 0.0);
        let mut win = Moved::default();
        drv.idle(&mut win, 1);
        assert_eq!(drv.find_text("abc"), Some(at(0, 0, 3)), "unmoved");
        for (by, faded) in [(Vec2::ZERO, true), (by, true)] {
            win = Moved {
                by,
                faded,
                beside: true,
            };
            drv.idle(&mut win, 1);
            assert_eq!(
                (drv.find_text("abc"), drv.texts()),
                (None, vec![]),
                "{by:?}"
            );
        }
        win = Moved {
            by,
            faded: false,
            beside: true,
        };
        drv.idle(&mut win, 1);
        assert_eq!(drv.layers.len(), 2);
        /// A text query, which must panic here.
        type Query = fn(&Driver);
        let queries: [(&str, Query); 4] = [
            ("find_text", |d| assert!(d.find_text("abc").is_some())),
            ("texts", |d| assert!(!d.texts().is_empty())),
            ("text_starting_with", |d| {
                assert!(d.text_starting_with("a").is_some())
            }),
            ("column", |d| {
                assert!(!d.column(Rect::EVERYTHING).is_empty())
            }),
        ];
        for (name, query) in queries {
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| query(&drv)));
            let said = ran.expect_err(name);
            let said = said.downcast_ref::<String>().expect("a message");
            assert!(said.contains(&format!("{ORIGIN:?}")), "{name}: {said}");
        }
    }

    /// As for a galley: cells outside their layer's clip or the window, or painted invisible,
    /// are not found — `texts` keeps the clipped ones with their clip, as it keeps a galley that
    /// runs out of its clip; the ones inside the clip are found.
    #[test]
    fn cells_where_they_cannot_be_seen_are_not_found() {
        let buf = || buffer(10, 1, &[(0, 0, "abc", body()), (6, 0, "xyz", body())]);
        let clip = at(0, 0, 5);
        let below = ORIGIN + Vec2::new(0.0, 1000.0);
        let mut drv = Driver::new(600.0, 400.0);
        let mut win = Screen::of(vec![
            (ORIGIN, clip, buf()),
            (below, Rect::EVERYTHING, buf()),
        ]);
        drv.idle(&mut win, 1);
        assert_eq!(drv.find_text("abc"), Some(at(0, 0, 3)), "inside the clip");
        assert_eq!(drv.find_text("xyz"), None);
        assert_eq!(drv.colors_of("xyz"), None);
        assert_eq!(drv.rows_of("xyz"), None);
        assert_eq!(drv.text_starting_with("xy"), None);
        assert_eq!(
            drv.texts_starting_with("ab").len(),
            1,
            "only the one in the window"
        );
        let texts = drv.texts();
        assert!(
            texts.contains(&(at(6, 0, 3), clip, "xyz".to_string())),
            "{texts:?}"
        );
        assert_eq!(texts.len(), 4, "both layers' texts, wherever they lie");
        let every = drv.texts_starting_with("");
        assert_eq!(every, vec![(at(0, 0, 3), "abc".to_string())], "seen");
        win.faded = true;
        drv.idle(&mut win, 1);
        assert_eq!(drv.layers.len(), 2, "painted, invisibly");
        assert_eq!(drv.find_text("abc"), None);
        assert!(drv.texts().is_empty());
        assert!(drv.column(Rect::EVERYTHING).is_empty());
    }

    /// A layer painted under a transform (a moved area) is where the transform puts it: found
    /// there and read in its column there, and not found when that is past the window's end —
    /// where `texts` still lists it.
    #[test]
    fn cells_under_a_transform_are_where_it_puts_them() {
        let mut drv = Driver::new(600.0, 400.0);
        let buf = buffer(4, 1, &[(0, 0, "abc", body())]);
        drv.idle(&mut Screen::of(vec![(ORIGIN, Rect::EVERYTHING, buf)]), 1);
        let shapes = drv.shapes.clone();
        for (by, shown) in [
            (Vec2::new(100.0, 8.0), true),
            (Vec2::new(0.0, 1000.0), false),
        ] {
            let moved = egui::emath::TSTransform::from_translation(by);
            drv.shapes = shapes.clone();
            for c in &mut drv.shapes {
                c.transform_after_tessellation = moved;
                c.clip_rect = moved.mul_rect(c.clip_rect);
            }
            let rect = at(0, 0, 3).translate(by);
            assert_eq!(drv.find_text("abc"), shown.then_some(rect), "{by:?}");
            assert_eq!(drv.texts()[0].0, rect, "{by:?}");
            let column = drv.column(rect);
            let want = shown.then(|| (rect, "abc".to_string()));
            assert_eq!(column, want.into_iter().collect::<Vec<_>>(), "{by:?}");
        }
    }

    /// Shapes and cells answer in the order they were painted: the same label in cells and in
    /// egui, the one painted later last — on top, where a click on top lands.
    #[test]
    fn shapes_and_cells_answer_in_painting_order() {
        let mut drv = Driver::new(600.0, 400.0);
        let mut alone = Screen {
            after: Some("Copy"),
            ..Screen::default()
        };
        drv.idle(&mut alone, 1);
        let label = drv.find_text("Copy").expect("the egui label");
        let cells = at(1, 0, 4);
        for (before, after, order) in [
            (None, Some("Copy"), [cells, label]),
            (Some("Copy"), None, [label, cells]),
        ] {
            let layer = (
                ORIGIN,
                Rect::EVERYTHING,
                buffer(8, 1, &[(1, 0, "Copy", body())]),
            );
            let mut win = Screen {
                layers: vec![layer],
                before,
                after,
                ..Screen::default()
            };
            drv.idle(&mut win, 1);
            assert_eq!(drv.labelled("Copy"), order);
            assert_eq!(drv.find_text("Copy"), Some(order[0]));
            let starting: Vec<Rect> = (drv.texts_starting_with("Cop").into_iter())
                .map(|(r, _)| r)
                .collect();
            assert_eq!(starting, order);
            drv.click_text_on_top(&mut win, "Copy");
            assert_eq!(drv.pointer, order[1].center());
        }
    }

    /// Without cells the answers are the shapes' alone, as before: a label's are the same with
    /// or without a cell layer beside it, the pass after the cells are gone forgets them, and the
    /// cells of a pass egui discarded are not kept.
    #[test]
    fn without_cells_the_answers_are_the_shapes_alone() {
        let answers = |drv: &Driver| {
            (
                drv.find_text(TEXT),
                drv.colors_of(TEXT),
                drv.painted_colors_of(TEXT),
                drv.rows_of(TEXT),
                drv.texts_starting_with("a complete"),
            )
        };
        let mut drv = Driver::new(600.0, 400.0);
        let mut win = Screen {
            before: Some(TEXT),
            ..Screen::default()
        };
        drv.idle(&mut win, 1);
        assert!(drv.layers.is_empty());
        let (egui_only, texts) = (answers(&drv), drv.texts());
        assert!(egui_only.0.is_some() && texts.len() == 1, "{texts:?}");
        win.layers = vec![(
            ORIGIN,
            Rect::EVERYTHING,
            buffer(4, 1, &[(0, 0, "cell", body())]),
        )];
        drv.idle(&mut win, 1);
        assert_eq!(answers(&drv), egui_only);
        assert_eq!(drv.texts()[..1], texts[..]);
        assert_eq!(drv.find_text("cell"), Some(at(0, 0, 4)));
        win.layers.clear();
        drv.idle(&mut win, 1);
        assert!(drv.layers.is_empty());
        assert_eq!((answers(&drv), drv.texts()), (egui_only, texts));
        assert_eq!(drv.find_text("cell"), None);

        /// Paints "old" in its first pass and asks egui to discard it, then "new".
        struct Discarding {
            passes: usize,
        }
        impl Window for Discarding {
            fn ui(&mut self, ui: &mut egui::Ui) {
                self.passes += 1;
                let text = if self.passes == 1 { "old" } else { "new" };
                let buf = buffer(3, 1, &[(0, 0, text, body())]);
                cells::paint(ui.painter(), ORIGIN, &buf, None);
                if self.passes == 1 {
                    ui.ctx().request_discard("a test's first pass");
                }
            }
            fn ready(&self) -> bool {
                true
            }
        }
        let mut drv = Driver::new(600.0, 400.0);
        let mut win = Discarding { passes: 0 };
        drv.frame(&mut win, Vec::new(), Modifiers::NONE);
        assert_eq!(win.passes, 2, "egui ran the pass again");
        assert_eq!(drv.layers.len(), 1);
        assert_eq!(drv.find_text("new"), Some(at(0, 0, 3)));
        assert_eq!(drv.find_text("old"), None);
    }
}
