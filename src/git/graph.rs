//! The lane layout (03 §4.3): pure and deterministic — rows in display order in, a [`RowGraph`]
//! per row out; the same input always gives the same output.
//!
//! # For the panel (S2-D)
//!
//! - **Rows**: [`RowStream`] makes them (03 §4.3 "The row stream"). [`RowStream::new`] takes
//!   HEAD's commit, whether there are uncommitted changes, the stash read and the Stashes filter
//!   (§4.6); [`RowStream::page`] takes each page of the log in order and returns the [`Row`]s it
//!   adds: the uncommitted row first, stash internals dropped, each stash row immediately above
//!   its base's row (newest first; one whose base is not loaded yet waits for it). An empty page
//!   still starts the stream (an empty repository's uncommitted row). The panel appends them;
//!   rows above never move. A soft refresh that only adds or removes the uncommitted row (§5)
//!   changes row 0: the panel inserts or removes [`Row::Uncommitted`] itself and lays the loaded
//!   rows out again with [`layout`] — every row index shifts by one, and lanes below can move,
//!   since the uncommitted row takes a slot like any row.
//! - **Layout**: [`layout`] for a whole list, or [`Graph::new`] and [`Graph::extend`] with each
//!   page's rows — the same result: paging never redraws rows above. `first_parent` is the
//!   "First parent only" filter (§4.6): git's `%P` still lists every parent under
//!   `--first-parent` (*verified, git 2.56.1*), so the layout is told to draw the first alone.
//! - **Per row `i`**: [`Graph::row`] gives the node's slot ([`RowGraph::column`]), its kind
//!   ([`RowGraph::node`]: §4.4's `■` `◙` `≡` `○`) and colour; [`Graph::segments`] the straight
//!   lines from the node (mid-row, at its column) to a slot on the row's top or bottom edge
//!   ([`End`]) — vertical when that slot is the column, else one diagonal across half a row:
//!   into the node from above, joins, and out to each parent's slot.
//! - **Pass-throughs** are not stored: [`Graph::pass_throughs`] derives, for the visible rows
//!   only, the runs of rows a slot's line crosses top to bottom without touching a node. A line
//!   still awaiting a commit that is not loaded runs through the last loaded row.
//! - **Width**: [`Graph::width`] is the most slots any loaded row uses: the Graph column's lanes
//!   (2 cells each, cut with `»`, §4.2).
//! - **Colours are indices**: each slot occupancy takes the next one in turn and keeps it down
//!   its first-parent line ([`Interval::colour`]); a node, a segment and a pass-through carry the
//!   colour of the occupancy they draw. Painting takes it modulo `chrome::git`'s lane colours.
//! - **Dashed**: the uncommitted row's line to HEAD's row (§4.4, dim) — its bottom segment, the
//!   pass-throughs down to HEAD's row and the top segment into HEAD's node have `dashed` set;
//!   every other piece is solid.
//! - **Storage** (§4.5): a 16-byte [`RowGraph`] per row and its node segments (about 2, 12 bytes
//!   each, in one list), plus one [`Interval`] per slot occupancy — proportional to rows and
//!   edges, never rows × lanes.

use crate::git::model::{CommitRow, Oid, StashRow};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// A row of the graph, in display order: what [`RowStream`] makes of the log and the panel
/// shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The uncommitted row (§4.3): one parent, HEAD's commit — none in an empty repository.
    Uncommitted { head: Option<Oid> },
    /// A stash row: one parent, the stash's base.
    Stash(StashRow),
    /// A commit of the log; `checked_out` when it is HEAD's commit.
    Commit {
        commit: CommitRow,
        checked_out: bool,
    },
}

impl Row {
    /// What its node is drawn as (§4.4).
    pub fn node(&self) -> Node {
        match self {
            Row::Uncommitted { .. } => Node::Uncommitted,
            Row::Stash(_) => Node::Stash,
            Row::Commit {
                checked_out: true, ..
            } => Node::CheckedOut,
            Row::Commit { .. } => Node::Commit,
        }
    }

    /// The commit the row draws: a log commit's or a stash's own; the uncommitted row has none.
    pub fn oid(&self) -> Option<&Oid> {
        match self {
            Row::Uncommitted { .. } => None,
            Row::Stash(stash) => Some(&stash.oid),
            Row::Commit { commit, .. } => Some(&commit.oid),
        }
    }

