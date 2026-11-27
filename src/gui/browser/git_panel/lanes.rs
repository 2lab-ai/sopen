//! Lanes as glyphs (10 §6; 03 §4.3–§4.4): the Graph column of the visible rows in cells, each
//! cell's glyph and colour, from `git::graph`'s node segments and the visible rows'
//! pass-throughs ([`Graph::pass_throughs`]). Pure: it draws nothing; the panel draws its cells
//! (X4-graph), as `graph_view::lanes_of` draws the same pieces as egui lines today.
//!
//! - **Two cells a lane** (03 §4.2): a lane's first cell holds its node or its line, its second
//!   the line of a lane change. The row stream keeps one row a commit (03 §4.3) and a cell holds
//!   no diagonal, so a lane change runs along its node's row: `─` from the node to the lane, and
//!   a corner there — `┐` down to a new lane, `┘` up from a lane that joins.
//! - **Where pieces meet** a cell joins their sides (`chrome::git::line`): `├ ┤ ┬ ┴` where lines
//!   join, `┼` where a lane change crosses a line passing by. A join comes only from a lane right
//!   of the node, which takes the lowest lane awaiting it (`Graph::push`'s column, 03 §4.3 step
//!   2), and a line to a lane left of it ends on the line passing by there, `├`: so `└` never
//!   stands, nor `┌` alone. Every glyph is code page 437's, which the VGA font holds: it has no
//!   rounded corner (`╭ ╮ ╰ ╯`).
//! - **The uncommitted row's line** to HEAD's row is `:` a cell, dim ([`look::DOTTED`]): CP437
//!   has no dashed line. It runs straight down its lane — the uncommitted row is row 0, on lane
//!   0, and HEAD's node takes the lowest lane awaiting it — so only a line passing by is dotted;
//!   a cell where a solid line meets it is that line's glyph.
//! - **Colour**: a cell takes the colour of the piece drawn last — the lines passing by, then
//!   the row's lane changes, then the node, as `lanes_of` draws lines, segments and nodes — and
//!   the lane changes among themselves longest first (on a tie the one coming in before the one
//!   going down; egui's diagonals share no stretch, so `lanes_of` needs no such order). So a
//!   crossing is the crossing line's colour, a stretch two lane changes share is the nearer
//!   lane's, and the colour changes where a line turns. A lane change is the colour of the lane
//!   it ends on (the layout gives it that occupancy's), so where it joins a line passing by the
//!   two are one colour — but on the dotted line, which is dim, as the uncommitted row's node is
//!   (egui's too). A join a longer lane change goes on past is `┼`, as a crossing is: only that
//!   colour tells them apart, and two lanes five apart share it (03 §4.3's limit).
//! - **The cut** (03 §4.2): a row with a piece on a lane past those shown has `»` in the cut's
//!   first cell, dim, as `lanes_of` marks it; the pieces stop at the column's edge. A column of no
//!   cells (a panel under 6 cells wide) has no `»` either, where `lanes_of` paints one over the
//!   description. Nothing past the lanes shown is drawn, so a call's work is bounded by the
//!   column, not by the history's width.
//! - **An ambiguous-wide terminal** (`wide`, 10 §10, T38) gets §10's stand-ins here, at the
//!   source, as the viewers' own glyphs do: `| - +` for box lines, `#` for `■`, `.` for `≡` and
//!   `○`; `◙`, `»` and `:` are narrow and stay.

#![cfg_attr(not(test), allow(dead_code))] // X4-graph draws through it.

use super::graph_view::Columns;
use crate::git::graph::{End, Graph, Node, RowGraph};
use crate::gui::chrome::git::{self as look, LOOK};
use crate::gui::term;
use eframe::egui::Color32;
use std::cmp::Reverse;
use std::ops::Range;

#[cfg(test)]
mod tests;

#[cfg(test)]
thread_local! {
    /// The cells a call has allocated and drawn on this thread: its work, which the tests bound
    /// by the column's cells.
    static WORK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts `cells` of work ([`WORK`], in the tests only).
fn work(cells: usize) {
    #[cfg(test)]
    WORK.with(|work| work.set(work.get() + cells));
    #[cfg(not(test))]
    let _ = cells;
}

/// A cell of the Graph column: its glyph and colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub glyph: char,
    pub colour: Color32,
}

/// What the pieces drawn so far left in a cell.
#[derive(Debug, Clone, Copy)]
enum Ink {
    Blank,
    /// Lines, by the sides they leave the cell's middle to (up, down, left, right): the colour of
    /// the last drawn, and whether any of them is solid.
    Line {
        sides: [bool; 4],
        colour: Color32,
        solid: bool,
    },
    Node(Cell),
}

impl Ink {
    /// A line to `sides` drawn over the cell: its sides join the ones there, its colour wins.
    fn draw(&mut self, sides: [bool; 4], colour: Color32, dashed: bool) {
        work(1);
        let (old, was_solid) = match *self {
            Ink::Line { sides, solid, .. } => (sides, solid),
            _ => ([false; 4], false),
        };
        *self = Ink::Line {
            sides: [0, 1, 2, 3].map(|k| old[k] || sides[k]),
            colour,
            solid: was_solid || !dashed,
        };
    }

