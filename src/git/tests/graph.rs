//! The lane layout's tests (03 §4.3; G-G2): a table of small graphs with each row's column and
//! segments written out, the measured shapes of §4.3, and the properties over 1 000 graphs from
//! a fixed-seed generator.

use crate::git::graph::{layout, End, Graph, Node, PassThrough, Row, RowGraph, RowStream, Segment};
use crate::git::model::{CommitRow, Oid, StashRow};
use std::collections::{BTreeSet, HashMap, HashSet};

/// A test id: the name's bytes in hex, zero-padded to 40 digits, so a name prints back.
fn oid(name: &str) -> Oid {
    let hex: String = name.bytes().map(|b| format!("{b:02x}")).collect();
    Oid::parse(&format!("{hex:0>40}")).expect("a name of at most 20 bytes")
}

/// The name [`oid`] made `oid` from.
fn name(oid: &Oid) -> String {
    let hex = oid.as_str().trim_start_matches('0');
    (0..hex.len())
        .step_by(2)
        .map(|i| char::from(u8::from_str_radix(&hex[i..i + 2], 16).expect("hex")))
        .collect()
}

/// A log record: `name` and its parents, the rest blank.
fn commit(name: &str, parents: &[&str]) -> CommitRow {
    CommitRow {
        oid: oid(name),
        parents: parents.iter().map(|p| oid(p)).collect(),
        author_name: String::new(),
        author_email: String::new(),
        author_time: 0,
        commit_time: 0,
        subject: name.to_string(),
    }
}

/// The records of `"c:b b:a a:"`, in display order: each `name:parent,parent…`.
fn log(spec: &str) -> Vec<CommitRow> {
    spec.split_whitespace()
        .map(|word| {
            let (name, parents) = word.split_once(':').expect("name:parents");
            let parents: Vec<&str> = parents.split(',').filter(|p| !p.is_empty()).collect();
            commit(name, &parents)
        })
        .collect()
}

/// Log records as rows, with no HEAD among them and no synthetic row.
fn commits(spec: &str) -> Vec<Row> {
    log(spec)
        .into_iter()
        .map(|commit| Row::Commit {
            commit,
            checked_out: false,
        })
        .collect()
}

/// A stash `name` on `base`, with its index `name.i` and untracked files `name.u`.
fn stash(name: &str, base: &str) -> StashRow {
    StashRow {
        oid: oid(name),
        base: oid(base),
        index: Some(oid(&format!("{name}.i"))),
        untracked: Some(oid(&format!("{name}.u"))),
        selector: format!("stash@{{{name}}}"),
        commit_time: 0,
        subject: name.to_string(),
    }
}

/// Each row's pass-throughs among `rows` as `(slot, colour, dashed)`, sorted.
fn passing(graph: &Graph, rows: std::ops::Range<usize>) -> Vec<Vec<(u32, u32, bool)>> {
    let mut each = vec![Vec::new(); rows.len()];
    for PassThrough {
        slot,
        colour,
        rows: run,
        dashed,
    } in graph.pass_throughs(rows.clone())
    {
        for r in run {
            assert!(rows.contains(&r), "pass-through row {r} outside {rows:?}");
            each[r - rows.start].push((slot, colour, dashed));
        }
    }
    for row in &mut each {
        row.sort_unstable();
    }
    each
}

/// One line per row: its name (`◙` checked out, `≡` a stash, `○` the uncommitted row), `@` its
/// column, then the slots of its segments to the top edge (`^`) and to the bottom edge (`v`) and
/// of the lines passing it (`|`), each `~` when dashed.
fn drawing(rows: &[Row], graph: &Graph) -> Vec<String> {
    let passes = passing(graph, 0..graph.len());
    let list = |mut slots: Vec<(u32, bool)>| {
        slots.sort_unstable();
        let words: Vec<String> = slots
            .iter()
            .map(|(slot, dashed)| format!("{slot}{}", if *dashed { "~" } else { "" }))
            .collect();
        words.join(",")
    };
    (0..graph.len())
        .map(|i| {
            let label = match &rows[i] {
                Row::Uncommitted { .. } => "○".to_string(),
                Row::Stash(stash) => format!("≡{}", name(&stash.oid)),
                Row::Commit {
                    commit,
                    checked_out,
                } => format!(
                    "{}{}",
                    if *checked_out { "◙" } else { "" },
                    name(&commit.oid)
                ),
            };
            let mut text = format!("{label} @{}", graph.row(i).column);
            let ends = |end: End| {
                graph
                    .segments(i)
                    .iter()
                    .filter(|s| s.end == end)
                    .map(|s| (s.slot, s.dashed))
                    .collect::<Vec<_>>()
            };
            let through = passes[i].iter().map(|&(s, _, d)| (s, d)).collect();
            for (mark, slots) in [
                ("^", ends(End::Top)),
                ("v", ends(End::Bottom)),
                ("|", through),
            ] {
                if !slots.is_empty() {
                    text += &format!(" {mark}{}", list(slots));
                }
            }
            text
        })
        .collect()
}

