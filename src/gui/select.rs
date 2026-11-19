//! The rectangle selection shared by both windows: drawn by dragging with the primary button,
//! adjusted with eight handles, moved by dragging its inside. Kept in source pixels (image
//! pixels, or video source pixels when the frame on screen is a scaled decode).

use eframe::egui::{
    self, Color32, CursorIcon, PointerButton, Pos2, Rect, Response, Stroke, StrokeKind, Vec2,
};

/// Screen rectangle `disp` showing a `src`-sized source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Map {
    pub disp: Rect,
    pub src: Vec2,
}

impl Map {
    /// Screen point → source pixel position, clamped to the source.
    pub fn to_src(&self, p: Pos2) -> Pos2 {
        let r = (p - self.disp.min) / self.disp.size();
        Pos2::new(
            (r.x * self.src.x).clamp(0.0, self.src.x),
            (r.y * self.src.y).clamp(0.0, self.src.y),
        )
    }

    pub fn to_disp(&self, p: Pos2) -> Pos2 {
        self.disp.min + (p.to_vec2() / self.src) * self.disp.size()
    }

    pub fn rect_to_disp(&self, r: Rect) -> Rect {
        Rect::from_min_max(self.to_disp(r.min), self.to_disp(r.max))
    }

    /// Screen points per source pixel.
    pub fn scale(&self) -> f32 {
        self.disp.width() / self.src.x
    }
}

/// `src` scaled to fit `avail`, aspect kept, centred.
pub fn fit(avail: Rect, src: Vec2) -> Rect {
    let s = (avail.width() / src.x).min(avail.height() / src.y);
    Rect::from_center_size(avail.center(), src * s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

impl Handle {
    const ALL: [Handle; 8] = [
        Handle::NW,
        Handle::N,
        Handle::NE,
        Handle::E,
        Handle::SE,
        Handle::S,
        Handle::SW,
        Handle::W,
    ];

    /// Where the handle sits on `r`.
    fn pos(self, r: Rect) -> Pos2 {
        let (cx, cy) = (r.center().x, r.center().y);
        match self {
            Handle::N => Pos2::new(cx, r.top()),
            Handle::S => Pos2::new(cx, r.bottom()),
            Handle::E => Pos2::new(r.right(), cy),
            Handle::W => Pos2::new(r.left(), cy),
            Handle::NE => r.right_top(),
            Handle::NW => r.left_top(),
            Handle::SE => r.right_bottom(),
            Handle::SW => r.left_bottom(),
        }
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Handle::N | Handle::S => CursorIcon::ResizeVertical,
            Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
            Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
            Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
        }
    }

    /// `start` with this handle's edges moved to `p` (source space), normalised.
    pub fn drag(self, start: Rect, p: Pos2) -> Rect {
        let (mut x0, mut y0, mut x1, mut y1) = (start.min.x, start.min.y, start.max.x, start.max.y);
        match self {
            Handle::N => y0 = p.y,
            Handle::S => y1 = p.y,
            Handle::E => x1 = p.x,
            Handle::W => x0 = p.x,
            Handle::NE => (y0, x1) = (p.y, p.x),
            Handle::NW => (y0, x0) = (p.y, p.x),
            Handle::SE => (y1, x1) = (p.y, p.x),
            Handle::SW => (y1, x0) = (p.y, p.x),
        }
        Rect::from_two_pos(Pos2::new(x0, y0), Pos2::new(x1, y1))
    }
}

/// What a press at a screen point grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Handle(Handle),
    Inside,
}

/// Handle radius on screen, in points.
const GRAB: f32 = 7.0;

/// Handles win over the inside; a selection too small on screen to hold handles is moved.
pub fn hit(disp: Rect, p: Pos2) -> Option<Hit> {
    if disp.width() >= 3.0 * GRAB && disp.height() >= 3.0 * GRAB {
        if let Some(h) = Handle::ALL
            .into_iter()
            .find(|h| h.pos(disp).distance(p) <= GRAB)
        {
            return Some(Hit::Handle(h));
        }
    } else if let Some(h) = [Handle::NW, Handle::NE, Handle::SE, Handle::SW]
        .into_iter()
        .find(|h| h.pos(disp).distance(p) <= GRAB * 0.6)
    {
        return Some(Hit::Handle(h));
    }
    disp.contains(p).then_some(Hit::Inside)
}

/// Snaps a source-space rect to whole pixels inside `bounds`; `None` when it covers no pixel.
pub fn to_pixels(r: Rect, bounds: Vec2) -> Option<[u32; 4]> {
    let x0 = r.min.x.round().clamp(0.0, bounds.x);
    let y0 = r.min.y.round().clamp(0.0, bounds.y);
    let x1 = r.max.x.round().clamp(0.0, bounds.x);
    let y1 = r.max.y.round().clamp(0.0, bounds.y);
    (x1 > x0 && y1 > y0).then_some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
}

#[derive(Debug, Clone, Copy)]
enum Drag {
    New(Pos2),
    Move { grab: Pos2, start: Rect },
    Resize { handle: Handle, start: Rect },
}