    /// The parents its lines go to, the first parent first.
    pub fn parents(&self) -> &[Oid] {
        match self {
            Row::Uncommitted { head } => head.as_slice(),
            Row::Stash(stash) => std::slice::from_ref(&stash.base),
            Row::Commit { commit, .. } => &commit.parents,
        }
    }
}

/// A row's node (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// `■` in its lane's colour.
    Commit,
    /// The checked-out commit: `◙`.
    CheckedOut,
    /// `≡`.
    Stash,
    /// `○` in the dim colour.
    Uncommitted,
}

/// The row stream (§4.3), built page by page before layout.
#[derive(Debug, Clone)]
pub struct RowStream {
    head: Option<Oid>,
    /// The uncommitted row is still to come.
    uncommitted: bool,
    /// Every stash's own commit and its index and untracked parents: dropped wherever they appear.
    internals: HashSet<Oid>,
    /// Stash rows not yet shown, by base, newest first.
    waiting: HashMap<Oid, Vec<StashRow>>,
}

impl RowStream {
    /// A stream for one load: `head` is HEAD's commit (`None` in an empty repository),
    /// `uncommitted` whether there are changes, `stashes` the stash read (newest first) and
    /// `show_stashes` the Stashes filter — its internals are dropped either way.
    pub fn new(
        head: Option<Oid>,
        uncommitted: bool,
        stashes: Vec<StashRow>,
        show_stashes: bool,
    ) -> RowStream {
        let mut internals = HashSet::new();
        let mut waiting: HashMap<Oid, Vec<StashRow>> = HashMap::new();
        for stash in stashes {
            internals.insert(stash.oid.clone());
            internals.extend(stash.index.iter().chain(&stash.untracked).cloned());
            if show_stashes {
                waiting.entry(stash.base.clone()).or_default().push(stash);
            }
        }
        RowStream {
            head,
            uncommitted,
            internals,
            waiting,
        }
    }

    /// The rows the next page of the log adds, in display order: the first page's start with the
    /// uncommitted row (an empty page too); a stash row enters when its base's record arrives.
    pub fn page(&mut self, commits: Vec<CommitRow>) -> Vec<Row> {
        let mut rows = Vec::with_capacity(commits.len() + 1);
        if std::mem::take(&mut self.uncommitted) {
            rows.push(Row::Uncommitted {
                head: self.head.clone(),
            });
        }
        for commit in commits {
            if self.internals.contains(&commit.oid) {
                continue;
            }
            if let Some(stashes) = self.waiting.remove(&commit.oid) {
                rows.extend(stashes.into_iter().map(Row::Stash));
            }
            let checked_out = self.head.as_ref() == Some(&commit.oid);
            rows.push(Row::Commit {
                commit,
                checked_out,
            });
        }
        rows
    }
}

/// Lays out `rows`, in display order (children above their parents): `first_parent` draws only
/// each row's first parent.
pub fn layout(rows: &[Row], first_parent: bool) -> Graph {
    let mut graph = Graph::new(first_parent);
    graph.extend(rows);
    graph
}

/// The layout of the rows so far, and the slots awaiting commits below them (§4.3 steps 1–7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    first_parent: bool,
    rows: Vec<RowGraph>,
    /// Every row's node segments, row after row.
    segments: Vec<Segment>,
    /// One per slot occupancy, in the order they began (so by `first`).
    intervals: Vec<Interval>,
    /// The slots at the bottom of the last row: empty, or awaiting a commit.
    lanes: Vec<Option<Lane>>,
    width: u32,
}

/// A slot awaiting a commit: a parent seen above, not drawn yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lane {
    awaits: Oid,
    /// Its occupancy, an index into `intervals`.
    interval: usize,
}

/// What the layout stores for one row (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowGraph {
    /// The node's slot.
    pub column: u32,
    pub node: Node,
    /// The colour index of the occupancy the node is on.
    pub colour: u32,
    /// Where its segments start in the graph's list; the next row's start ends them.
    segments: u32,
}

/// A straight line from a row's node to `slot` on the row's top or bottom edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub end: End,
    pub slot: u32,
    pub colour: u32,
    /// A piece of the uncommitted row's line to HEAD's row.
    pub dashed: bool,
}