    fn cell(self) -> Option<Cell> {
        match self {
            Ink::Blank => None,
            Ink::Line {
                sides: [up, down, left, right],
                colour,
                solid,
            } => Some(Cell {
                glyph: if !solid && up && down && !left && !right {
                    look::DOTTED
                } else {
                    look::line(up, down, left, right)
                },
                colour,
            }),
            Ink::Node(cell) => Some(cell),
        }
    }
}

/// A line passing by and a lane change's stretch, by their sides as [`Ink::Line`] lists them.
const ALONG: [bool; 4] = [true, true, false, false];
const ACROSS: [bool; 4] = [false, false, true, true];

/// The Graph column of `rows` (clamped to the rows laid out), in the columns `cols` gives it: a
/// list of `cols.graph` cells for each row, `None` where a cell is blank. `wide`: the cells go to
/// an ambiguous-wide terminal (10 §10), which gets the stand-ins.
///
/// Its work is bounded by the column, not by the history's width: only the lanes shown have
/// cells, a lane change is drawn only across them, and a piece on a lane past them only marks its
/// row as cut.
pub fn rows(
    graph: &Graph,
    rows: Range<usize>,
    cols: &Columns,
    wide: bool,
) -> Vec<Vec<Option<Cell>>> {
    let end = rows.end.min(graph.len());
    let start = rows.start.min(end);
    let lanes = cols.lanes;
    let shown = (2 * lanes as usize).min(cols.graph);
    let mut ink = vec![vec![Ink::Blank; shown]; end - start];
    work(shown * (end - start));
    // Rows a line passing by past the lanes shown crosses, counted where its run starts and ends.
    let mut hidden = vec![0i32; end - start + 1];
    for line in graph.pass_throughs(start..end) {
        let (from, to) = (line.rows.start - start, line.rows.end - start);
        let x = 2 * line.slot as usize;
        if line.slot >= lanes || x >= shown {
            hidden[from] += 1;
            hidden[to] -= 1;
            continue;
        }
        let colour = colour(line.colour, line.dashed);
        for row in &mut ink[from..to] {
            row[x].draw(ALONG, colour, line.dashed);
        }
    }
    let mut past: Vec<bool> = (hidden.iter())
        .scan(0, |crossing, n| {
            *crossing += n;
            Some(*crossing > 0)
        })
        .take(end - start)
        .collect();
    for ((row, past), i) in ink.iter_mut().zip(&mut past).zip(start..end) {
        let node = graph.row(i);
        let at = 2 * node.column as usize;
        *past |= node.column >= lanes;
        let mut changes: Vec<_> = (graph.segments(i).iter())
            .filter(|s| s.slot != node.column)
            .collect();
        // By its whole length, cut or not, so a cut changes no colour.
        changes.sort_by_key(|s| (Reverse(s.slot.abs_diff(node.column)), s.end));
        for s in changes {
            *past |= s.slot >= lanes;
            let colour = colour(s.colour, s.dashed);
            let to = 2 * s.slot as usize;
            // The corner at the lane: up or down, and back toward the node.
            let up = s.end == End::Top;
            let (between, corner) = if to > at {
                (at + 1..to, [up, !up, true, false])
            } else {
                (to + 1..at, [up, !up, false, true])
            };
            for cell in &mut row[between.start.min(shown)..between.end.min(shown)] {
                cell.draw(ACROSS, colour, s.dashed);
            }
            if let Some(cell) = row.get_mut(to) {
                cell.draw(corner, colour, s.dashed);
            }
        }
        if let Some(cell) = row.get_mut(at) {
            *cell = Ink::Node(node_cell(node));
        }
    }
    let cut = Cell {
        glyph: look::CUT,
        colour: LOOK.cut,
    };
    (ink.into_iter().zip(past))
        .map(|(row, past)| {
            let mut cells: Vec<Option<Cell>> = row.into_iter().map(Ink::cell).collect();
            cells.resize(cols.graph, None);
            if cols.cut && shown < cols.graph && past {
                cells[shown] = Some(cut);
            }
            for cell in cells.iter_mut().flatten() {
                cell.glyph = stand_in(cell.glyph, wide);
            }
            cells
        })
        .collect()
}

/// A line's colour: its lane's, the uncommitted row's dim for its dotted line (03 §4.4).
fn colour(lane: u32, dashed: bool) -> Color32 {
    if dashed {
        LOOK.uncommitted
    } else {
        look::lane(lane)
    }
}

/// A row's node (03 §4.4): `■` `◙` `≡` in its lane's colour, `○` dim.
fn node_cell(node: &RowGraph) -> Cell {
    let (glyph, colour) = match node.node {
        Node::Commit => (look::COMMIT, look::lane(node.colour)),
        Node::CheckedOut => (look::CHECKED_OUT, look::lane(node.colour)),
        Node::Stash => (look::STASH, look::lane(node.colour)),
        Node::Uncommitted => (look::UNCOMMITTED, LOOK.uncommitted),
    };
    Cell { glyph, colour }
}

/// `glyph` as a terminal that draws a glyph of ambiguous width in two cells (`wide`) is sent it:
/// §10's stand-in (`term::narrow`), `.` for one it has none for; as it is everywhere else.
fn stand_in(glyph: char, wide: bool) -> char {
    if !wide || !term::ambiguous(glyph) {
        return glyph;
    }
    let stand_in = term::narrow(glyph.encode_utf8(&mut [0u8; 4]));
    stand_in.and_then(|s| s.chars().next()).unwrap_or('.')
}