/// Lays `rows` out and checks every row's drawing.
fn check(rows: &[Row], first_parent: bool, expected: &[&str]) -> Graph {
    let graph = layout(rows, first_parent);
    assert_eq!(drawing(rows, &graph), expected);
    graph
}

/// Each occupancy as `(slot, colour, first row, last row)`.
fn occupancies(graph: &Graph) -> Vec<(u32, u32, u32, Option<u32>)> {
    graph
        .intervals()
        .iter()
        .map(|i| (i.slot, i.colour, i.first, i.last))
        .collect()
}

fn node_colours(graph: &Graph) -> Vec<u32> {
    (0..graph.len()).map(|i| graph.row(i).colour).collect()
}

/// A line: one slot, one occupancy.
#[test]
fn a_line() {
    let graph = check(
        &commits("c:b b:a a:"),
        false,
        &["c @0 v0", "b @0 ^0 v0", "a @0 ^0"],
    );
    assert_eq!(occupancies(&graph), [(0, 0, 0, Some(2))]);
    assert_eq!(graph.width(), 1);
}

/// A branch and its merge: the other parent takes a new slot right of the column with the next
/// colour; the first parent keeps the column's slot and colour; the two lines to `b` run side by
/// side and join at `b`'s row (steps 3–5).
#[test]
fn a_branch_and_its_merge() {
    let graph = check(
        &commits("m:c,f f:b c:b b:a a:"),
        false,
        &[
            "m @0 v0,1",
            "f @1 ^1 v1 |0",
            "c @0 ^0 v0 |1",
            "b @0 ^0,1 v0",
            "a @0 ^0",
        ],
    );
    assert_eq!(
        occupancies(&graph),
        [(0, 0, 0, Some(4)), (1, 1, 0, Some(3))]
    );
    assert_eq!(node_colours(&graph), [0, 1, 0, 0, 0]);
    assert!(graph.segments(3).contains(&Segment {
        end: End::Top,
        slot: 1,
        colour: 1,
        dashed: false
    }));
    assert_eq!(graph.width(), 2);
}

/// An octopus merge: each other parent, in order, takes the next new slot.
#[test]
fn an_octopus_merge() {
    let graph = check(
        &commits("o:a,b,c c:r b:r a:r r:"),
        false,
        &[
            "o @0 v0,1,2",
            "c @2 ^2 v2 |0,1",
            "b @1 ^1 v1 |0,2",
            "a @0 ^0 v0 |1,2",
            "r @0 ^0,1,2",
        ],
    );
    assert_eq!(graph.width(), 3);
    assert_eq!(node_colours(&graph), [0, 2, 1, 0, 0]);
}

/// Two tips sharing a parent: the second tip finds no empty slot and takes a new one.
#[test]
fn two_tips_sharing_a_parent() {
    check(
        &commits("x:p y:p p:"),
        false,
        &["x @0 v0", "y @1 v1 |0", "p @0 ^0,1"],
    );
}

/// A criss-cross merge: `m1`'s other parent `b` is already awaited by slot 0, so its line goes
/// to that slot; `a` is awaited twice and joins.
#[test]
fn a_criss_cross_merge() {
    check(
        &commits("m2:b,a m1:a,b b:o a:o o:"),
        false,
        &[
            "m2 @0 v0,1",
            "m1 @2 v0,2 |0,1",
            "b @0 ^0 v0 |1,2",
            "a @1 ^1,2 v1 |0",
            "o @0 ^0,1",
        ],
    );
}

/// A parent not loaded (the next page's, or beyond a shallow boundary): its slot runs to the
/// bottom of the loaded rows (step 7), its occupancy open.
#[test]
fn a_parent_not_loaded() {
    let graph = check(
        &commits("m:c,z c:b b:y"),
        false,
        &["m @0 v0,1", "c @0 ^0 v0 |1", "b @0 ^0 v0 |1"],
    );
    assert_eq!(occupancies(&graph), [(0, 0, 0, None), (1, 1, 0, None)]);
}

