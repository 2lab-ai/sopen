//! A file panel on the cell grid (10 §4, X1a): the frame with the path in its top border, the
//! column heads, the rows — the bar, a selected entry in the selection's colour, the entry being
//! renamed as its field in its row's cells (`ops`' field: egui edits, the cells show it, its
//! caret the buffer's) — the scroll bar on the right border, the rule and the foot, written into
//! a buffer of the side's cells and painted by [`cells::paint`],
//! a glyph per cell. Every width is a count of cells (unicode-width), so a Hangul name takes
//! two cells a glyph and a cut never splits one (10 §10, 01 §3a). The pointer stays egui's: each
//! row is an `interact` rect at its cells, the wheel is read over the side.

use super::*;
use crate::gui::cells::{Buffer, Cells, Pen};

/// The columns, in cells: Ext, Size, Date and Time; Name takes what is left.
const EXT: u16 = 5;
const SIZE: u16 = 14;
const DATE: u16 = 10;
const TIME: u16 = 5;

/// Where a panel's columns start, in cells from the inside of its frame: Name (what is left) ·
/// Ext · Size (right-aligned, ending at `size_end`) · Date · Time, a blank cell between each;
/// Date and Time only from 48 cells on.
struct Columns {
    name: u16,
    ext: u16,
    size_end: u16,
    date: Option<(u16, u16)>,
}

impl Columns {
    fn of(inner: Cells) -> Columns {
        let chars = inner.width.max(10);
        let dated = chars >= 48;
        let fixed = EXT + SIZE + 2 + if dated { DATE + TIME + 2 } else { 0 };
        let name = chars.saturating_sub(fixed + 1).max(4);
        let at = |c: u16| inner.x + c;
        Columns {
            name,
            ext: at(name + 1),
            size_end: at(name + 1 + EXT + 1 + SIZE),
            date: dated.then(|| (at(name + EXT + SIZE + 4), at(name + EXT + SIZE + DATE + 5))),
        }
    }
}

