//! F-dialog-wrap (`.prd/xtools/loop.md`): `dialog::wrap` walks a text once — it measured what was
//! left of it at each row, quadratic in a long line's length — and gives the rows it gave before,
//! byte for byte (the old one, kept here, is the reference). And the kit's rows that scroll come
//! shared ([`Part::Wrapped`]), wrapped by the kit once a width: a pass copies, wraps and measures
//! none of them and draws only those it shows, and they show, scroll and cut as each row wrapped
//! at the box's width by `dialog::wrap`.

use super::*;
use crate::gui::cells::Pen;
use dialog::{work, Wrapped};
use unicode_segmentation::UnicodeSegmentation;

const CELL: Vec2 = chrome::CELL;

/// `dialog::wrap` as it was at 87f78b4: the reference its rows are compared with.
fn old_wrap(text: &str, room: usize) -> Vec<String> {
    let room = room.max(1);
    let mut rows = vec![];
    let mut rest = text;
    // After a break, blanks alone — the text's own trailing ones — make no row, however wide.
    while cells::width(rest) > room && (rows.is_empty() || !rest.trim().is_empty()) {
        let (mut used, mut cut, mut blank) = (0, 0, None);
        for (i, g) in rest.grapheme_indices(true) {
            let w = cells::width(g);
            if g == " " && i > 0 {
                blank = Some(i);
            }
            if used + w > room {
                break;
            }
            used += w;
            cut = i + g.len();
        }
        let (row, next) = match blank {
            Some(b) if b <= cut => (&rest[..b], &rest[b + 1..]),
            // A glyph wider than the row alone: it goes on a row of its own, cut there.
            _ if cut == 0 => {
                let first = rest.graphemes(true).next().map_or(rest.len(), str::len);
                (&rest[..first], &rest[first..])
            }
            _ => (&rest[..cut], &rest[cut..]),
        };
        rows.push(row.to_string());
        rest = next;
    }
    if !rest.trim().is_empty() || rows.is_empty() {
        rows.push(rest.to_string());
    }
    rows
}

/// A small deterministic generator (splitmix64), as `encoding/tests.rs` has.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// What a rewrite of the walk gets wrong: blanks (several in a row too), a glyph of two cells
/// (Hangul, a CJK ideograph, an emoji's sequence), the ideographic space and a no-break space —
/// blanks to `trim`, never a place to break — a combining mark, a zero-width joiner and a
/// control character (no cells: they join a full row), a tab, a line's end alone and with its
/// carriage return, `…` and a box line.
const PIECES: [&str; 22] = [
    "a",
    "b",
    "word",
    " ",
    " ",
    "  ",
    "가",
    "한글",
    "漢",
    "👩\u{200d}💻",
    "❤\u{fe0f}",
    "\u{3000}",
    "\u{a0}",
    "e\u{301}",
    "\u{200d}",
    "\u{7}",
    "\t",
    "\n",
    "\r\n",
    "…",
    "─",
    "x",
];

/// On random texts of up to sixty pieces, at every room from 0 to 24, the rows are the old wrap's.
#[test]
fn fdw_wrap_gives_the_old_wraps_rows() {
    let mut rng = Rng(0x5eed_d1a1_0600);
    for case in 0..3_000 {
        let n = rng.below(60);
        let text: String = (0..n).map(|_| PIECES[rng.below(PIECES.len())]).collect();
        for room in 0..25 {
            assert_eq!(
                dialog::wrap(&text, room),
                old_wrap(&text, room),
                "case {case}: {text:?} at {room}"
            );
        }
    }
}