/// `--first-parent`: git still prints every parent, and only the first is drawn. Without the
/// flag the second parent, never listed, would run to the bottom.
#[test]
fn first_parent_draws_the_first_parent_only() {
    let rows = commits("m:c,f c:b b:");
    check(&rows, true, &["m @0 v0", "c @0 ^0 v0", "b @0 ^0"]);
    check(&rows, false, &["m @0 v0,1", "c @0 ^0 v0 |1", "b @0 ^0 |1"]);
}

/// The uncommitted row first, its one parent HEAD's commit far down: a dashed line runs down its
/// slot to HEAD's row while another branch's rows pass beside it; below HEAD the line is solid.
/// In an empty repository the uncommitted row has no parent.
#[test]
fn the_uncommitted_row_with_head_far_down() {
    let mut stream = RowStream::new(Some(oid("h")), true, Vec::new(), true);
    let rows = stream.page(log("t:s s:b h:b b:"));
    let graph = check(
        &rows,
        false,
        &[
            "○ @0 v0~",
            "t @1 v1 |0~",
            "s @1 ^1 v1 |0~",
            "◙h @0 ^0~ v0 |1",
            "b @0 ^0,1",
        ],
    );
    assert_eq!(graph.row(0).node, Node::Uncommitted);
    assert_eq!(graph.row(3).node, Node::CheckedOut);
    assert_eq!(node_colours(&graph), [0, 1, 1, 0, 0]);
    // A window below the uncommitted row still knows the line is dashed.
    assert_eq!(passing(&graph, 2..3), [vec![(0, 0, true)]]);

    let mut empty = RowStream::new(None, true, Vec::new(), true);
    let rows = empty.page(Vec::new());
    assert_eq!(rows, [Row::Uncommitted { head: None }]);
    check(&rows, false, &["○ @0"]);
}

/// A root commit in the middle: its slot is empty after its node, so the next tip takes it;
/// `r` and `q` are two roots.
#[test]
fn a_root_in_the_middle_and_two_roots() {
    let graph = check(
        &commits("a:r b:q r: c:q q:"),
        false,
        &[
            "a @0 v0",
            "b @1 v1 |0",
            "r @0 ^0 |1",
            "c @0 v0 |1",
            "q @0 ^0,1",
        ],
    );
    assert_eq!(
        occupancies(&graph),
        [(0, 0, 0, Some(2)), (1, 1, 1, Some(4)), (0, 2, 3, Some(4))]
    );
}

/// Another parent not yet awaited takes the lowest empty slot right of the column (step 5),
/// not an empty slot left of it: `q`'s `z` goes to a new slot 2 while slot 0 is empty.
#[test]
fn another_parent_takes_a_slot_right_of_the_column() {
    check(
        &commits("a:r b:q r: q:x,z z:x x:"),
        false,
        &[
            "a @0 v0",
            "b @1 v1 |0",
            "r @0 ^0 |1",
            "q @1 ^1 v1,2",
            "z @2 ^2 v2 |1",
            "x @1 ^1,2",
        ],
    );
}

/// A join frees slot 1 and the same row's other parent takes it (step 5: the lowest empty slot
/// right of the column): a line ends at the slot's top edge and a new occupancy leaves its
/// bottom edge — two node segments, no pass-through.
#[test]
fn a_slot_freed_by_a_join_is_taken_in_the_same_row() {
    let graph = check(
        &commits("x:p y:p p:q,z z:q q:"),
        false,
        &[
            "x @0 v0",
            "y @1 v1 |0",
            "p @0 ^0,1 v0,1",
            "z @1 ^1 v1 |0",
            "q @0 ^0,1",
        ],
    );
    assert_eq!(
        occupancies(&graph),
        [(0, 0, 0, Some(4)), (1, 1, 1, Some(2)), (1, 2, 2, Some(4))]
    );
    let colours: BTreeSet<(End, u32, u32)> = graph
        .segments(2)
        .iter()
        .map(|s| (s.end, s.slot, s.colour))
        .collect();
    assert_eq!(
        colours,
        BTreeSet::from([
            (End::Top, 0, 0),
            (End::Top, 1, 1),
            (End::Bottom, 0, 0),
            (End::Bottom, 1, 2)
        ])
    );
}

