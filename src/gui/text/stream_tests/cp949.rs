//! 08 §7.4 (VM-4) through the stream (the Fake): EA-3's ranged reads of CP949, inside the 16 MiB
//! index and past it, each held to the whole-file decode (08 §12) — a place proven by a sync
//! byte, by its line's start less than REACH back (a grid cut too), by the phase checkpoints a
//! line of the index gets lazily through the stream, or by nothing: placeholder marks, never a
//! character the whole-file decode lacks.

use super::super::tests::cp949::{check_rows, korean_lines, syllables, Whole};
use super::*;
use crate::encoding::{Encoding::Cp949, W};

/// A line: `head` (ASCII), then `n` syllables — with a space after every `spaced` of them when
/// given — then LF. Where it starts and ends (before its LF).
fn long_line(body: &mut Vec<u8>, head: &[u8], n: usize, spaced: Option<usize>) -> (u64, u64) {
    let start = body.len() as u64;
    body.extend_from_slice(head);
    match spaced {
        Some(k) => {
            for i in 0..n / k {
                body.extend(syllables(k + i % 3));
                body.push(b' ');
            }
        }
        None => body.extend(syllables(n)),
    }
    let end = body.len() as u64;
    body.push(b'\n');
    (start, end)
}

/// Korean CRLF lines up to `to` bytes (and a little past).
fn lines_to(body: &mut Vec<u8>, block: &[u8], to: usize) {
    while body.len() < to {
        body.extend_from_slice(block);
    }
}