/// A line of a mebibyte — without a blank (Hangul and a letter), and of words — wraps in one walk.
/// What is counted is every grapheme any width measure walks on the thread (`cells::measured`),
/// wherever in the wrap it is asked — the old wrap's `cells::width(rest)` in its loop's condition
/// too: about the line's once (the last glyphs of a row broken at a blank again, a row's first
/// glyph past it). A row is what the old wrap made of it: the same text, at most the room wide,
/// broken where it broke. On the line's first 4 KiB the old wrap gives the same rows, and through
/// the same count it is past that bound — the shape the bound catches.
#[test]
fn fdw_a_mebibyte_line_wraps_in_one_walk() {
    for (piece, room) in [("가x", 7), ("x", 80), ("단어 word ", 80), ("a bc  def ", 9)] {
        let text = piece.repeat((1 << 20) / piece.len());
        let n = text.graphemes(true).count();
        cells::measured::take();
        // Past it, the walk is the old one's: it stops at once instead of taking minutes.
        let budget = cells::measured::budget(3 * n);
        let rows = dialog::wrap(&text, room);
        let measured = cells::measured::take();
        drop(budget);
        assert!(
            (n..=2 * n + rows.len()).contains(&measured),
            "{piece:?}: {measured} graphemes measured for {n}"
        );
        assert!(rows.iter().all(|r| cells::width(r) <= room), "{piece:?}");
        match piece.contains(' ') {
            // A break at a blank drops it: the words are the line's, in order.
            true => assert_eq!(rows.join(" ").trim_end(), text.trim_end(), "{piece:?}"),
            false => assert_eq!(rows.concat(), text, "{piece:?}"),
        }
        let head: String = text.chars().take(4 << 10).collect();
        let h = head.graphemes(true).count();
        cells::measured::take();
        let new = dialog::wrap(&head, room);
        let walked = cells::measured::take();
        let old = old_wrap(&head, room);
        let old_walked = cells::measured::take();
        assert_eq!(new, old, "{piece:?}");
        let bound = 2 * h + new.len();
        assert!((h..=bound).contains(&walked), "{piece:?}: {walked} for {h}");
        assert!(
            old_walked > bound,
            "{piece:?}: the old wrap's {old_walked} for {h}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The rows that scroll, shared.

/// The dialog's nominal width: the result's, past the kit's widest, so its inside is the grid's
/// less eight up to 80 cells whatever its parts want.
const NOMINAL: usize = 80;

/// One dialog of `parts` over a screen of cells, its rows that scroll from `top` — or, `fresh`,
/// those rows made afresh each pass, as a dialog that does not keep its rows makes them.
struct Host {
    parts: Vec<Part>,
    top: usize,
    fresh: Option<Vec<(String, Ink)>>,
}

impl Window for Host {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let grid = dialog::grid(&ctx);
        let made = (self.fresh.as_ref()).map(|rows| {
            around(Part::Wrapped {
                rows: Wrapped::new(rows.clone()),
                most: 12,
                top: 0,
            })
        });
        let parts = made.as_ref().unwrap_or(&self.parts);
        let layout = dialog::layout(grid, "Rows", NOMINAL, parts);
        // The screen under it, the cells it covers blanked first (cells.rs's rule).
        let body = Pen::new(chrome::PALETTE.text, chrome::PALETTE.back);
        let mut under = Buffer::empty(grid.all());
        cells::fill(&mut under, grid.all(), body);
        let words = "한글 under ".repeat(20);
        for y in 0..grid.rows {
            cells::put(&mut under, 0, y, &words, usize::from(grid.cols), body);
        }
        for c in layout.covers() {
            cells::fill(&mut under, c, body);
        }
        cells::paint(ui.painter(), grid.origin, &under, None);
        let inputs = Inputs {
            top: Some(&mut self.top),
            ..Inputs::default()
        };
        dialog::show(&ctx, egui::Id::unique("sopen-fdw-dialog"), &layout, inputs);
    }

    fn ready(&self) -> bool {
        true
    }
}

/// A result's rows before they are wrapped: steps past the box's right border, blank rows, Hangul,
/// `…`, a row of no blank longer than the box, the ideographic space, in the three inks.
fn report() -> Vec<(String, Ink)> {
    (0..40)
        .map(|k| {
            let ink = [Ink::Text, Ink::Error, Ink::Weak][k % 3];
            let text = match k % 5 {
                0 => format!(
                    "failed   git push origin refs/heads/가지-{k}:refs/heads/가지-{k} with words \
                     that go on past the box's right border"
                ),
                1 => String::new(),
                2 => format!("    remote: 한글 줄 {k} … 끝"),
                3 => "x".repeat(120),
                _ => format!("line {k}\u{3000}with an ideographic space"),
            };
            (text, ink)
        })
        .collect()
}

/// The dialog's parts around its rows that scroll: a command line that wraps under them (and is
/// cut to `…` on a short grid), `[ OK ]` and the hint.
fn around(rows: Part) -> Vec<Part> {
    // Two rows in a box of 80 cells; thirteen in one of 8, which leaves the bar seven.
    let line = "git push --progress origin refs/heads/b:refs/heads/b and a long command line that \
                wraps";
    vec![
        rows,
        Part::Text(line.to_string(), Ink::Weak),
        Part::Blank,
        Part::Buttons(vec![Button::new("OK", egui::Id::unique("sopen-fdw-ok"))]),
        Part::Text("Enter or Esc closes".to_string(), Ink::Weak),
    ]
}

/// The rows of [`report`], each wrapped at the inside of a box on a grid `cols` wide.
fn wrapped_for(cols: u16) -> Arc<Wrapped> {
    let width = dialog::inside_width(cols, NOMINAL, NOMINAL);
    Wrapped::new(
        (report().into_iter())
            .flat_map(|(t, ink)| dialog::wrap(&t, width).into_iter().map(move |r| (r, ink)))
            .collect(),
    )
}

/// What the pass drew in cells: each layer's cells and caret.
fn drawn(drv: &Driver) -> Vec<(Buffer, Option<Position>)> {
    (cells::layers(&drv.ctx).iter())
        .map(|l| (l.buf.clone(), l.caret))
        .collect()
}

/// Rows the kit wraps show as each row wrapped at the box's width by `dialog::wrap`, the rule
/// rows that scroll always had: the same cells — rows, inks, Hangul's two cells, the clip at the
/// box's edge, the scroll bar and its thumb, the command line under them and its `…` where the
/// grid is short — on a wide grid, a grid of sixteen columns and one of nine rows; and the same
/// scroll, two wheel notches in one pass, a click on the bar's ↓, and one on its track below the
/// thumb.
#[test]
fn fdw_shared_rows_show_as_each_row_wrapped_at_the_box() {
    for (cols, rows) in [(100u16, 30u16), (16, 30), (100, 9)] {
        let (w, h) = (f32::from(cols) * CELL.x, f32::from(rows) * CELL.y);
        let kits = Part::Wrapped {
            rows: Wrapped::new(report()),
            most: 12,
            top: 0,
        };
        let own = Part::Wrapped {
            rows: wrapped_for(cols),
            most: 12,
            top: 0,
        };
        let mut hosts = [kits, own].map(|p| Host {
            parts: around(p),
            top: 0,
            fresh: None,
        });
        let mut drvs = [Driver::new(w, h), Driver::new(w, h)];
        let same = |drvs: &[Driver; 2], hosts: &[Host; 2], what: &str| {
            assert_eq!(drawn(&drvs[0]), drawn(&drvs[1]), "{cols}×{rows}: {what}");
            assert_eq!(hosts[0].top, hosts[1].top, "{cols}×{rows}: {what}");
        };
        for k in 0..2 {
            drvs[k].idle(&mut hosts[k], 2);
        }
        same(&drvs, &hosts, "first look");
        let shown = cells::find(&cells::layers(&drvs[0].ctx), "failed");
        let first = shown.first().expect("a step's row").cells;
        let notch = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: Vec2::new(0.0, -1.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: Modifiers::NONE,
        };
        for k in 0..2 {
            let at = Pos2::new(
                (f32::from(first.x) + 0.5) * CELL.x,
                (f32::from(first.y) + 0.5) * CELL.y,
            );
            drvs[k].move_to(&mut hosts[k], at);
            drvs[k].frame(
                &mut hosts[k],
                vec![notch.clone(), notch.clone()],
                Modifiers::NONE,
            );
            drvs[k].idle(&mut hosts[k], 1);
        }
        same(&drvs, &hosts, "two notches in one pass");
        assert!(hosts[0].top > 0, "{cols}×{rows}: the wheel scrolled");
        // On the short grid one row is left, too few for a bar to take the pointer.
        if rows < 30 {
            continue;
        }
        // The bar on the box's right border: its ↓, then its track just above the ↓.
        let down = cells::find(&cells::layers(&drvs[0].ctx), "↓");
        let down = down.last().expect("the scroll bar's ↓").cells;
        for (at, what) in [(down.y, "the ↓"), (down.y - 1, "the track")] {
            let p = Pos2::new(
                (f32::from(down.x) + 0.5) * CELL.x,
                (f32::from(at) + 0.5) * CELL.y,
            );
            let before = hosts[0].top;
            for k in 0..2 {
                drvs[k].click(&mut hosts[k], p);
                drvs[k].idle(&mut hosts[k], 1);
            }
            same(&drvs, &hosts, what);
            assert!(hosts[0].top > before, "{cols}×{rows}: {what} scrolled");
        }
    }
}

/// 1,200 rows shared (the result's 64 KiB of stderr): ten passes without input copy, wrap and
/// measure none of them and draw only the rows the box shows; no pass keeps them. The same rows
/// made afresh each pass are measured and wrapped again each pass — the copy the count sees.
#[test]
fn fdw_shared_rows_cost_their_view_a_pass() {
    // Wider than the box: wrapped, the rows made afresh are copied each pass.
    let rows: Vec<(String, Ink)> = (0..1200)
        .map(|k| {
            let text =
                format!("remote: line {k:04} of a long refusal, in words that run past the box");
            (format!("{text} and wrap under it"), Ink::Text)
        })
        .collect();
    let bytes: usize = rows.iter().map(|(t, _)| t.len()).sum();
    let shared = Wrapped::new(rows.clone());
    for kind in ["shared", "fresh"] {
        let part = Part::Wrapped {
            rows: shared.clone(),
            most: 12,
            top: 0,
        };
        let mut h = Host {
            parts: around(part),
            top: 0,
            fresh: (kind == "fresh").then(|| rows.clone()),
        };
        let mut drv = Driver::new(800.0, 480.0);
        drv.idle(&mut h, 2);
        work::take();
        drv.idle(&mut h, 10);
        let done = work::take();
        assert!(done.drawn > 0 && done.drawn <= 10 * 12, "{kind}: {done:?}");
        match kind {
            "shared" => {
                assert_eq!((done.copied, done.measured), (0, 0), "{done:?}");
                assert!(done.wrapped < bytes / 10, "only its texts: {done:?}");
            }
            _ => {
                // Each pass its rows again, less the blanks the breaks drop.
                assert!(done.copied >= 9 * bytes, "{done:?}");
                assert!(done.measured >= 10 * rows.len(), "{done:?}");
            }
        }
        drop(h);
    }
    assert_eq!(Arc::strong_count(&shared), 1, "no pass keeps them");
}

/// A report of 500 items (its rows a reason each that wraps), over ten idle passes: its rows reach
/// the kit kept and shared — no pass copies, wraps or measures them — and only the sixteen the box
/// shows are drawn; the rows still show.
#[test]
fn fdw_a_long_report_copies_no_rows_a_pass() {
    let names: Vec<String> = (0..500).map(|k| format!("file-{k:03}.txt")).collect();
    let why = "the server refused it: no room left on the device, and nothing else was written";
    let report = Report {
        items: (names.iter())
            .map(|n| item(n, ItemOutcome::Failed(why.to_string())))
            .collect(),
        notes: vec!["the last note".to_string()],
    };
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let Finished::Report(mut view) = finish(&copy_job(&refs), report) else {
        panic!("a report");
    };
    struct One<'a>(&'a mut ReportView);
    impl Window for One<'_> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            self.0.ui(ui.ctx(), dialog::grid(ui.ctx()));
        }
        fn ready(&self) -> bool {
            true
        }
    }
    let mut drv = Driver::new(800.0, 480.0);
    let mut win = One(&mut view);
    drv.idle(&mut win, 2);
    work::take();
    drv.idle(&mut win, 10);
    let done = work::take();
    assert_eq!((done.copied, done.measured), (0, 0), "{done:?}");
    assert!(done.drawn > 0 && done.drawn <= 10 * 16, "{done:?}");
    let shown = cells::find(&cells::layers(&drv.ctx), "failed      file-000.txt");
    assert!(!shown.is_empty(), "its first item");
}

/// One `Wrapped` laid out at one width, another, and the first again: each layout's rows are its
/// rows wrapped at its own width — the kit wraps them again when the width changes, a copy — and
/// a second layout at the width kept copies none of them.
#[test]
fn fdw_one_wrapped_at_two_widths() {
    let shared = Wrapped::new(report());
    let parts = around(Part::Wrapped {
        rows: shared.clone(),
        most: 12,
        top: 0,
    });
    for cols in [100u16, 40, 100, 40] {
        let grid = Grid {
            origin: Pos2::ZERO,
            cols,
            rows: 30,
        };
        let width = dialog::inside_width(cols, NOMINAL, NOMINAL);
        let want: Vec<String> = (report().iter())
            .flat_map(|(t, _)| dialog::wrap(t, width))
            .collect();
        work::take();
        let laid = dialog::layout(grid, "Rows", NOMINAL, &parts).scroll_rows();
        let copied = work::take().copied;
        assert_eq!(laid, want, "{cols} columns, {width} cells inside");
        assert!(copied > 0, "wrapped again at {cols} columns");
        dialog::layout(grid, "Rows", NOMINAL, &parts);
        assert_eq!(work::take().copied, 0, "kept at {cols} columns");
    }
}