/// A stash on a loaded base: its row enters immediately above the base's row.
#[test]
fn a_stash_on_a_loaded_base() {
    let mut stream = RowStream::new(Some(oid("c")), false, vec![stash("s", "b")], true);
    let rows = stream.page(log("c:b b:a a:"));
    let graph = check(
        &rows,
        false,
        &["◙c @0 v0", "≡s @1 v1 |0", "b @0 ^0,1 v0", "a @0 ^0"],
    );
    assert_eq!(graph.row(1).node, Node::Stash);
}

/// Two stashes on one base: newest first, both above the base.
#[test]
fn two_stashes_on_one_base() {
    let stashes = vec![stash("s1", "b"), stash("s0", "b")];
    let mut stream = RowStream::new(Some(oid("c")), false, stashes, true);
    let rows = stream.page(log("c:b b:a a:"));
    check(
        &rows,
        false,
        &[
            "◙c @0 v0",
            "≡s1 @1 v1 |0",
            "≡s0 @2 v2 |0,1",
            "b @0 ^0,1,2 v0",
            "a @0 ^0",
        ],
    );
}

/// A stash whose base arrives on the next page waits for it; the rows above do not move and the
/// layout extended page by page is the layout of all the rows.
#[test]
fn a_stash_whose_base_arrives_on_the_next_page() {
    let mut stream = RowStream::new(Some(oid("c")), false, vec![stash("s", "a")], true);
    let first = stream.page(log("c:b b:a"));
    let mut graph = check(&first, false, &["◙c @0 v0", "b @0 ^0 v0"]);
    let second = stream.page(log("a:"));
    graph.extend(&second);
    let rows: Vec<Row> = first.iter().chain(&second).cloned().collect();
    assert_eq!(
        drawing(&rows, &graph),
        ["◙c @0 v0", "b @0 ^0 v0", "≡s @1 v1 |0", "a @0 ^0,1"]
    );
    assert_eq!(graph, layout(&rows, false));
}

/// With "Reflog commits" the stash's own commit, its index and its untracked commit come in
/// through the log too: they are dropped, and the stash is drawn once, as its stash row. With
/// the Stashes filter off there is no stash row and they are still dropped.
#[test]
fn a_stash_whose_commits_come_in_through_the_reflog_is_drawn_once() {
    let records = "c:b s:b,s.i,s.u s.i:b s.u: b:a a:";
    let mut stream = RowStream::new(Some(oid("c")), false, vec![stash("s", "b")], true);
    let rows = stream.page(log(records));
    check(
        &rows,
        false,
        &["◙c @0 v0", "≡s @1 v1 |0", "b @0 ^0,1 v0", "a @0 ^0"],
    );

    let mut hidden = RowStream::new(Some(oid("c")), false, vec![stash("s", "b")], false);
    let rows = hidden.page(log(records));
    check(&rows, false, &["◙c @0 v0", "b @0 ^0 v0", "a @0 ^0"]);
}

/// 03 §4.3's measured shapes, scaled down. Sixty branches committed in turn and merged back:
/// 61 slots, one occupancy per branch and one for main, and node segments about 2 per row —
/// what the layout stores grows with rows and edges, not rows × lanes. A two-commit side
/// branch merged every 50 commits: at most 2 slots.
#[test]
fn the_measured_shapes_store_node_segments_only() {
    let (branches, rounds) = (60, 100);
    let mut records = Vec::new();
    for i in (1..=branches).rev() {
        let main = if i == 1 {
            "base".to_string()
        } else {
            format!("m{}", i - 1)
        };
        records.push(format!("m{i}:{main},b{i}.{rounds}"));
    }
    for k in (1..=rounds).rev() {
        for i in (1..=branches).rev() {
            let parent = if k == 1 {
                "base".to_string()
            } else {
                format!("b{i}.{}", k - 1)
            };
            records.push(format!("b{i}.{k}:{parent}"));
        }
    }
    records.push("base:".to_string());
    let graph = layout(&commits(&records.join(" ")), false);
    assert_eq!(graph.len(), 6061);
    assert_eq!(graph.width(), 61);
    assert_eq!(graph.intervals().len(), 61);
    // The merges: 2 + 59 × 3; each branch commit: 2; the base: 61 joins.
    assert_eq!(graph.segment_count(), 2 + 59 * 3 + 6000 * 2 + 61);
    // What a row stores does not grow with the width (03 §4.5): its node and its segments.
    assert_eq!(std::mem::size_of::<RowGraph>(), 16);
    assert_eq!(std::mem::size_of::<Segment>(), 12);

    let mut records = Vec::new();
    let merges = 40;
    for j in (1..=merges).rev() {
        let below = format!("p{j}");
        records.push(format!("m{j}:{below},s{j}.2"));
        records.push(format!("s{j}.2:s{j}.1"));
        records.push(format!("s{j}.1:{below}"));
        for k in (1..=48).rev() {
            let parent = if k == 1 && j == 1 {
                String::new()
            } else if k == 1 {
                format!("m{}", j - 1)
            } else {
                format!("p{j}.{}", k - 1)
            };
            let me = if k == 48 {
                below.clone()
            } else {
                format!("p{j}.{k}")
            };
            records.push(format!("{me}:{parent}"));
        }
    }
    let graph = layout(&commits(&records.join(" ")), false);
    assert_eq!(graph.len(), merges * 51);
    assert_eq!(graph.width(), 2);
    assert_eq!(graph.intervals().len(), 1 + merges);
    assert!(graph.segment_count() <= graph.len() * 21 / 10);
}