/// **EA-3 (i)–(iv)** (VM-4) through the Fake. Inside the index: (ii) a 256 KiB line of Hangul
/// with no sync byte, Go to 100 KiB past its start → placeholder marks and the status line
/// while its checkpoints are read through the stream (held here), then rows equal to the
/// whole-file decode, its first six GRID points read. Past 16 MiB: (i) a 256 KiB line of
/// Hangul with spaces, 100 KiB in → exact by the last sync byte; (iii) the line of (ii) → marks
/// with the reason, never a syllable the decode lacks, and no checkpoint asked for (its start is
/// not known); (iv) a line shorter than REACH, its start off the grid, past its first piece's
/// grid cut → exact, by the line start (the cut and its piece too).
#[test]
fn ea3_cp949_inside_and_past_the_16_mib_index_reads_as_the_whole_file_decode() {
    bounded("CP949 inside and past 16 MiB", || {
        let (block, _) = korean_lines(400);
        let mut body = Vec::new();
        lines_to(&mut body, &block, 1 << 20);
        let ii = long_line(&mut body, b"a", 128 * 1024, None);
        lines_to(&mut body, &block, HIGHLIGHT_MAX as usize + (1 << 20));
        let i = long_line(&mut body, b"a", 128 * 1024, Some(20));
        let to = body.len() + 200 * 1024;
        lines_to(&mut body, &block, to);
        let iii = long_line(&mut body, b"a", 128 * 1024, None);
        let to = body.len() + 200 * 1024;
        lines_to(&mut body, &block, to);
        // (iv) starts 10 KiB past a grid point: its first piece ends 54 KiB in.
        let off = (body.len() as u64 / GRID + 1) * GRID + 10 * 1024 - 1;
        body.extend(std::iter::repeat_n(
            b'x',
            (off - body.len() as u64) as usize,
        ));
        body.push(b'\n');
        let iv = long_line(&mut body, b"a", 30 * 1024, None);
        let to = body.len() + 200 * 1024;
        lines_to(&mut body, &block, to);
        // (iv) again with two ASCII bytes first: no lead pending at its first piece's cut, so
        // the piece after it begins on the grid point itself, proven by the line start too.
        let off = (body.len() as u64 / GRID + 1) * GRID + 10 * 1024 - 1;
        body.extend(std::iter::repeat_n(
            b'x',
            (off - body.len() as u64) as usize,
        ));
        body.push(b'\n');
        let iv_b = long_line(&mut body, b"ab", 30 * 1024, None);
        let to = body.len() + 200 * 1024;
        lines_to(&mut body, &block, to);
        // (v) spaced, then a stretch with no sync byte across a grid point more than W into it
        // (a lead pending there), then spaced again.
        let v0 = body.len() as u64;
        body.push(b'a');
        for i in 0..4000 {
            body.extend(syllables(10 + i % 3));
            body.push(b' ');
        }
        // The stretch begins 8 KiB before g: the piece before g starts proven (a sync byte
        // within W of g − GRID), and ends at g with none.
        while body.len() as u64 % GRID != GRID / 2 {
            body.push(b' ');
        }
        let g = body.len() as u64 + GRID / 2;
        assert!(g - v0 > REACH + GRID, "the line start is out of reach");
        let stretch = g + 4 * 1024 - body.len() as u64;
        body.extend(syllables(stretch as usize / 2 + 1));
        if !Cp949.phase(&body[v0 as usize..g as usize]).unwrap() {
            body.insert(g as usize - 2000, b'b');
        }
        assert!(
            Cp949.phase(&body[v0 as usize..g as usize]).unwrap(),
            "a lead at g − 1"
        );
        assert!(!body[g as usize - W..g as usize].iter().any(|&b| b <= 0x40));
        for i in 0..2000 {
            body.extend(syllables(10 + i % 3));
            body.push(b' ');
        }
        let v = (v0, body.len() as u64);
        body.push(b'\n');
        assert_eq!(iv.0 % GRID, 10 * 1024);
        assert!(iv.1 - iv.0 < REACH);
        let to = body.len() + 100 * 1024;
        lines_to(&mut body, &block, to);
        let whole = |(s, e): (u64, u64)| Whole::of(&body, s, e);
        let (w_i, w_ii, w_iii, w_iv, w_v) = (whole(i), whole(ii), whole(iii), whole(iv), whole(v));
        let w_iv_b = whole(iv_b);

        let mut net = net(&body, gate());
        let mut win = net.window("big.txt");
        let (go, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        win.phase_hook = Some(Arc::new(move || {
            let _ = wait.lock().unwrap().recv();
        }));
        let mut drv = Driver::new(500.0, 200.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
            w.doc.size() >= HIGHLIGHT_MAX
        });
        net.settle();
        drv.idle(&mut win, 3);
        assert_eq!(win.doc.encoding_label().as_deref(), Some("CP949 (guessed)"));
        let settled = |w: &TextWindow, _: &Driver| w.ready() && w.jump.is_none() && has_rows(w);
        let go_to = |drv: &mut Driver, win: &mut TextWindow, net: &mut Net, at: u64| {
            win.jump_to(Jump::Byte(at));
            run(drv, win, net, "the jump", |w, _| {
                w.jump.is_none() && has_rows(w) && !w.pending
            });
        };

        // (ii) inside the index: marks while the checkpoints are read, then the decode.
        let at = ii.0 + 100 * 1024;
        go_to(&mut drv, &mut win, &mut net, at);
        drv.idle(&mut win, 2);
        let read = win.phase_read.as_ref().expect("a checkpoint read");
        assert_eq!((read.line, read.from), (ii.0, 0));
        assert!(!win.ready());
        let rows = win.page.as_ref().unwrap().rows.len();
        assert_eq!(check_rows(&win, &w_ii, "(ii) before"), rows);
        // (The status row is cut to its cells: its start.)
        assert!(
            says(&drv, "CP949: no character boundary"),
            "{:?}",
            painted(&drv)
        );
        let _ = go.send(());
        run(
            &mut drv,
            &mut win,
            &mut net,
            "(ii) the checkpoints",
            settled,
        );
        assert_eq!(check_rows(&win, &w_ii, "(ii)"), 0);
        assert_eq!(
            win.doc.phases.lines[&ii.0].len(),
            6,
            "the line's first 96 KiB read"
        );
        win.phase_hook = None;

        // (i) past 16 MiB, spaced: exact by the last sync byte; and across its pieces.
        go_to(&mut drv, &mut win, &mut net, i.0 + 100 * 1024);
        run(&mut drv, &mut win, &mut net, "(i)", settled);
        assert!(
            win.page.as_ref().unwrap().rows[0].loose,
            "its start not known"
        );
        assert_eq!(check_rows(&win, &w_i, "(i)"), 0);
        for key in [
            Key::PageUp,
            Key::PageUp,
            Key::PageDown,
            Key::PageDown,
            Key::PageDown,
        ] {
            drv.key(&mut win, key, Modifiers::NONE);
            run(&mut drv, &mut win, &mut net, "(i) paging", settled);
            assert_eq!(check_rows(&win, &w_i, &format!("(i) {key:?}")), 0);
        }

        // (iii) past 16 MiB, no sync byte: marks with the reason, nothing asked.
        go_to(&mut drv, &mut win, &mut net, iii.0 + 100 * 1024);
        run(&mut drv, &mut win, &mut net, "(iii)", settled);
        drv.idle(&mut win, 1);
        let rows = win.page.as_ref().unwrap().rows.len();
        assert_eq!(check_rows(&win, &w_iii, "(iii)"), rows, "all marks");
        assert!(win.phase_read.is_none() && win.doc.phases.wanted.is_none());
        // The status row says why, cut to its cells after the place: its start shows.
        let said = win.status_line().1.text;
        assert!(
            said.starts_with("CP949: no character boundary known here"),
            "{said}"
        );
        assert!(says(&drv, "CP949: n"), "{:?}", painted(&drv));
        for key in [Key::PageUp, Key::PageDown, Key::PageDown] {
            drv.key(&mut win, key, Modifiers::NONE);
            run(&mut drv, &mut win, &mut net, "(iii) paging", settled);
            check_rows(&win, &w_iii, &format!("(iii) {key:?}"));
        }

        // (iv) a line shorter than REACH past 16 MiB, past its first piece's grid cut.
        let cut = iv.0 / GRID * GRID + REACH;
        assert!(iv.0 < cut && cut < iv.1);
        for at in [iv.0 + 2, cut - 1, cut + 1, cut + 3 * 1024] {
            go_to(&mut drv, &mut win, &mut net, at);
            run(&mut drv, &mut win, &mut net, "(iv)", settled);
            assert_eq!(check_rows(&win, &w_iv, &format!("(iv) {at}")), 0);
        }
        drv.key(&mut win, Key::PageUp, Modifiers::NONE);
        run(&mut drv, &mut win, &mut net, "(iv) back", settled);
        assert_eq!(check_rows(&win, &w_iv, "(iv) back"), 0);
        let cut = iv_b.0 / GRID * GRID + REACH;
        assert!(!Cp949.phase(&body[iv_b.0 as usize..cut as usize]).unwrap());
        for at in [cut - 1, cut + 1, cut + 3 * 1024] {
            go_to(&mut drv, &mut win, &mut net, at);
            run(&mut drv, &mut win, &mut net, "(iv) b", settled);
            assert_eq!(check_rows(&win, &w_iv_b, &format!("(iv) b {at}")), 0);
        }

        // (v) a piece proven from its start that ends at a grid point nothing proves: the lead
        // pending there is shown by neither piece, never as a U+FFFD the decode lacks; the piece
        // after is marks up to its first sync byte.
        for at in [g - 300, g - 40, g - 1, g + 10] {
            go_to(&mut drv, &mut win, &mut net, at);
            run(&mut drv, &mut win, &mut net, "(v)", settled);
            check_rows(&win, &w_v, &format!("(v) {at}"));
            for key in [Key::PageDown, Key::PageUp, Key::PageUp] {
                drv.key(&mut win, key, Modifiers::NONE);
                run(&mut drv, &mut win, &mut net, "(v) paging", settled);
                check_rows(&win, &w_v, &format!("(v) {at} {key:?}"));
            }
        }
        // The piece from g is marks only up to its first sync byte, characters from there.
        let first_sync = (g as usize..).find(|&i| body[i] == b' ').unwrap() as u64;
        go_to(&mut drv, &mut win, &mut net, first_sync - 20);
        run(
            &mut drv,
            &mut win,
            &mut net,
            "(v) its first sync byte",
            settled,
        );
        check_rows(&win, &w_v, "(v) its first sync byte");
        let rows = &win.page.as_ref().unwrap().rows;
        let row = rows
            .iter()
            .find(|r| {
                let at = r.at + r.start;
                at <= first_sync && first_sync < at + r.cut.bytes as u64
            })
            .expect("the row holding the sync byte");
        assert!(row.loose, "a piece from the grid");
        let at = row.at + row.start;
        assert_eq!(
            at + row.cut.dim as u64,
            first_sync,
            "marks up to it, no further"
        );
        assert!(row.cut.dim < row.cut.shown.len(), "characters from it");
    });
}