/// Which edge of the row a [`Segment`] runs to from the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum End {
    /// Into the node from above.
    Top,
    /// Out of the node to a parent's slot.
    Bottom,
}

/// One slot occupancy (§4.3): from the row whose node starts it to the row whose node ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub slot: u32,
    /// The occupancy's colour index: the next in turn when it began.
    pub colour: u32,
    pub first: u32,
    /// `None` while it still awaits a commit: it runs to the bottom of the loaded rows.
    pub last: Option<u32>,
    /// Its rows above this one are the uncommitted row's dashed line (0: none; `u32::MAX`:
    /// HEAD's row is not loaded yet).
    dashed_to: u32,
}

/// A vertical line through `rows` at `slot`, top edge to bottom edge, touching no node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassThrough {
    pub slot: u32,
    /// Its occupancy's colour index.
    pub colour: u32,
    pub rows: Range<usize>,
    /// A piece of the uncommitted row's line to HEAD's row.
    pub dashed: bool,
}

impl Graph {
    /// An empty layout; `first_parent` draws only each row's first parent.
    pub fn new(first_parent: bool) -> Graph {
        Graph {
            first_parent,
            rows: Vec::new(),
            segments: Vec::new(),
            intervals: Vec::new(),
            lanes: Vec::new(),
            width: 0,
        }
    }

    /// Lays out `rows` below the rows so far.
    pub fn extend(&mut self, rows: &[Row]) {
        for row in rows {
            self.push(row);
        }
    }

    /// Lays out one row below the rows so far (§4.3 steps 2–7).
    pub fn push(&mut self, row: &Row) {
        let r = self.rows.len() as u32;
        let first_segment = self.segments.len() as u32;
        let node = row.node();
        let id = row.oid();

        // 2. Column: the lowest slot awaiting this commit; a branch tip's is the lowest empty
        // slot, else a new slot at the end.
        let awaited = id.and_then(|id| self.lanes.iter().position(|lane| awaits(lane, id)));
        let column = awaited.unwrap_or_else(|| self.empty_slot(0));

        // Lines into the node from above: its column's, and 3. the joins, whose slots empty.
        let mut continued = None;
        if let Some(id) = id {
            for (slot, lane) in self.lanes.iter_mut().enumerate().skip(column) {
                let Some(index) = lane
                    .as_ref()
                    .filter(|lane| &lane.awaits == id)
                    .map(|lane| lane.interval)
                else {
                    continue;
                };
                let occupancy = &mut self.intervals[index];
                let dashed = occupancy.dashed_to == u32::MAX;
                if dashed {
                    occupancy.dashed_to = r;
                }
                self.segments.push(Segment {
                    end: End::Top,
                    slot: slot as u32,
                    colour: occupancy.colour,
                    dashed,
                });
                if slot == column {
                    continued = Some(index);
                } else {
                    occupancy.last = Some(r);
                    *lane = None;
                }
            }
        }
        // The column's occupancy goes on down its first-parent line; a tip begins one.
        let interval = continued.unwrap_or_else(|| self.open(column, r));
        let colour = self.intervals[interval].colour;

        let parents = row.parents();
        let parents = if self.first_parent {
            &parents[..parents.len().min(1)]
        } else {
            parents
        };
        for (i, parent) in parents.iter().enumerate() {
            // A parent listed twice is drawn once.
            if parents[..i].contains(parent) {
                continue;
            }
            if i == 0 {
                // 4. The first parent takes over the column's slot, even when another slot
                // already awaits it: the two lines join at the parent's row.
                let dashed = node == Node::Uncommitted;
                if dashed {
                    self.intervals[interval].dashed_to = u32::MAX;
                }
                self.lanes[column] = Some(Lane {
                    awaits: parent.clone(),
                    interval,
                });
                self.segments.push(Segment {
                    end: End::Bottom,
                    slot: column as u32,
                    colour,
                    dashed,
                });
                continue;
            }
            // 5. Another parent: a line to the slot already awaiting it, else to the lowest empty
            // slot right of the column, else to a new slot at the end.
            let found = self.lanes.iter().enumerate().find_map(|(slot, lane)| {
                lane.as_ref()
                    .filter(|lane| &lane.awaits == parent)
                    .map(|lane| (slot, lane.interval))
            });
            let (slot, index) = match found {
                Some(found) => found,
                None => {
                    let slot = self.empty_slot(column + 1);
                    let index = self.open(slot, r);
                    self.lanes[slot] = Some(Lane {
                        awaits: parent.clone(),
                        interval: index,
                    });
                    (slot, index)
                }
            };
            self.segments.push(Segment {
                end: End::Bottom,
                slot: slot as u32,
                colour: self.intervals[index].colour,
                dashed: false,
            });
        }
        if parents.is_empty() {
            // 6. A root: no line leaves the node downward; the next row may take the slot.
            self.intervals[interval].last = Some(r);
            self.lanes[column] = None;
        }

        self.width = self.width.max(self.lanes.len() as u32);
        // 7. Trailing empty slots are dropped.
        while matches!(self.lanes.last(), Some(None)) {
            self.lanes.pop();
        }
        self.rows.push(RowGraph {
            column: column as u32,
            node,
            colour,
            segments: first_segment,
        });
    }