/// Out-of-order rows, a commit twice, a parent listed twice and a commit its own parent never
/// panic, and every line still continues across each row edge.
#[test]
fn input_git_never_prints_lays_out_without_panicking() {
    let rows = commits("a:b b: c:a,a d:d a:x e:e,f");
    let graph = layout(&rows, false);
    assert_eq!(graph.len(), rows.len());
    edges_meet(&graph, "odd input");
}

// ---------------------------------------------------------------------------------------------
// Properties over 1 000 generated graphs (§4.3).

/// The tests' generator: Knuth's MMIX LCG, as `gui::text`'s tests use; a graph's seed makes it
/// again.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize % n.max(1)
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// One generated load: what [`RowStream::new`] takes, the log's pages and the filter.
struct Load {
    head: Option<Oid>,
    uncommitted: bool,
    stashes: Vec<StashRow>,
    show_stashes: bool,
    pages: Vec<Vec<CommitRow>>,
    first_parent: bool,
}

/// A random history in a valid display order (each commit above its parents): branches grow
/// and start anywhere, merges and octopus merges, roots in the middle, parents never loaded;
/// HEAD, uncommitted changes, stashes (a base not loaded among them) whose commits come in
/// through the reflog half the time; the log cut into pages; first parent only a quarter of
/// the time.
fn generate(seed: u64) -> Load {
    let mut rng = Lcg(seed);
    let n = 1 + rng.below(40);
    let mut made: Vec<CommitRow> = Vec::new();
    let mut tips: Vec<usize> = Vec::new();
    for k in 0..n {
        let mut parents: Vec<String> = Vec::new();
        if k > 0 && !rng.chance(10) {
            let first = if !tips.is_empty() && rng.chance(70) {
                tips[rng.below(tips.len())]
            } else {
                rng.below(k)
            };
            parents.push(format!("c{first}"));
            if rng.chance(25) {
                for _ in 0..1 + rng.below(3) {
                    parents.push(format!("c{}", rng.below(k)));
                }
            }
        }
        if rng.chance(6) {
            parents.push(format!("x{k}"));
        }
        tips.retain(|t| !parents.contains(&format!("c{t}")));
        tips.push(k);
        let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
        made.push(commit(&format!("c{k}"), &parents));
    }
    let mut records: Vec<CommitRow> = made.into_iter().rev().collect();

    let head = match rng.below(4) {
        0 => None,
        1 => Some(oid("x-head")),
        _ => Some(records[rng.below(records.len())].oid.clone()),
    };
    let uncommitted = rng.chance(50);
    let mut stashes = Vec::new();
    for j in 0..rng.below(4) {
        let base = if rng.chance(85) {
            records[rng.below(records.len())].oid.clone()
        } else {
            oid(&format!("x-base{j}"))
        };
        let untracked = rng.chance(30).then(|| oid(&format!("u{j}")));
        stashes.push(StashRow {
            oid: oid(&format!("w{j}")),
            base,
            index: Some(oid(&format!("i{j}"))),
            untracked,
            selector: format!("stash@{{{j}}}"),
            commit_time: 0,
            subject: String::new(),
        });
    }
    if rng.chance(50) {
        for stash in &stashes {
            let below = records
                .iter()
                .position(|c| c.oid == stash.base)
                .unwrap_or(records.len());
            let at = rng.below(below + 1);
            let index = stash.index.clone().expect("an index");
            let mut parents = vec![stash.base.clone(), index.clone()];
            parents.extend(stash.untracked.clone());
            let named: Vec<String> = parents.iter().map(name).collect();
            let named: Vec<&str> = named.iter().map(String::as_str).collect();
            records.insert(at, commit(&name(&stash.oid), &named));
            records.insert(at + 1, commit(&name(&index), &[&name(&stash.base)]));
            if let Some(untracked) = &stash.untracked {
                let at = at + 1 + rng.below(records.len() - at);
                records.insert(at, commit(&name(untracked), &[]));
            }
        }
    }
    let mut cuts: Vec<usize> = (0..rng.below(4))
        .map(|_| rng.below(records.len() + 1))
        .collect();
    cuts.sort_unstable();
    let mut pages = Vec::new();
    let mut from = 0;
    for cut in cuts.into_iter().chain([records.len()]) {
        pages.push(records[from..cut].to_vec());
        from = cut;
    }
    Load {
        head,
        uncommitted,
        stashes,
        show_stashes: rng.chance(80),
        pages,
        first_parent: rng.chance(25),
    }
}