/// The index's open line (§7.4): a 400 KiB line of Hangul with no sync byte that starts on a grid
/// point 200 KiB before the 16 MiB cap and runs past it — its start is the index's, the rest of
/// it pieces. The cut REACH past its start (no line-start proof there: it is REACH away, as a
/// jump's look-back sees it) asks for the line's checkpoints and is exact once they are in,
/// reached from the line start's piece or from the piece after; a place further on reads on
/// from the last checkpoint (to the grid point that ends its piece); past the cap nothing
/// proves a start: marks, nothing asked.
#[test]
fn the_open_lines_grid_cuts_take_its_checkpoints_inside_the_index() {
    bounded("CP949's open line", || {
        let (block, _) = korean_lines(400);
        let mut body = Vec::new();
        lines_to(&mut body, &block, HIGHLIGHT_MAX as usize - 200 * 1024);
        let next = (body.len() as u64 / GRID + 2) * GRID;
        body.extend(std::iter::repeat_n(b'x', (next - 1) as usize - body.len()));
        body.push(b'\n');
        let o = long_line(&mut body, b"a", 200 * 1024, None);
        let to = body.len() + 100 * 1024;
        lines_to(&mut body, &block, to);
        let bound = o.0 + REACH;
        assert_eq!(o.0 % GRID, 0);
        assert!(bound + 4 * GRID < HIGHLIGHT_MAX && o.1 > HIGHLIGHT_MAX + 100 * 1024);
        let w_o = Whole::of(&body, o.0, o.1);

        let mut net = net(&body, gate());
        let mut win = net.window("open.txt");
        let (go, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        win.phase_hook = Some(Arc::new(move || {
            let _ = wait.lock().unwrap().recv();
        }));
        let mut drv = Driver::new(500.0, 200.0);
        run(&mut drv, &mut win, &mut net, "the first screen", |w, _| {
            has_rows(w)
        });
        run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
            w.doc.size() >= HIGHLIGHT_MAX
        });
        net.settle();
        drv.idle(&mut win, 3);
        assert_eq!(win.doc.known().1, o.0, "the index ends inside the line");
        let settled = |w: &TextWindow, _: &Driver| w.ready() && w.jump.is_none() && has_rows(w);
        let go_to = |drv: &mut Driver, win: &mut TextWindow, net: &mut Net, at: u64| {
            win.jump_to(Jump::Byte(at));
            run(drv, win, net, "the jump", |w, _| {
                w.jump.is_none() && has_rows(w) && !w.pending
            });
        };

        // Past the cut REACH in: marks while the line's checkpoints are read — to the grid point
        // that ends the piece holding the place (its end is a cut too): the first five.
        go_to(&mut drv, &mut win, &mut net, bound + 100);
        drv.idle(&mut win, 2);
        let read = win.phase_read.as_ref().expect("a checkpoint read");
        assert_eq!((read.line, read.from), (o.0, 0));
        assert!(check_rows(&win, &w_o, "before") > 0);
        let _ = go.send(());
        run(&mut drv, &mut win, &mut net, "the checkpoints", settled);
        assert_eq!(check_rows(&win, &w_o, "after"), 0);
        assert_eq!(win.doc.phases.lines[&o.0].len(), 5);
        // The same cut reached from either side.
        for (at, keys) in [
            (bound - 100, [Key::PageDown, Key::PageDown]),
            (bound + 100, [Key::PageUp, Key::PageUp]),
        ] {
            go_to(&mut drv, &mut win, &mut net, at);
            run(&mut drv, &mut win, &mut net, "settled", settled);
            assert_eq!(check_rows(&win, &w_o, &format!("{at}")), 0);
            for key in keys {
                drv.key(&mut win, key, Modifiers::NONE);
                run(&mut drv, &mut win, &mut net, "paging", settled);
                assert_eq!(check_rows(&win, &w_o, &format!("{at} {key:?}")), 0);
            }
        }
        // Further on, inside the index: read on from the fifth checkpoint.
        let further = bound + 3 * GRID + 50;
        go_to(&mut drv, &mut win, &mut net, further);
        drv.idle(&mut win, 2);
        let read = win.phase_read.as_ref().expect("a checkpoint read");
        assert_eq!((read.line, read.from), (o.0, 5));
        let _ = go.send(());
        run(&mut drv, &mut win, &mut net, "further", settled);
        assert_eq!(check_rows(&win, &w_o, "further"), 0);
        assert_eq!(win.doc.phases.lines[&o.0].len(), 8);
        // Past the cap: marks, and no checkpoint asked for.
        win.phase_hook = None;
        go_to(&mut drv, &mut win, &mut net, HIGHLIGHT_MAX + 3 * GRID + 50);
        run(&mut drv, &mut win, &mut net, "past the cap", settled);
        let rows = win.page.as_ref().unwrap().rows.len();
        assert_eq!(check_rows(&win, &w_o, "past the cap"), rows);
        assert!(win.phase_read.is_none() && win.doc.phases.wanted.is_none());
    });
}