    /// The lowest empty slot from `from` on, else a new slot at the end.
    fn empty_slot(&mut self, from: usize) -> usize {
        match self.lanes.iter().skip(from).position(Option::is_none) {
            Some(i) => from + i,
            None => {
                self.lanes.push(None);
                self.lanes.len() - 1
            }
        }
    }

    /// Begins an occupancy of `slot` at row `first`, with the next colour in turn.
    fn open(&mut self, slot: usize, first: u32) -> usize {
        let index = self.intervals.len();
        self.intervals.push(Interval {
            slot: slot as u32,
            colour: index as u32,
            first,
            last: None,
            dashed_to: 0,
        });
        index
    }

    /// The rows laid out.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row `i`'s column, node and colour (`i` below [`Graph::len`], as indexing).
    pub fn row(&self, i: usize) -> &RowGraph {
        &self.rows[i]
    }

    /// Row `i`'s node segments (`i` below [`Graph::len`], as indexing).
    pub fn segments(&self, i: usize) -> &[Segment] {
        let start = self.rows[i].segments as usize;
        let end = self
            .rows
            .get(i + 1)
            .map_or(self.segments.len(), |next| next.segments as usize);
        &self.segments[start..end]
    }

    /// All rows' node segments: the layout's per-row storage.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// The slot occupancies, in the order they began.
    pub fn intervals(&self) -> &[Interval] {
        &self.intervals
    }

    /// The most slots any row uses.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The pass-through lines of `rows` (clamped to the rows laid out), derived from the
    /// occupancies: a slot's line crosses each row strictly inside its occupancy whose node is
    /// in another slot — or, still open, each row below its first down to the last loaded row.
    pub fn pass_throughs(&self, rows: Range<usize>) -> Vec<PassThrough> {
        let end = rows.end.min(self.rows.len());
        let start = rows.start.min(end);
        let mut lines = Vec::new();
        // Occupancies begin in row order; one beginning at `end` or below crosses no row here.
        let began = self.intervals.partition_point(|o| (o.first as usize) < end);
        for occupancy in &self.intervals[..began] {
            let slot = occupancy.slot;
            let from = (occupancy.first as usize + 1).max(start);
            let to = occupancy
                .last
                .map_or(self.rows.len(), |last| last as usize)
                .min(end);
            // A node on the slot inside the occupancy is on its first-parent line: it parts runs;
            // `to` ends the last.
            let nodes = self.rows.iter().enumerate().take(to).skip(from);
            let parts = nodes.filter(|(_, row)| row.column == slot).map(|(r, _)| r);
            let mut run = from;
            for part in parts.chain([to]) {
                if run < part {
                    lines.push(PassThrough {
                        slot,
                        colour: occupancy.colour,
                        rows: run..part,
                        // No run crosses the node that ends the dash.
                        dashed: part as u64 <= u64::from(occupancy.dashed_to),
                    });
                }
                run = part + 1;
            }
        }
        lines
    }
}

/// `lane` awaits `oid`.
fn awaits(lane: &Option<Lane>, oid: &Oid) -> bool {
    lane.as_ref().is_some_and(|lane| &lane.awaits == oid)
}