/// The lines at one row edge, as `(slot, colour)`.
type Edge = BTreeSet<(u32, u32)>;

/// Each row's lines at its top edge and at its bottom edge.
fn edges(graph: &Graph) -> (Vec<Edge>, Vec<Edge>) {
    let passes = passing(graph, 0..graph.len());
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for (i, through) in passes.iter().enumerate() {
        let through: Edge = through.iter().map(|&(s, c, _)| (s, c)).collect();
        let at = |end: End| -> Edge {
            let segments = graph.segments(i).iter().filter(|s| s.end == end);
            segments
                .map(|s| (s.slot, s.colour))
                .chain(through.clone())
                .collect()
        };
        top.push(at(End::Top));
        bottom.push(at(End::Bottom));
    }
    (top, bottom)
}

/// Every segment and pass-through joins two adjacent rows at the same slot, in the same colour,
/// on both sides; nothing enters the first row from above; no slot is beyond the width.
fn edges_meet(graph: &Graph, context: &str) {
    let (top, bottom) = edges(graph);
    if let Some(first) = top.first() {
        assert!(first.is_empty(), "{context}: lines above the first row");
    }
    for r in 1..graph.len() {
        assert_eq!(bottom[r - 1], top[r], "{context}: the edge above row {r}");
    }
    for (r, slots) in top.iter().chain(&bottom).enumerate() {
        let r = r % graph.len().max(1);
        assert!(graph.row(r).column < graph.width(), "{context}: row {r}");
        assert!(slots.iter().all(|&(s, _)| s < graph.width()), "{context}");
    }
}