/// MF-1 (review of VM-4): the open line one byte past a grid point — its first piece's cut, REACH
/// minus one past its start, is proven by the line start and falls on the grid point itself (no
/// lead pending): the piece after it is proven too, never blind marks.
#[test]
fn a_piece_after_a_cut_its_line_start_proves_is_not_blind() {
    bounded("the piece after a line-start cut", || {
        let (block, _) = korean_lines(400);
        let mut body = Vec::new();
        lines_to(&mut body, &block, HIGHLIGHT_MAX as usize - 200 * 1024);
        let next = (body.len() as u64 / GRID + 2) * GRID + 1;
        body.extend(std::iter::repeat_n(b'x', (next - 1) as usize - body.len()));
        body.push(b'\n');
        let o = long_line(&mut body, b"a", 200 * 1024, None);
        let to = body.len() + 100 * 1024;
        lines_to(&mut body, &block, to);
        let bound = o.0 / GRID * GRID + REACH;
        assert_eq!((o.0 % GRID, bound - o.0), (1, REACH - 1));
        assert!(!Cp949.phase(&body[o.0 as usize..bound as usize]).unwrap());
        let w_o = Whole::of(&body, o.0, o.1);
        let mut net = net(&body, gate());
        let mut win = net.window("open.txt");
        let mut drv = Driver::new(500.0, 200.0);
        run(&mut drv, &mut win, &mut net, "the pass", |w, _| {
            w.doc.size() >= HIGHLIGHT_MAX
        });
        net.settle();
        drv.idle(&mut win, 3);
        let settled = |w: &TextWindow, _: &Driver| w.ready() && w.jump.is_none() && has_rows(w);
        for at in [bound + 100, bound - 100] {
            win.jump_to(Jump::Byte(at));
            run(&mut drv, &mut win, &mut net, "the jump", settled);
            assert_eq!(check_rows(&win, &w_o, &format!("{at}")), 0);
            for key in [Key::PageDown, Key::PageDown, Key::PageUp] {
                drv.key(&mut win, key, Modifiers::NONE);
                run(&mut drv, &mut win, &mut net, "paging", settled);
                assert_eq!(check_rows(&win, &w_o, &format!("{at} {key:?}")), 0);
            }
        }
    });
}