#[derive(Debug, Default, Clone)]
pub struct Selection {
    /// Source space, whole pixels.
    rect: Option<Rect>,
    drag: Option<Drag>,
}

impl Selection {
    pub fn set(&mut self, [x, y, w, h]: [u32; 4]) {
        self.rect = Some(Rect::from_min_size(
            Pos2::new(x as f32, y as f32),
            Vec2::new(w as f32, h as f32),
        ));
        self.drag = None;
    }

    pub fn clear(&mut self) {
        self.rect = None;
        self.drag = None;
    }

    /// `[x, y, w, h]` in whole source pixels inside `bounds`.
    pub fn pixels(&self, bounds: Vec2) -> Option<[u32; 4]> {
        self.rect.and_then(|r| to_pixels(r, bounds))
    }

    /// A rectangle is drawn (Esc clears it; with none, Esc is not the window's).
    pub fn is_set(&self) -> bool {
        self.rect.is_some()
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Starts, updates or ends a drag from `resp` (the response of the area showing the
    /// source). Only the primary button selects; other buttons are left to the caller.
    pub fn handle(&mut self, ui: &egui::Ui, resp: &Response, map: Map) {
        let bounds = map.src;
        let pointer = resp.interact_pointer_pos();
        if resp.drag_started_by(PointerButton::Primary) {
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(p) = origin {
                let disp = self.rect.map(|r| map.rect_to_disp(r));
                self.drag = Some(match disp.and_then(|d| hit(d, p)) {
                    Some(Hit::Handle(handle)) => Drag::Resize {
                        handle,
                        start: self.rect.unwrap_or(Rect::NOTHING),
                    },
                    Some(Hit::Inside) => Drag::Move {
                        grab: map.to_src(p),
                        start: self.rect.unwrap_or(Rect::NOTHING),
                    },
                    None => Drag::New(map.to_src(p)),
                });
            }
        }
        if let (Some(drag), Some(p), true) =
            (self.drag, pointer, resp.dragged_by(PointerButton::Primary))
        {
            let q = map.to_src(p);
            let next = match drag {
                Drag::New(anchor) => Rect::from_two_pos(anchor, q),
                Drag::Resize { handle, start } => handle.drag(start, q),
                Drag::Move { grab, start } => {
                    let max = (bounds - start.size()).max(Vec2::ZERO);
                    let min = (start.min + (q - grab)).clamp(Pos2::ZERO, max.to_pos2());
                    Rect::from_min_size(min, start.size())
                }
            };
            self.rect = Some(Rect::from_min_max(next.min.round(), next.max.round()));
        }
        if resp.drag_stopped() {
            self.drag = None;
            if self.pixels(bounds).is_none() {
                self.rect = None;
            }
        }
        if resp.clicked_by(PointerButton::Primary) {
            let inside = pointer
                .zip(self.rect)
                .is_some_and(|(p, r)| map.rect_to_disp(r).expand(GRAB).contains(p));
            if !inside {
                self.clear();
            }
        }
        if let (Some(p), Some(r), None) = (resp.hover_pos(), self.rect, self.drag) {
            match hit(map.rect_to_disp(r), p) {
                Some(Hit::Handle(h)) => ui.ctx().set_cursor_icon(h.cursor()),
                Some(Hit::Inside) => ui.ctx().set_cursor_icon(CursorIcon::Move),
                None => ui.ctx().set_cursor_icon(CursorIcon::Crosshair),
            }
        } else if resp.hovered() && self.drag.is_none() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
    }

    /// Dims everything outside the selection inside `map.disp`, outlines it, draws the handles
    /// and a `w×h @ x,y` label.
    pub fn draw(&self, painter: &egui::Painter, map: Map) {
        let Some([x, y, w, h]) = self.pixels(map.src) else {
            return;
        };
        let src = Rect::from_min_size(Pos2::new(x as f32, y as f32), Vec2::new(w as f32, h as f32));
        let r = map.rect_to_disp(src);
        let shade = Color32::from_black_alpha(110);
        let d = map.disp;
        for part in [
            Rect::from_min_max(d.min, Pos2::new(d.max.x, r.min.y)),
            Rect::from_min_max(Pos2::new(d.min.x, r.max.y), d.max),
            Rect::from_min_max(Pos2::new(d.min.x, r.min.y), Pos2::new(r.min.x, r.max.y)),
            Rect::from_min_max(Pos2::new(r.max.x, r.min.y), Pos2::new(d.max.x, r.max.y)),
        ] {
            if part.is_positive() {
                painter.rect_filled(part, 0.0, shade);
            }
        }
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(1.5, Color32::YELLOW),
            StrokeKind::Outside,
        );
        if r.width() >= 3.0 * GRAB && r.height() >= 3.0 * GRAB {
            for handle in Handle::ALL {
                let c = handle.pos(r);
                let square = Rect::from_center_size(c, Vec2::splat(GRAB));
                painter.rect_filled(square, 1.0, Color32::YELLOW);
                painter.rect_stroke(
                    square,
                    1.0,
                    Stroke::new(1.0, Color32::BLACK),
                    StrokeKind::Inside,
                );
            }
        }
        let label = format!("{w}×{h} @ {x},{y}");
        let galley = painter.layout_no_wrap(label, egui::FontId::monospace(13.0), Color32::BLACK);
        let above = r.top() - galley.size().y - 6.0;
        let top = if above >= d.top() {
            above
        } else {
            r.top() + 4.0
        };
        let bg = Rect::from_min_size(Pos2::new(r.left(), top), galley.size()).expand(2.0);
        painter.rect_filled(bg, 2.0, Color32::YELLOW);
        painter.galley(bg.min + Vec2::splat(2.0), galley, Color32::BLACK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
    }

    #[test]
    fn map_round_trips_a_scaled_display() {
        // 1920×1080 source shown at 1280×720, offset by (40, 30).
        let m = Map {
            disp: Rect::from_min_size(Pos2::new(40.0, 30.0), Vec2::new(1280.0, 720.0)),
            src: Vec2::new(1920.0, 1080.0),
        };
        assert_eq!(m.to_src(Pos2::new(680.0, 390.0)), Pos2::new(960.0, 540.0));
        assert_eq!(
            m.to_disp(Pos2::new(1920.0, 1080.0)),
            Pos2::new(1320.0, 750.0)
        );
        // Outside the picture clamps to its edge.
        assert_eq!(m.to_src(Pos2::new(0.0, 2000.0)), Pos2::new(0.0, 1080.0));
        assert!((m.scale() - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn fit_keeps_aspect_and_centres() {
        let r = fit(rect(0.0, 0.0, 1000.0, 500.0), Vec2::new(400.0, 400.0));
        assert_eq!(r, rect(250.0, 0.0, 750.0, 500.0));
    }

    #[test]
    fn pixels_snap_and_clamp() {
        let b = Vec2::new(100.0, 50.0);
        assert_eq!(
            to_pixels(rect(9.6, 10.2, 20.4, 30.0), b),
            Some([10, 10, 10, 20])
        );
        assert_eq!(
            to_pixels(rect(90.0, 40.0, 140.0, 90.0), b),
            Some([90, 40, 10, 10])
        );
        assert_eq!(to_pixels(rect(5.0, 5.0, 5.2, 9.0), b), None);
        assert_eq!(to_pixels(rect(-20.0, -20.0, -1.0, 4.0), b), None);
    }

    #[test]
    fn handles_resize_and_normalise() {
        let start = rect(10.0, 10.0, 50.0, 40.0);
        assert_eq!(
            Handle::SE.drag(start, Pos2::new(70.0, 60.0)),
            rect(10.0, 10.0, 70.0, 60.0)
        );
        assert_eq!(
            Handle::E.drag(start, Pos2::new(80.0, 999.0)),
            rect(10.0, 10.0, 80.0, 40.0)
        );
        // Dragging the west edge past the east edge flips the rectangle instead of inverting it.
        assert_eq!(
            Handle::W.drag(start, Pos2::new(60.0, 0.0)),
            rect(50.0, 10.0, 60.0, 40.0)
        );
    }

    #[test]
    fn hits_prefer_handles_then_inside() {
        let d = rect(100.0, 100.0, 300.0, 200.0);
        assert_eq!(
            hit(d, Pos2::new(101.0, 99.0)),
            Some(Hit::Handle(Handle::NW))
        );
        assert_eq!(
            hit(d, Pos2::new(200.0, 203.0)),
            Some(Hit::Handle(Handle::S))
        );
        assert_eq!(
            hit(d, Pos2::new(299.0, 150.0)),
            Some(Hit::Handle(Handle::E))
        );
        assert_eq!(hit(d, Pos2::new(150.0, 130.0)), Some(Hit::Inside));
        assert_eq!(hit(d, Pos2::new(50.0, 50.0)), None);
        // A tiny selection on screen: only the corners are grabbable, the rest moves it.
        let tiny = rect(10.0, 10.0, 18.0, 16.0);
        assert_eq!(hit(tiny, Pos2::new(14.0, 13.0)), Some(Hit::Inside));
        assert_eq!(
            hit(tiny, Pos2::new(18.0, 16.0)),
            Some(Hit::Handle(Handle::SE))
        );
    }

    #[test]
    fn set_and_read_back() {
        let mut s = Selection::default();
        assert_eq!(s.pixels(Vec2::new(10.0, 10.0)), None);
        s.set([2, 3, 4, 5]);
        assert_eq!(s.pixels(Vec2::new(10.0, 10.0)), Some([2, 3, 4, 5]));
        // Clamped to a smaller source (an image that was cropped meanwhile).
        assert_eq!(s.pixels(Vec2::new(4.0, 4.0)), Some([2, 3, 2, 1]));
        s.clear();
        assert_eq!(s.pixels(Vec2::new(10.0, 10.0)), None);
    }
}