/// A commit's node is at the slot its children's lines end in: following each line out of a
/// node — down its slot through pass-throughs to the first line into a node — reaches exactly
/// the rows of the parents drawn (a parent not loaded runs off the bottom), and every line
/// into a node is reached so. A node sits on the lowest line into it, or, with none, on the
/// lowest slot free at its top edge (step 2); its own lines carry its colour.
fn lines_reach_parents(rows: &[Row], graph: &Graph, first_parent: bool, context: &str) {
    let (top, _) = edges(graph);
    let passes = passing(graph, 0..graph.len());
    let at: HashMap<&Oid, usize> = rows
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(i, row)| row.oid().map(|o| (o, i)))
        .collect();
    let mut reached = BTreeSet::new();
    let mut into = BTreeSet::new();
    for c in 0..graph.len() {
        let node = graph.row(c);
        let mut tops: Vec<u32> = Vec::new();
        for s in graph.segments(c) {
            match s.end {
                End::Top => {
                    into.insert((c, s.slot));
                    tops.push(s.slot);
                }
                End::Bottom => {}
            }
            if s.slot == node.column {
                assert_eq!(s.colour, node.colour, "{context}: row {c}'s own line");
            }
        }
        match tops.iter().min() {
            Some(&lowest) => assert_eq!(node.column, lowest, "{context}: row {c}"),
            None => {
                let free = (0..).find(|s| !top[c].iter().any(|&(t, _)| t == *s));
                assert_eq!(Some(node.column), free, "{context}: tip row {c}");
            }
        }
        // Step 5: the other parents that begin an occupancy here take, in parent order (their
        // colours), the lowest slots right of the column that no line crosses.
        let mut began: Vec<(u32, u32)> = graph
            .segments(c)
            .iter()
            .filter(|s| s.end == End::Bottom && s.slot != node.column)
            .filter(|s| {
                let occupancy = graph.intervals().iter().find(|o| o.colour == s.colour);
                occupancy.is_some_and(|o| o.first as usize == c)
            })
            .map(|s| (s.colour, s.slot))
            .collect();
        began.sort_unstable();
        let crossed: Vec<u32> = passes[c].iter().map(|&(s, _, _)| s).collect();
        let free: Vec<u32> = (node.column + 1..)
            .filter(|s| !crossed.contains(s))
            .take(began.len())
            .collect();
        let taken: Vec<u32> = began.iter().map(|&(_, slot)| slot).collect();
        assert_eq!(taken, free, "{context}: row {c}'s new slots");

        let mut considered: Vec<&Oid> = Vec::new();
        for p in rows[c].parents() {
            if !considered.contains(&p) {
                considered.push(p);
            }
        }
        if first_parent {
            considered.truncate(1);
        }
        let mut expected: Vec<Option<usize>> = considered
            .iter()
            .map(|p| at.get(p).copied().filter(|&j| j > c))
            .collect();
        let mut found: Vec<Option<usize>> = Vec::new();
        for s in graph.segments(c).iter().filter(|s| s.end == End::Bottom) {
            let mut r = c + 1;
            let end = loop {
                if r == graph.len() {
                    break None;
                }
                let ends_here = graph
                    .segments(r)
                    .iter()
                    .any(|t| t.end == End::Top && t.slot == s.slot);
                if ends_here {
                    reached.insert((r, s.slot));
                    break Some(r);
                }
                let passes_here = passes[r].iter().any(|&(t, _, _)| t == s.slot);
                assert!(passes_here, "{context}: row {c}'s line breaks at row {r}");
                r += 1;
            };
            found.push(end);
        }
        expected.sort_unstable();
        found.sort_unstable();
        assert_eq!(found, expected, "{context}: row {c}'s lines");
    }
    assert_eq!(reached, into, "{context}: lines into nodes from no child");
}

/// The dashed pieces are exactly the uncommitted row's line to HEAD's row.
fn dashes_are_the_uncommitted_line(rows: &[Row], graph: &Graph, context: &str) {
    let passes = passing(graph, 0..graph.len());
    let mut dashed = BTreeSet::new();
    for (r, through) in passes.iter().enumerate() {
        for &(slot, _, d) in through {
            if d {
                dashed.insert((r, slot, "|"));
            }
        }
        for s in graph.segments(r).iter().filter(|s| s.dashed) {
            dashed.insert((r, s.slot, if s.end == End::Top { "^" } else { "v" }));
        }
    }
    let mut line = BTreeSet::new();
    if let (Some(Row::Uncommitted { head: Some(_) }), Some(down)) =
        (rows.first(), graph.segments(0).first())
    {
        line.insert((0, down.slot, "v"));
        for r in 1..graph.len() {
            let ends = graph
                .segments(r)
                .iter()
                .any(|s| s.end == End::Top && s.slot == down.slot);
            if ends {
                line.insert((r, down.slot, "^"));
                break;
            }
            line.insert((r, down.slot, "|"));
        }
    }
    assert_eq!(dashed, line, "{context}: the dashed pieces");
}

