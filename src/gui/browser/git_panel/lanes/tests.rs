//! X4-lanes's tests: each of 03 §4.3's graphs written out as rows of glyphs — and of colours,
//! where lines meet — the cut, an ambiguous-wide terminal's stand-ins, and properties over 1 000
//! graphs from a fixed-seed generator: every glyph from code page 437 (none of ambiguous width in
//! such a terminal), lines that meet across cells and rows, the visible rows' cells those of the
//! whole, paging never redrawing the rows above.

use super::super::graph_view::Columns;
use super::{rows, stand_in, Cell, WORK};
use crate::git::graph::{layout, End, Graph, Row, RowStream};
use crate::git::model::{CommitRow, Oid, StashRow};
use crate::gui::chrome::git::{self as look, LOOK};
use crate::gui::chrome::{self, cga, PALETTE};
use crate::gui::term;

/// A test id: the name's bytes in hex, zero-padded to 40 digits.
fn oid(name: &str) -> Oid {
    let hex: String = name.bytes().map(|b| format!("{b:02x}")).collect();
    Oid::parse(&format!("{hex:0>40}")).expect("a name of at most 20 bytes")
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

/// Columns that show every lane of `graph`: no cut.
fn every_lane(graph: &Graph) -> Columns {
    let lanes = graph.width().max(1);
    let cols = Columns::new(6 * (lanes as usize + 1), lanes);
    assert!(!cols.cut);
    cols
}

/// A row's cells as text, a blank cell a space.
fn text(row: &[Option<Cell>]) -> String {
    row.iter().map(|c| c.map_or(' ', |c| c.glyph)).collect()
}

/// A row's cells as the letters of their colours: the lane colours `c y g m w` (light cyan,
/// yellow, light green, light magenta, white), `d` the dim colour (the uncommitted row's and the
/// cut's), a blank cell a space.
fn letters(row: &[Option<Cell>]) -> String {
    let letter = |cell: &Option<Cell>| {
        let Some(cell) = cell else { return ' ' };
        match cell.colour {
            c if c == cga::LIGHT_CYAN => 'c',
            c if c == cga::YELLOW => 'y',
            c if c == cga::LIGHT_GREEN => 'g',
            c if c == cga::LIGHT_MAGENTA => 'm',
            c if c == cga::WHITE => 'w',
            c if c == PALETTE.dim => 'd',
            c => panic!("not a lane colour: {c:?}"),
        }
    };
    row.iter().map(letter).collect()
}

/// The Graph column of every row of `graph` in `cols`, as text.
fn drawn(graph: &Graph, cols: &Columns, wide: bool) -> Vec<String> {
    let cells = rows(graph, 0..graph.len(), cols, wide);
    cells.iter().map(|row| text(row)).collect()
}

/// The colours of every row of `graph` in `cols`, as letters.
fn coloured(graph: &Graph, cols: &Columns, wide: bool) -> Vec<String> {
    let cells = rows(graph, 0..graph.len(), cols, wide);
    cells.iter().map(|row| letters(row)).collect()
}

/// Lays `list` out and checks the Graph column of every row, every lane shown: its glyphs, and
/// its colours when given.
fn check(list: &[Row], first_parent: bool, glyphs: &[&str], colours: Option<&[&str]>) -> Graph {
    let graph = layout(list, first_parent);
    let cols = every_lane(&graph);
    assert_eq!(drawn(&graph, &cols, false), glyphs);
    if let Some(colours) = colours {
        assert_eq!(coloured(&graph, &cols, false), colours);
    }
    graph
}

/// A line: one lane, its nodes in its colour.
#[test]
fn a_line() {
    check(
        &commits("c:b b:a a:"),
        false,
        &["■ ", "■ ", "■ "],
        Some(&["c ", "c ", "c "]),
    );
}

/// A branch and its merge: the merge's line to the branch runs along its row, `─` then `┐` down
/// to the new lane; the branch's line comes back `┘` into its parent; each line in its lane's
/// colour, the nodes in theirs.
#[test]
fn a_branch_and_its_merge() {
    check(
        &commits("m:c,f f:b c:b b:a a:"),
        false,
        &["■─┐ ", "│ ■ ", "■ │ ", "■─┘ ", "■   "],
        Some(&["cyy ", "c y ", "c y ", "cyy ", "c   "]),
    );
}

/// An octopus merge: the lines to its parents share the node's row, so where the first turns
/// down the other goes on, `┬`; they come back `┴`. A shared stretch is the nearer lane's colour.
#[test]
fn an_octopus_merge() {
    check(
        &commits("o:a,b,c c:r b:r a:r r:"),
        false,
        &["■─┬─┐ ", "│ │ ■ ", "│ ■ │ ", "■ │ │ ", "■─┴─┘ "],
        Some(&["cyygg ", "c y g ", "c y g ", "c y g ", "cyygg "]),
    );
}

/// Two tips sharing a parent: the second takes a new lane, which joins the parent.
#[test]
fn two_tips_sharing_a_parent() {
    check(
        &commits("x:p y:p p:"),
        false,
        &["■   ", "│ ■ ", "■─┘ "],
        Some(&["c   ", "c y ", "cyy "]),
    );
}

/// A criss-cross merge: `m1`'s line to `b`'s lane, left of it, crosses the lane between (`┼`, in
/// the crossing line's colour) and joins the lane passing by (`├`).
#[test]
fn a_criss_cross_merge() {
    check(
        &commits("m2:b,a m1:a,b b:o a:o o:"),
        false,
        &["■─┐   ", "├─┼─■ ", "■ │ │ ", "│ ■─┘ ", "■─┘   "],
        Some(&["cyy   ", "ccccg ", "c y g ", "c ygg ", "cyy   "]),
    );
}

/// A merge whose two other parents are both awaited by lanes left of it: the cell where the
/// nearer line joins its lane is `┼` as a crossing is, and its colour tells them apart — the
/// joining line's own (`m`'s row), where a crossing takes the crossing line's (`a`'s row, and
/// `m1`'s in the criss-cross above).
#[test]
fn a_merge_joining_two_lanes_to_its_left() {
    check(
        &commits("x:a y:b m:z,a,b z:a a:b b:"),
        false,
        &["■     ", "│ ■   ", "├─┼─■ ", "│ │ ■ ", "■─┼─┘ ", "■─┘   "],
        Some(&["c     ", "c y   ", "ccyyg ", "c y g ", "cgggg ", "cyy   "]),
    );
}

/// A merge whose other parent a lane passing by on its right already awaits: its line goes down
/// into that lane, `┤`, in that lane's colour.
#[test]
fn a_merge_goes_down_into_a_lane_passing_on_its_right() {
    check(
        &commits("a:b y:p b:c,p c:p p:"),
        false,
        &["■   ", "│ ■ ", "■─┤ ", "■ │ ", "■─┘ "],
        Some(&["c   ", "c y ", "cyy ", "c y ", "cyy "]),
    );
}

/// A join a longer lane change goes on past is `┼`, as a crossing is; only its colour, the
/// joined lane's, tells them apart (03 §4.3), so where the two lanes share one — five apart, the
/// lane colours' cycle — the rows read the same. `m`'s line to `a` crosses lanes 1–5; with `f` as
/// a parent too it also joins lane 5, whose colour is lane 0's, and every row is drawn as before.
/// With `e` instead (lane 4, white) the stretch from the join to the node is white.
#[test]
fn a_join_a_line_goes_on_past_is_told_from_a_crossing_by_colour_alone() {
    let lines = "x0:a x1:b x2:c x3:d x4:e x5:f m:z,a z:a a: b: c: d: e: f:";
    let crossing = check(
        &commits(lines),
        false,
        &[
            "■             ",
            "│ ■           ",
            "│ │ ■         ",
            "│ │ │ ■       ",
            "│ │ │ │ ■     ",
            "│ │ │ │ │ ■   ",
            "├─┼─┼─┼─┼─┼─■ ",
            "│ │ │ │ │ │ ■ ",
            "■─┼─┼─┼─┼─┼─┘ ",
            "  ■ │ │ │ │   ",
            "    ■ │ │ │   ",
            "      ■ │ │   ",
            "        ■ │   ",
            "          ■   ",
        ],
        Some(&[
            "c             ",
            "c y           ",
            "c y g         ",
            "c y g m       ",
            "c y g m w     ",
            "c y g m w c   ",
            "ccccccccccccy ",
            "c y g m w c y ",
            "cyyyyyyyyyyyy ",
            "  y g m w c   ",
            "    g m w c   ",
            "      m w c   ",
            "        w c   ",
            "          c   ",
        ]),
    );
    let cols = every_lane(&crossing);
    let joining = layout(&commits(&lines.replace("m:z,a ", "m:z,a,f ")), false);
    assert_eq!(joining.segments(6).len(), crossing.segments(6).len() + 1);
    assert_eq!(
        drawn(&joining, &cols, false),
        drawn(&crossing, &cols, false)
    );
    assert_eq!(
        coloured(&joining, &cols, false),
        coloured(&crossing, &cols, false)
    );
    let white = layout(&commits(&lines.replace("m:z,a ", "m:z,a,e ")), false);
    assert_eq!(drawn(&white, &cols, false)[6], "├─┼─┼─┼─┼─┼─■ ");
    assert_eq!(coloured(&white, &cols, false)[6], "ccccccccwwwwy ");
}

/// A parent not loaded: its lane runs to the bottom of the loaded rows.
#[test]
fn a_parent_not_loaded() {
    check(
        &commits("m:c,z c:b b:y"),
        false,
        &["■─┐ ", "■ │ ", "■ │ "],
        Some(&["cyy ", "c y ", "c y "]),
    );
}

/// `--first-parent`: one lane; without it the second parent's lane runs to the bottom.
#[test]
fn first_parent_draws_one_lane() {
    let list = commits("m:c,f c:b b:");
    check(&list, true, &["■ ", "■ ", "■ "], None);
    check(&list, false, &["■─┐ ", "■ │ ", "■ │ "], None);
}

/// The uncommitted row with HEAD far down: `○` and its line to HEAD's row dim and dotted, `:` a
/// row, beside another branch's rows; below HEAD the lane is solid. In an empty repository the
/// uncommitted row stands alone.
#[test]
fn the_uncommitted_row_with_head_far_down() {
    let mut stream = RowStream::new(Some(oid("h")), true, Vec::new(), true);
    check(
        &stream.page(log("t:s s:b h:b b:")),
        false,
        &["○   ", ": ■ ", ": ■ ", "◙ │ ", "■─┘ "],
        Some(&["d   ", "d y ", "d y ", "c y ", "cyy "]),
    );

    let mut empty = RowStream::new(None, true, Vec::new(), true);
    check(&empty.page(Vec::new()), false, &["○ "], Some(&["d "]));
}

/// A merge onto HEAD's commit, under the uncommitted row: its solid line joins the dotted one,
/// `├` in the joining line's colour; above and below it the dotted line goes on.
#[test]
fn a_merge_joins_the_dotted_line_to_head() {
    let mut stream = RowStream::new(Some(oid("h")), true, Vec::new(), true);
    check(
        &stream.page(log("m:x,h x:h h:b b:")),
        false,
        &["○   ", "├─■ ", ": ■ ", "◙─┘ ", "■   "],
        Some(&["d   ", "ccy ", "d y ", "cyy ", "c   "]),
    );
}

/// A root in the middle: no line leaves it downward, its lane is free on the next row (a new tip
/// takes it, in the next colour) or blank; `r` and `q` are two roots.
#[test]
fn a_root_in_the_middle_and_two_roots() {
    check(
        &commits("a:r b:q r: c:q q:"),
        false,
        &["■   ", "│ ■ ", "■ │ ", "■ │ ", "■─┘ "],
        Some(&["c   ", "c y ", "c y ", "g y ", "gyy "]),
    );
    check(
        &commits("a:r b:q r: q:"),
        false,
        &["■   ", "│ ■ ", "■ │ ", "  ■ "],
        None,
    );
    check(
        &commits("a:r b:q r: q:x,z z:x x:"),
        false,
        &["■     ", "│ ■   ", "■ │   ", "  ■─┐ ", "  │ ■ ", "  ■─┘ "],
        None,
    );
}

/// A join frees a lane and the same row's other parent takes it: the line coming in and the one
/// going down meet in `┤`, in the colour of the one going down, which goes on below.
#[test]
fn a_lane_freed_by_a_join_is_taken_in_the_same_row() {
    check(
        &commits("x:p y:p p:q,z z:q q:"),
        false,
        &["■   ", "│ ■ ", "■─┤ ", "│ ■ ", "■─┘ "],
        Some(&["c   ", "c y ", "cgg ", "c g ", "cgg "]),
    );
}

/// The stash rows: one on a loaded base, `≡` in its lane's colour beside the checked-out `◙`;
/// two on one base, which they join `┴` and `┘`; one whose base arrives on the next page — the
/// rows above stay as they were drawn; and one whose commits also come in through `--reflog`,
/// drawn once.
#[test]
fn the_stash_rows() {
    let loaded = &["◙   ", "│ ≡ ", "■─┘ ", "■   "];
    let mut stream = RowStream::new(Some(oid("c")), false, vec![stash("s", "b")], true);
    check(
        &stream.page(log("c:b b:a a:")),
        false,
        loaded,
        Some(&["c   ", "c y ", "cyy ", "c   "]),
    );

    let stashes = vec![stash("s1", "b"), stash("s0", "b")];
    let mut stream = RowStream::new(Some(oid("c")), false, stashes, true);
    check(
        &stream.page(log("c:b b:a a:")),
        false,
        &["◙     ", "│ ≡   ", "│ │ ≡ ", "■─┴─┘ ", "■     "],
        Some(&["c     ", "c y   ", "c y g ", "cyygg ", "c     "]),
    );

    let mut stream = RowStream::new(Some(oid("c")), false, vec![stash("s", "a")], true);
    let first = stream.page(log("c:b b:a"));
    let mut graph = check(&first, false, &["◙ ", "■ "], None);
    graph.extend(&stream.page(log("a:")));
    let cols = every_lane(&graph);
    assert_eq!(
        drawn(&graph, &cols, false),
        ["◙   ", "■   ", "│ ≡ ", "■─┘ "]
    );
    let alone = layout(&first, false);
    assert_eq!(
        rows(&alone, 0..2, &cols, false),
        rows(&graph, 0..2, &cols, false)
    );

    let records = "c:b s:b,s.i,s.u s.i:b s.u: b:a a:";
    let mut stream = RowStream::new(Some(oid("c")), false, vec![stash("s", "b")], true);
    check(&stream.page(log(records)), false, loaded, None);
}

/// Lanes past the Graph column's room are cut: a row with anything past the lanes shown has `»`
/// in the cut's first cell, dim; a row with nothing there has none. Each row has exactly the
/// column's cells, none when the column has none.
#[test]
fn lanes_past_the_room_are_cut_with_the_cut_glyph() {
    let graph = layout(&commits("o:a,b,c c:r b:r a:r r:z z:"), false);
    let cols = Columns::new(12, graph.width());
    assert_eq!((cols.graph, cols.lanes, cols.cut), (4, 1, true));
    assert_eq!(
        drawn(&graph, &cols, false),
        ["■─» ", "│ » ", "│ » ", "■ » ", "■─» ", "■   "]
    );
    assert_eq!(
        coloured(&graph, &cols, false),
        ["cyd ", "c d ", "c d ", "c d ", "cyd ", "c   "]
    );

    let cols = Columns::new(6, graph.width());
    assert_eq!((cols.graph, cols.lanes, cols.cut), (2, 0, true));
    assert_eq!(drawn(&graph, &cols, false), ["» "; 6]);
    let cols = Columns::new(3, graph.width());
    assert_eq!((cols.graph, cols.cut), (0, true));
    assert_eq!(drawn(&graph, &cols, false), [""; 6]);
}

/// A call works only on the cells the column shows: a history 2 000 lanes wide (lanes passing
/// by) and an octopus of 500 parents (its lines crossing them), asked for a 20-cell column,
/// allocate and draw no more than those cells, the lanes they show and, for each of a row's
/// segments, the cells it can cross — never the history's width — and give what the whole width
/// gives, cut.
#[test]
fn a_call_works_only_on_the_cells_the_column_shows() {
    let n = 2_000;
    let tips = (0..n).map(|i| format!("t{i}:b{i}"));
    let bases = (0..n).map(|i| format!("b{i}:"));
    let wide = layout(
        &commits(&tips.chain(bases).collect::<Vec<_>>().join(" ")),
        false,
    );
    let parents: Vec<String> = (0..500).map(|i| format!("p{i}")).collect();
    let roots: Vec<String> = parents.iter().map(|p| format!("{p}:")).collect();
    let spec = format!("o:{} {}", parents.join(","), roots.join(" "));
    let octopus = layout(&commits(&spec), false);
    assert_eq!((wide.width(), octopus.width()), (2_000, 500));
    for (graph, window) in [(&wide, n..n + 10), (&octopus, 0..10)] {
        let cols = Columns::new(60, graph.width());
        assert_eq!((cols.graph, cols.lanes, cols.cut), (20, 9, true));
        let shown = 2 * cols.lanes as usize;
        let bound: usize = (window.clone())
            .map(|i| cols.graph + cols.lanes as usize + graph.segments(i).len() * (shown + 1))
            .sum();
        WORK.with(|work| work.set(0));
        let cut = rows(graph, window.clone(), &cols, false);
        let work = WORK.with(|work| work.get());
        assert!(work <= bound, "{work} cells of work, bound {bound}");

        let whole = rows(graph, window.clone(), &every_lane(graph), false);
        for (i, (row, whole)) in window.zip(cut.iter().zip(&whole)) {
            assert_eq!(row[..shown], whole[..shown], "row {i}");
            let mark = past(graph, i, cols.lanes).then_some(Cell {
                glyph: look::CUT,
                colour: LOOK.cut,
            });
            assert_eq!((row[shown], row[shown + 1]), (mark, None), "row {i}");
        }
    }
}

/// The rows asked for are clamped to the rows laid out, as `Graph::pass_throughs` clamps them.
#[test]
fn the_rows_asked_for_are_clamped_to_the_rows_laid_out() {
    let graph = layout(&commits("m:c,f f:b c:b b:a a:"), false);
    let cols = every_lane(&graph);
    let all = rows(&graph, 0..graph.len(), &cols, false);
    assert_eq!(rows(&graph, 3..100, &cols, false), all[3..]);
    assert!(rows(&graph, 9..12, &cols, false).is_empty());
    assert!(rows(&layout(&[], false), 0..10, &every_lane(&graph), false).is_empty());
}

/// An ambiguous-wide terminal (10 §10, T38) gets the stand-ins at the source: box lines `| - +`,
/// `#` for `■`, `.` for `≡` and `○`; `◙`, `»` and the dotted `:` are narrow and stay. The window
/// keeps code page 437. The colours do not change.
#[test]
fn an_ambiguous_wide_terminal_gets_the_stand_ins() {
    for (glyph, want) in [
        ('│', '|'),
        ('─', '-'),
        ('┌', '+'),
        ('┐', '+'),
        ('└', '+'),
        ('┘', '+'),
        ('├', '+'),
        ('┤', '+'),
        ('┬', '+'),
        ('┴', '+'),
        ('┼', '+'),
        (look::COMMIT, '#'),
        (look::STASH, '.'),
        (look::UNCOMMITTED, '.'),
        (look::CHECKED_OUT, '◙'),
        (look::CUT, '»'),
        (look::DOTTED, ':'),
    ] {
        assert_eq!(stand_in(glyph, true), want, "{glyph}");
        assert_eq!(stand_in(glyph, false), glyph, "{glyph}");
    }

    let mut stream = RowStream::new(Some(oid("h")), true, Vec::new(), true);
    let graph = layout(&stream.page(log("t:s s:b h:b b:")), false);
    let cols = every_lane(&graph);
    assert_eq!(
        drawn(&graph, &cols, true),
        [".   ", ": # ", ": # ", "◙ | ", "#-+ "]
    );
    assert_eq!(
        coloured(&graph, &cols, true),
        coloured(&graph, &cols, false)
    );

    let stashes = vec![stash("s1", "b"), stash("s0", "b")];
    let mut stream = RowStream::new(Some(oid("c")), false, stashes, true);
    let graph = layout(&stream.page(log("c:b b:a a:")), false);
    assert_eq!(
        drawn(&graph, &every_lane(&graph), true),
        ["◙     ", "| .   ", "| | . ", "#-+-+ ", "#     "]
    );

    let graph = layout(&commits("m2:b,a m1:a,b b:o a:o o:"), false);
    assert_eq!(
        drawn(&graph, &every_lane(&graph), true),
        ["#-+   ", "+-+-# ", "# | | ", "| #-+ ", "#-+   "]
    );

    let graph = layout(&commits("o:a,b,c c:r b:r a:r r:z z:"), false);
    assert_eq!(
        drawn(&graph, &Columns::new(12, graph.width()), true),
        ["#-» ", "| » ", "| » ", "# » ", "#-» ", "#   "]
    );
}

/// The sides a lane glyph's lines leave its cell's middle to — up, down, left, right — written out
/// here apart from `chrome::git::line`, so a corner turned the wrong way there fails.
fn sides(c: char) -> [bool; 4] {
    const T: bool = true;
    const F: bool = false;
    match c {
        ' ' => [F, F, F, F],
        '│' | ':' => [T, T, F, F],
        '─' => [F, F, T, T],
        '┌' => [F, T, F, T],
        '┐' => [F, T, T, F],
        '└' => [T, F, F, T],
        '┘' => [T, F, T, F],
        '├' => [T, T, F, T],
        '┤' => [T, T, T, F],
        '┬' => [F, T, T, T],
        '┴' => [T, F, T, T],
        '┼' => [T, T, T, T],
        c => panic!("not a lane glyph: {c:?}"),
    }
}

/// Whether `c` is in code page 437, which the VGA font holds.
fn in_cp437(c: char) -> bool {
    (0..=255).any(|b| chrome::cp437(b) == c)
}

/// `chrome::git::line` gives each set of sides its box line, every one from code page 437; a
/// single side (no piece draws one) a straight line.
#[test]
fn every_set_of_sides_has_its_box_line() {
    for bits in 0..16u8 {
        let [up, down, left, right] = [1, 2, 4, 8].map(|b| bits & b != 0);
        let glyph = look::line(up, down, left, right);
        assert!(in_cp437(glyph), "{glyph:?}");
        let want = match bits.count_ones() {
            1 if up || down => [true, true, false, false],
            1 => [false, false, true, true],
            _ => [up, down, left, right],
        };
        assert_eq!(sides(glyph), want, "{bits:04b} gave {glyph:?}");
    }
}

/// A fixed-seed generator: a 64-bit LCG (Knuth's MMIX constants).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = (self.0)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

/// A random history in display order, children above parents: up to 14 commits of 0–3 parents,
/// some not loaded, some listed twice; at times HEAD, among them or not loaded, the uncommitted
/// row and up to two stashes; at times `--first-parent`.
fn random_rows(rng: &mut Lcg) -> (Vec<Row>, bool) {
    let n = 1 + rng.below(14);
    let mut records = Vec::new();
    for k in 0..n {
        let mut parents = Vec::new();
        for _ in 0..[0, 1, 1, 1, 1, 2, 2, 3][rng.below(8)] {
            if k + 1 < n && rng.below(6) > 0 {
                parents.push(format!("c{}", k + 1 + rng.below(n - k - 1)));
            } else {
                parents.push(format!("z{}", rng.below(3)));
            }
        }
        let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
        records.push(commit(&format!("c{k}"), &parents));
    }
    let head = match rng.below(4) {
        0 => None,
        // Not loaded yet: the dotted line runs to the bottom.
        1 => Some(oid("z9")),
        _ => Some(oid(&format!("c{}", rng.below(n)))),
    };
    let stashes = (0..rng.below(3))
        .map(|j| stash(&format!("s{j}"), &format!("c{}", rng.below(n))))
        .collect();
    let mut stream = RowStream::new(head, rng.below(2) == 0, stashes, true);
    (stream.page(records), rng.below(4) == 0)
}

/// The sides of every cell, every lane shown: a line's from its glyph ([`sides`]), a node's from
/// its segments — up and down where one runs on its own lane, left and right where one goes to a
/// lane on that side.
fn every_side(graph: &Graph, cells: &[Vec<Option<Cell>>]) -> Vec<Vec<[bool; 4]>> {
    let nodes = [
        look::COMMIT,
        look::CHECKED_OUT,
        look::STASH,
        look::UNCOMMITTED,
    ];
    let side = |i: usize, x: usize, cell: &Option<Cell>| {
        let node = graph.row(i);
        let glyph = cell.map_or(' ', |c| c.glyph);
        if x != 2 * node.column as usize {
            assert!(!nodes.contains(&glyph), "row {i} cell {x}: {glyph}");
            return sides(glyph);
        }
        assert!(nodes.contains(&glyph), "row {i} cell {x}: {glyph}");
        let segments = graph.segments(i);
        let on = |end: End| (segments.iter()).any(|s| s.end == end && s.slot == node.column);
        [
            on(End::Top),
            on(End::Bottom),
            segments.iter().any(|s| s.slot < node.column),
            segments.iter().any(|s| s.slot > node.column),
        ]
    };
    let row = |(i, row): (usize, &Vec<Option<Cell>>)| {
        (row.iter().enumerate())
            .map(|(x, cell)| side(i, x, cell))
            .collect()
    };
    cells.iter().enumerate().map(row).collect()
}

/// Whether row `i` has a piece on a lane at or past `lanes`: what `graph_view::lanes_of` marks
/// with the cut.
fn past(graph: &Graph, i: usize, lanes: u32) -> bool {
    graph.row(i).column >= lanes
        || graph.segments(i).iter().any(|s| s.slot >= lanes)
        || (graph.pass_throughs(i..i + 1).iter()).any(|p| p.slot >= lanes)
}

/// Over 1 000 graphs: the uncommitted row's dotted line is only ever vertical; every row has the
/// column's cells, glyphs from code page 437 (never `└`, nor `┌` alone, 03 §4.4) and, in an
/// ambiguous-wide terminal, none of ambiguous width; a lane's second cell holds only a lane
/// change's line; lines meet across cells and rows; the visible rows' cells are those of the
/// whole, and the first rows' those of all rows (paging never redraws the rows above); a narrower
/// column only cuts, with `»` where `lanes_of` puts it — in the column: one of no cells has none,
/// where `lanes_of` paints it over the description.
#[test]
fn properties_over_a_thousand_graphs() {
    let mut rng = Lcg(0x5eed_1a9e);
    for _ in 0..1_000 {
        let (list, first_parent) = random_rows(&mut rng);
        let graph = layout(&list, first_parent);
        let len = graph.len();
        for i in 0..len {
            let column = graph.row(i).column;
            for s in graph.segments(i).iter().filter(|s| s.dashed) {
                assert_eq!(s.slot, column, "a dotted lane change on row {i}");
            }
        }
        let cols = every_lane(&graph);
        let cells = rows(&graph, 0..len, &cols, false);
        let wide = rows(&graph, 0..len, &cols, true);
        assert_eq!(cells.len(), len);
        for (row, wide_row) in cells.iter().zip(&wide) {
            assert_eq!(row.len(), cols.graph);
            assert_eq!(wide_row.len(), cols.graph);
            for cell in row.iter().flatten() {
                assert!(in_cp437(cell.glyph), "{:?}", cell.glyph);
                // Joins come from the right; a line to the left ends on a line passing by.
                assert!(!matches!(cell.glyph, '└' | '┌'), "{:?}", cell.glyph);
            }
            for cell in wide_row.iter().flatten() {
                assert!(!term::ambiguous(cell.glyph), "{:?}", cell.glyph);
            }
            for cell in row.iter().skip(1).step_by(2).flatten() {
                assert_eq!(cell.glyph, '─');
            }
        }

        let side = every_side(&graph, &cells);
        for (i, row) in side.iter().enumerate() {
            for x in 0..row.len() {
                let [up, down, _, right] = row[x];
                if x + 1 < row.len() {
                    assert_eq!(right, row[x + 1][2], "row {i}, cells {x} and {}", x + 1);
                }
                if i == 0 {
                    assert!(!up, "row 0 cell {x} comes from above");
                }
                if i + 1 < len {
                    assert_eq!(down, side[i + 1][x][0], "rows {i} and {}, cell {x}", i + 1);
                }
            }
        }

        let a = rng.below(len + 1);
        let b = a + rng.below(len + 1 - a);
        assert_eq!(rows(&graph, a..b, &cols, false), cells[a..b]);
        let k = 1 + rng.below(len);
        let first = layout(&list[..k], first_parent);
        assert_eq!(rows(&first, 0..k, &cols, false), cells[..k]);

        let narrow = Columns::new(rng.below(40), graph.width().max(1));
        let cut = rows(&graph, 0..len, &narrow, false);
        let shown = 2 * narrow.lanes as usize;
        for (i, row) in cut.iter().enumerate() {
            assert_eq!(row.len(), narrow.graph);
            if !narrow.cut {
                assert_eq!(row, &cells[i]);
                continue;
            }
            let lanes = shown.min(row.len());
            assert_eq!(row[..lanes], cells[i][..lanes], "row {i}");
            if shown < row.len() {
                let mark = row[shown].map(|c| (c.glyph, c.colour));
                let want = past(&graph, i, narrow.lanes).then_some((look::CUT, LOOK.cut));
                assert_eq!(mark, want, "row {i}");
                assert!(row[shown + 1..].iter().all(Option::is_none));
            }
        }
    }
}