impl Browser {
    /// Panel `i` at cells `side` of `grid`. A click puts the bar on a row (and makes the panel
    /// active); a double click opens it.
    pub(super) fn panel_cells(&mut self, ui: &mut egui::Ui, i: usize, grid: &Grid, side: Cells) {
        let ctx = ui.ctx().clone();
        let rect = grid.rect(side);
        let p = chrome::Palette::of(ui.visuals());
        let active = i == self.active && self.viewer.is_none() && !self.git.focus;
        let title = show(self.label.as_deref(), &self.panels[i].dir);
        let mut buf = Buffer::empty(side);
        let inner = cells::frame(&mut buf, side, &title, active);
        let painter = ui.painter_at(rect);
        // The side's body under its cells, as a hosted pane's frame fills its own.
        painter.rect_filled(rect, 0.0, p.back);
        // The head, the rows, the rule and the foot: four rows at least.
        if inner.width == 0 || inner.height < 4 {
            if i == self.active {
                self.visible = 1;
            }
            cells::paint(&painter, grid.origin, &buf, None);
            return;
        }
        let cols = Columns::of(inner);
        let dim = Pen::new(p.dim, p.back);
        let head = inner.y;
        cells::put(&mut buf, inner.x, head, "Name", cols.name.into(), dim);
        cells::put(&mut buf, cols.ext, head, "Ext", EXT.into(), dim);
        cells::put_right(&mut buf, cols.size_end, head, "Size", dim);
        if let Some((date, time)) = cols.date {
            cells::put(&mut buf, date, head, "Date", DATE.into(), dim);
            cells::put(&mut buf, time, head, "Time", TIME.into(), dim);
        }
        let rows_top = head + 1;
        let foot = inner.bottom() - 1;
        let rule = foot - 1;
        let visible = usize::from(rule - rows_top).max(1);
        if i == self.active {
            self.visible = visible;
        }
        // The entry being renamed (F2), in the panel it was pressed in: its row is the field.
        let renaming = (i == self.rename_panel)
            .then(|| self.ops.renaming().map(str::to_string))
            .flatten();
        let mut field = None;
        let panel = &mut self.panels[i];
        // The wheel scrolls the rows (the bar stays where it is).
        if pointer_on_pane(ui, rect) {
            let lines: f32 = ctx.input(|inp| {
                inp.events
                    .iter()
                    .map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, .. } => match unit {
                            egui::MouseWheelUnit::Point => -delta.y / chrome::CELL.y,
                            _ => -delta.y * 3.0,
                        },
                        _ => 0.0,
                    })
                    .sum()
            });
            let max_top = panel.entries.len().saturating_sub(visible);
            panel.top = (panel.top as f32 + lines)
                .round()
                .clamp(0.0, max_top as f32) as usize;
        }
        if i == self.active {
            panel.follow(visible);
        }
        let mut clicked = None;
        for (row, idx) in (panel.top..panel.entries.len()).take(visible).enumerate() {
            let entry = &panel.entries[idx];
            let y = rows_top + row as u16;
            let cells_of_row = Cells::new(inner.x, y, inner.width, 1);
            let on_bar = idx == panel.highlight && active;
            let back = match (idx == panel.highlight, active) {
                (false, _) => p.back,
                (true, true) => p.bar,
                (true, false) => p.bar_idle,
            };
            let pen = Pen::new(
                ops::entry_color(&p, entry, panel.is_selected(entry), on_bar),
                back,
            );
            cells::fill(&mut buf, cells_of_row, pen);
            // Each cell fits its column at a glyph boundary, "…" where it is cut: never half a
            // wide glyph. A size too long for its column is said in a larger unit, never cut.
            if idx == panel.highlight && renaming.as_deref() == Some(entry.name.as_str()) {
                // The name is a field over the Name and Ext columns, written after the rows.
                let width = cols.ext + EXT - inner.x;
                field = Some(Cells::new(inner.x, y, width, 1));
            } else {
                let (name, ext) = panel::split_name(entry);
                let name = cells::fit_start(name, cols.name.into());
                cells::put(&mut buf, inner.x, y, &name, cols.name.into(), pen);
                let ext = cells::fit_start(ext, EXT.into());
                cells::put(&mut buf, cols.ext, y, &ext, EXT.into(), pen);
            }
            let size = panel::size_in(entry, SIZE.into());
            cells::put_right(&mut buf, cols.size_end, y, &size, pen);
            if let Some((date, time)) = cols.date {
                let (d, t) = panel::date_cells(entry);
                cells::put(&mut buf, date, y, &d, DATE.into(), pen);
                cells::put(&mut buf, time, y, &t, TIME.into(), pen);
            }
            let resp = ui.interact(
                grid.rect(cells_of_row),
                list_id().with((i, idx)),
                Sense::click(),
            );
            if resp.clicked() {
                let pair = list_id().with((i, &panel.dir, &entry.name));
                clicked = Some((idx, second_click(&resp, pair)));
            }
        }
        // The scroll bar on the right border, beside the rows; the rule across the frame.
        let max_top = panel.entries.len().saturating_sub(visible);
        let at = (max_top > 0).then(|| panel.top as f32 / max_top as f32);
        cells::scroll_bar(&mut buf, inner.right(), inner.y, rule - inner.y, at);
        cells::rule(&mut buf, side, rule, active);
        // The foot, centred when it fits inside the frame; else without the free space; else
        // the count from the left, its end cut — the count is never lost.
        let (listed, free) = panel.foot_parts();
        let whole = match &free {
            Some(free) => format!("{listed} · {free}"),
            None => listed.clone(),
        };
        let room = usize::from(inner.width);
        let (shown, centred) = if cells::width(&whole) <= room {
            (whole, true)
        } else if cells::width(&listed) <= room {
            (listed, true)
        } else {
            (cells::fit_start(&listed, room), false)
        };
        let gap = if centred {
            (room - cells::width(&shown)) / 2
        } else {
            0
        };
        let body = Pen::new(p.text, p.back);
        cells::put(&mut buf, inner.x + gap as u16, foot, &shown, room, body);
        // A dialog over the list: the cells under it blank first (cells.rs, one buffer over
        // another), so a wide glyph half under its edge goes as a terminal erases it.
        for under in self.ops.covered(grid) {
            cells::fill(&mut buf, under, body);
        }
        // The field after the rows' interactions — on top of its row's, it takes its clicks —
        // written into these cells, its caret theirs.
        let caret = field.and_then(|at| self.ops.rename_field(ui, grid, at, &mut buf));
        cells::paint(&painter, grid.origin, &buf, caret);
        if let Some((idx, open)) = clicked {
            // While the tree is open the other side is a snapshot (02 §3.1 O4): a click there
            // moves nothing.
            let free = self.listing.is_empty() && self.opening.is_none() && self.tree.is_none();
            if free && self.viewer.is_none() && !self.ops.holds_keyboard() {
                self.active = i;
                // A click in a list gives it the keyboard back from a git panel.
                self.git.focus = false;
                self.git.wide = false;
                self.panels[i].highlight = idx;
                if open {
                    self.enter(&ctx, false);
                }
            }
        }
    }
}