/// The stream: the uncommitted row first; every record but a stash's internals, once, in
/// order; each shown stash whose base is loaded once, immediately above its base's row (after
/// its newer stashes on that base).
fn stream_rows_are_in_place(load: &Load, rows: &[Row], context: &str) {
    let internal: HashSet<Oid> = load
        .stashes
        .iter()
        .flat_map(|s| [Some(s.oid.clone()), s.index.clone(), s.untracked.clone()])
        .flatten()
        .collect();
    let mut rest = rows;
    if load.uncommitted {
        let head = load.head.clone();
        assert_eq!(rows.first(), Some(&Row::Uncommitted { head }), "{context}");
        rest = &rows[1..];
    }
    let expected: Vec<&Oid> = load
        .pages
        .iter()
        .flatten()
        .map(|c| &c.oid)
        .filter(|o| !internal.contains(o))
        .collect();
    let drawn: Vec<&Oid> = rest
        .iter()
        .filter_map(|row| match row {
            Row::Commit {
                commit,
                checked_out,
            } => {
                assert_eq!(*checked_out, load.head.as_ref() == Some(&commit.oid));
                Some(&commit.oid)
            }
            Row::Stash(_) => None,
            Row::Uncommitted { .. } => panic!("{context}: a second uncommitted row"),
        })
        .collect();
    assert_eq!(drawn, expected, "{context}: the records");

    let mut stash_rows: Vec<&StashRow> = Vec::new();
    for (i, row) in rest.iter().enumerate() {
        if let Row::Stash(stash) = row {
            stash_rows.push(stash);
            let below = rest[i + 1..]
                .iter()
                .find(|r| !matches!(r, Row::Stash(s) if s.base == stash.base));
            assert_eq!(
                below.and_then(Row::oid),
                Some(&stash.base),
                "{context}: stash above its base"
            );
        }
    }
    let shown: Vec<&StashRow> = if load.show_stashes {
        let mut by_row: Vec<(usize, usize, &StashRow)> = Vec::new();
        for (n, stash) in load.stashes.iter().enumerate() {
            if let Some(i) = expected.iter().position(|o| **o == stash.base) {
                by_row.push((i, n, stash));
            }
        }
        by_row.sort_unstable_by_key(|&(i, n, _)| (i, n));
        by_row.into_iter().map(|(_, _, s)| s).collect()
    } else {
        Vec::new()
    };
    assert_eq!(stash_rows, shown, "{context}: the stash rows");
}

/// G-G2: the properties of §4.3 over 1 000 graphs from a fixed-seed generator — lines meet at
/// the same slot across every row edge; a commit's node is where its children's lines end; the
/// layout of the first k rows is a prefix of the layout of all rows, and laying out page by
/// page gives the one-shot layout; the result depends on the input only (run twice). A
/// failure names the graph and its seed.
#[test]
fn the_layout_properties_hold_over_1000_generated_graphs() {
    for g in 0..1000u64 {
        let seed = 0x5eed_6a7e ^ g.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let load = generate(seed);
        let fp = load.first_parent;
        let context = format!("graph {g} (seed {seed:#x}, first parent {fp})");

        let start = || {
            RowStream::new(
                load.head.clone(),
                load.uncommitted,
                load.stashes.clone(),
                load.show_stashes,
            )
        };
        let mut stream = start();
        let mut rows: Vec<Row> = Vec::new();
        let mut paged = Graph::new(fp);
        for page in &load.pages {
            let added = stream.page(page.clone());
            paged.extend(&added);
            rows.extend(added);
        }
        stream_rows_are_in_place(&load, &rows, &context);

        let whole = layout(&rows, fp);
        let shown = || drawing(&rows, &whole).join("\n");
        assert_eq!(paged, whole, "{context}: page by page\n{}", shown());
        edges_meet(&whole, &context);
        lines_reach_parents(&rows, &whole, fp, &context);
        dashes_are_the_uncommitted_line(&rows, &whole, &context);

        // Run twice: the stream and the layout depend on the input only.
        let mut again = start();
        let rows_again: Vec<Row> = load
            .pages
            .iter()
            .flat_map(|p| again.page(p.clone()))
            .collect();
        assert_eq!(rows_again, rows, "{context}: the stream twice");
        assert_eq!(layout(&rows, fp), whole, "{context}: the layout twice");

        // Paging never redraws rows above: the first k rows' layout is a prefix of the whole.
        let n = rows.len();
        let mut rng = Lcg(seed);
        for k in [0, 1.min(n), n / 2, rng.below(n + 1), n.saturating_sub(1), n] {
            let part = layout(&rows[..k], fp);
            assert_eq!(part.len(), k, "{context}");
            for i in 0..k {
                assert_eq!(part.row(i), whole.row(i), "{context}: row {i} of {k}");
                assert_eq!(
                    part.segments(i),
                    whole.segments(i),
                    "{context}: row {i} of {k}"
                );
            }
            assert_eq!(
                passing(&part, 0..k),
                passing(&whole, 0..k),
                "{context}: k {k}"
            );
        }

        // A window of rows draws what the whole range draws there.
        let a = rng.below(n + 1);
        let b = a + rng.below(n - a + 1);
        assert_eq!(
            passing(&whole, a..b),
            passing(&whole, 0..n)[a..b],
            "{context}: {a}..{b}"
        );
    }
}
