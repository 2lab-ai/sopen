//! The buffer (12 §7.1, §7.3): §14's EB-1, EB-5's model half (the caret and the block are the
//! caller's, so they travel here as an opaque state), the CR LF guard, the line index by each
//! encoding's LF unit, and random edits, undos and redos held to a plain `Vec<u8>`. Fixtures are
//! generated (12 §15); the random runs use a small stride and small add blocks, so every edit
//! crosses their boundaries.

use super::*;
use crate::encoding::Encoding::*;
use std::time::{Duration, Instant};

/// The caret and the block as an undo step gives them back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sel {
    caret: u64,
    block: Option<(u64, u64)>,
}

fn sel(caret: u64) -> Sel {
    Sel { caret, block: None }
}

fn open(bytes: &[u8], encoding: Encoding) -> Buffer<Sel> {
    Buffer::new(Original::new(bytes.to_vec(), encoding))
}

fn bytes_of(b: &Buffer<Sel>) -> Vec<u8> {
    let mut out = Vec::new();
    let n = b.write_to(&mut out).unwrap();
    assert_eq!(n, b.len());
    out
}

fn utf16(s: &str, be: bool) -> Vec<u8> {
    s.encode_utf16()
        .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
        .collect()
}

fn utf32(s: &str, be: bool) -> Vec<u8> {
    s.chars()
        .flat_map(|c| {
            let u = u32::from(c);
            if be {
                u.to_be_bytes()
            } else {
                u.to_le_bytes()
            }
        })
        .collect()
}

/// `text` in a 1-, 2- or 4-byte encoding (ASCII only, as the fixtures here need).
fn enc(encoding: Encoding, text: &str) -> Vec<u8> {
    match encoding {
        Utf16Le | Ucs2Le => utf16(text, false),
        Utf16Be | Ucs2Be => utf16(text, true),
        Utf32Le => utf32(text, false),
        Utf32Be => utf32(text, true),
        _ => text.as_bytes().to_vec(),
    }
}

/// Types `text` one key at a time, each key its own edit as the editor makes it.
fn type_text(b: &mut Buffer<Sel>, at: u64, text: &[u8], unit: usize) -> u64 {
    let mut at = at;
    for key in text.chunks(unit) {
        let mut e = b.edit(Group::Typing, sel(at));
        at = e.insert(at, key).end;
        e.end(sel(at));
    }
    at
}

/// The offset after the `n`-th LF unit of `doc` (0 for line 0): the oracle is the encoding's own
/// `find_lf` from byte 0.
fn oracle_line_start(doc: &[u8], encoding: Encoding, n: u64) -> Option<u64> {
    let mut at = 0;
    for _ in 0..n {
        let i = encoding.find_lf(&doc[at..], at as u64)?;
        at += i + encoding.lf().len();
    }
    Some(at as u64)
}

fn oracle_lfs(doc: &[u8], encoding: Encoding) -> u64 {
    let mut n = 0;
    while oracle_line_start(doc, encoding, n + 1).is_some() {
        n += 1;
    }
    n
}

/// §7.2 / E35: a boundary strictly between a CR unit and an LF unit, moved past the LF.
fn oracle_whole(doc: &[u8], encoding: Encoding, at: usize) -> usize {
    let u = encoding.unit();
    let pair = at >= u
        && at + u <= doc.len()
        && at.is_multiple_of(u)
        && doc[at - u..at] == *encoding.cr()
        && doc[at..at + u] == *encoding.lf();
    if pair {
        at + u
    } else {
        at
    }
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

#[test]
fn eb1_seven_fixtures_keep_every_byte_outside_the_edit() {
    let mut le_bom = vec![0xFF, 0xFE];
    le_bom.extend(utf16("안녕\r\n세계\r\n끝", false));
    // A lone high surrogate D800, low byte first.
    let mut lone = utf16("a\r\n", false);
    lone.extend([0x00, 0xD8]);
    lone.extend(utf16("b\r\nc", false));
    let fixtures: [(&str, Encoding, Vec<u8>); 7] = [
        ("UTF-8 with FF", Utf8, b"one\n\xFFtwo\nthree\n".to_vec()),
        (
            "UTF-8 CRLF, no final LF",
            Utf8,
            b"one\r\ntwo\r\nthree".to_vec(),
        ),
        ("UTF-8 with a BOM", Utf8, b"\xEF\xBB\xBFone\ntwo\n".to_vec()),
        ("UTF-16LE, BOM, Hangul, CRLF", Utf16Le, le_bom),
        ("UTF-16LE with a lone D800", Utf16Le, lone),
        // 똠 is 8C 63 in CP949 (a UHC syllable whose trail byte is ASCII `c`).
        ("CP949 똠", Cp949, b"\x8C\x63 one\n\x8C\x63two\n".to_vec()),
        ("CP949 lead C8 before LF", Cp949, b"one\xC8\ntwo\n".to_vec()),
    ];
    for (name, encoding, file) in fixtures {
        // `Z`, hand-encoded (the encode side is ED-enc's).
        let z = enc(encoding, "Z");
        let mut b = Buffer::<Sel>::new(Original::new(file.clone(), encoding));
        let lf = encoding.find_lf(&file, 0).unwrap() + encoding.lf().len();
        let at = b.line_start(1).unwrap();
        assert_eq!(
            at, lf as u64,
            "{name}: line 2 starts after line 1's LF unit"
        );
        type_text(&mut b, at, &z, z.len());
        let mut want = file[..lf].to_vec();
        want.extend(&z);
        want.extend(&file[lf..]);
        assert_eq!(bytes_of(&b), want, "{name}");
    }
}

#[test]
fn the_original_is_held_where_it_was_read_never_copied() {
    let file = b"one\ntwo\n".to_vec();
    let p = file.as_ptr();
    let original = Original::new(file, Utf8);
    assert_eq!(original.bytes().as_ptr(), p);
    let b = Buffer::<Sel>::new(original.clone());
    assert!(b.original().same(&original));
    assert_eq!(b.original().bytes().as_ptr(), p);
}

#[test]
fn serialising_writes_each_piece_as_it_is() {
    struct Writes(Vec<usize>);
    impl Write for Writes {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.push(buf.len());
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut b = open(b"alpha\nbeta\ngamma\n", Utf8);
    type_text(&mut b, 6, b"XY", 1);
    type_text(&mut b, 0, b"Q", 1);
    let mut w = Writes(Vec::new());
    assert_eq!(b.write_to(&mut w).unwrap(), b.len());
    let lens: Vec<usize> = b.pieces().iter().map(|p| p.len() as usize).collect();
    assert_eq!(w.0, lens, "one write per piece, nothing gathered first");
    assert_eq!(bytes_of(&b), b"Qalpha\nXYbeta\ngamma\n");
}

#[test]
fn lines_end_at_the_encodings_lf_unit_only() {
    // 갊 is `0a ac` in UTF-16LE and `ac 0a` in UTF-16BE: a 0x0A byte that is no LF.
    for (encoding, text) in [
        (Utf16Le, utf16("갊\n갊갊\n", false)),
        (Utf16Be, utf16("갊\n갊갊\n", true)),
        (Utf32Le, utf32("갊\n\u{a00}\n", false)),
        (Utf32Be, utf32("갊\n\u{a00}\n", true)),
        (Utf8, "갊\n갊\n".as_bytes().to_vec()),
    ] {
        let b = open(&text, encoding);
        assert_eq!(b.lines(), 3, "{encoding:?}");
        for n in 0..4 {
            assert_eq!(
                b.line_start(n),
                oracle_line_start(&text, encoding, n),
                "{encoding:?} line {n}"
            );
        }
        let u = encoding.unit();
        for x in (0..=text.len()).step_by(u) {
            let want = oracle_lfs(&text[..x], encoding);
            assert_eq!(b.line_of(x as u64), want, "{encoding:?} at {x}");
        }
    }
}

/// At the product's sizes (4 KiB strides, 64 KiB add blocks): LF units at and across stride
/// edges, a paste larger than a block, and a delete across strides, held to the oracle.
#[test]
fn lines_at_the_products_stride_and_block_sizes() {
    for encoding in [Utf8, Utf16Le, Utf32Be] {
        let u = encoding.unit();
        let mut rng = Rng(0x5712_1DE0 + u as u64);
        let mut text = |n: usize| -> Vec<u8> {
            let mut s = String::new();
            while s.len() < n {
                s.push(if rng.below(9) == 0 { '\n' } else { 'k' });
            }
            enc(encoding, &s)
        };
        let mut file = text(3 * STRIDE / u + 300);
        // LF units on both sides of each stride edge.
        for edge in [STRIDE, 2 * STRIDE, 3 * STRIDE] {
            for at in [edge - u, edge] {
                file[at..at + u].copy_from_slice(encoding.lf());
            }
        }
        let mut b = open(&file, encoding);
        let mut doc = file.clone();
        let paste = text(BLOCK + 5000);
        let small = text(2 * STRIDE);
        let mut e = b.edit(Group::Single, sel(0));
        let at = (STRIDE + 2 * u) as u64;
        e.insert(at, &paste);
        e.insert(u as u64, &small);
        let cut = 3 * STRIDE..BLOCK + STRIDE / 2;
        let gone = e.delete(cut.start as u64..cut.end as u64);
        e.end(sel(0));
        assert_eq!(gone, cut.start as u64..cut.end as u64);
        doc.splice(at as usize..at as usize, paste.iter().copied());
        doc.splice(u..u, small.iter().copied());
        doc.drain(cut);
        assert_eq!(bytes_of(&b), doc, "{encoding:?}");
        assert!(
            b.add.blocks.len() >= 2,
            "{encoding:?}: the paste took a block of its own"
        );
        let lfs = oracle_lfs(&doc, encoding);
        assert_eq!(b.lines(), lfs + 1, "{encoding:?}");
        for n in 0..=lfs {
            let start = oracle_line_start(&doc, encoding, n).unwrap();
            assert_eq!(b.line_start(n), Some(start), "{encoding:?} line {n}");
            assert_eq!(b.line_of(start), n, "{encoding:?} line {n}");
            if n > 0 {
                assert_eq!(b.line_of(start - u as u64), n - 1, "{encoding:?} line {n}");
            }
        }
        assert_eq!(b.line_of(doc.len() as u64), lfs, "{encoding:?}");
    }
}

#[test]
fn e35_a_boundary_between_cr_and_lf_moves_past_the_lf() {
    for encoding in [Utf8, Utf16Le, Utf16Be, Utf32Le] {
        let u = encoding.unit() as u64;
        let doc = enc(encoding, "a\r\nb");
        let cut = |range: Range<u64>| {
            let mut b = open(&doc, encoding);
            let mut e = b.edit(Group::Single, sel(range.start));
            let got = e.delete(range);
            e.end(sel(got.start));
            (got, bytes_of(&b))
        };
        // A block of `a` alone, cut: CR LF `b` stays (a boundary before the CR does not move).
        assert_eq!(cut(0..u), (0..u, enc(encoding, "\r\nb")), "{encoding:?}");
        // A block whose end is between the CR and the LF: its end after the LF.
        assert_eq!(
            cut(0..2 * u),
            (0..3 * u, enc(encoding, "b")),
            "{encoding:?}"
        );
        // Only the CR: both go.
        assert_eq!(
            cut(u..2 * u),
            (u..3 * u, enc(encoding, "ab")),
            "{encoding:?}"
        );
        // Only the LF: its start moves past it, so nothing is cut.
        assert_eq!(
            cut(2 * u..3 * u),
            (3 * u..3 * u, doc.clone()),
            "{encoding:?}"
        );
        // An insert between them lands after the LF.
        let mut b = open(&doc, encoding);
        let x = enc(encoding, "X");
        let mut e = b.edit(Group::Single, sel(2 * u));
        let got = e.insert(2 * u, &x);
        e.end(sel(got.end));
        assert_eq!(got, 3 * u..4 * u, "{encoding:?}");
        assert_eq!(bytes_of(&b), enc(encoding, "a\r\nXb"), "{encoding:?}");
    }
}

#[test]
fn e35_the_pair_is_found_across_pieces() {
    // The CR ends one piece and the LF starts the next.
    let mut b = open(b"a\nb", Utf8);
    type_text(&mut b, 1, b"\r", 1);
    assert_eq!(bytes_of(&b), b"a\r\nb");
    assert!(b.pieces().len() >= 3);
    let mut e = b.edit(Group::Single, sel(2));
    assert_eq!(e.insert(2, b"X"), 3..4);
    e.end(sel(4));
    assert_eq!(bytes_of(&b), b"a\r\nXb");
}

#[test]
fn undo_puts_back_what_a_delete_joined_into_a_pair() {
    // `a` CR `X` LF `b`: deleting X makes a CR LF pair; undo puts X back between them, where
    // a guarded insert would have put it after the LF.
    let mut b = open(b"a\rX\nb", Utf8);
    let mut e = b.edit(Group::Single, sel(3));
    assert_eq!(e.delete(2..3), 2..3);
    e.end(sel(2));
    assert_eq!(bytes_of(&b), b"a\r\nb");
    assert_eq!(b.undo(), Some(sel(3)));
    assert_eq!(bytes_of(&b), b"a\rX\nb");
    assert_eq!(b.redo(), Some(sel(2)));
    assert_eq!(bytes_of(&b), b"a\r\nb");
}

#[test]
fn eb5_a_typed_word_a_backspace_run_a_paste_and_a_block_move_are_four_steps() {
    let file = b"alpha beta\r\ngamma\r\n".to_vec();
    let mut b = open(&file, Utf8);
    let mut snapshots = vec![bytes_of(&b)];
    let mut sels = Vec::new();

    // A typed word at the start of line 2.
    let start = b.line_start(1).unwrap();
    let end = type_text(&mut b, start, b"new ", 1);
    sels.push((sel(start), sel(end)));
    snapshots.push(bytes_of(&b));

    // A Backspace run: `eta` of `beta`.
    let mut caret = 10;
    for _ in 0..3 {
        let mut e = b.edit(Group::Backspace, sel(caret));
        caret = e.delete(caret - 1..caret).start;
        e.end(sel(caret));
    }
    sels.push((sel(10), sel(7)));
    snapshots.push(bytes_of(&b));
    assert_eq!(snapshots.last().unwrap(), b"alpha b\r\nnew gamma\r\n");

    // A paste with a line break, one step.
    let mut e = b.edit(Group::Single, sel(0));
    let got = e.insert(0, b"x\r\ny ");
    e.end(sel(got.end));
    sels.push((sel(0), sel(got.end)));
    snapshots.push(bytes_of(&b));

    // Ctrl+K V: the block `new ` moved to the file's end, as pieces.
    let from = 14..18;
    assert_eq!(&snapshots.last().unwrap()[14..18], b"new ");
    let before = Sel {
        caret: b.len(),
        block: Some((from.start, from.end)),
    };
    let mut e = b.edit(Group::Single, before.clone());
    let pieces = e.pieces_in(from.clone());
    let gone = e.delete(from.clone());
    let to = e.len();
    let landed = e.insert_pieces(to, &pieces).unwrap();
    let after = Sel {
        caret: landed.start,
        block: Some((landed.start, landed.end)),
    };
    e.end(after.clone());
    assert_eq!(gone, from);
    sels.push((before, after));
    snapshots.push(bytes_of(&b));
    assert_eq!(
        snapshots.last().unwrap(),
        b"x\r\ny alpha b\r\ngamma\r\nnew "
    );

    assert_eq!(b.steps(), 4);
    for i in (0..4).rev() {
        assert_eq!(b.undo(), Some(sels[i].0.clone()), "undo {i}");
        assert_eq!(bytes_of(&b), snapshots[i], "undo {i}");
    }
    assert_eq!(b.undo(), None);
    assert!(b.is_saved());
    for i in 0..4 {
        assert_eq!(b.redo(), Some(sels[i].1.clone()), "redo {i}");
        assert_eq!(bytes_of(&b), snapshots[i + 1], "redo {i}");
    }
    assert_eq!(b.redo(), None);
}

#[test]
fn eb5_100001_single_steps_the_first_is_gone() {
    let mut b = open(b"", Utf8);
    let mut e = b.edit(Group::Single, sel(0));
    e.insert(0, b"a");
    e.end(sel(1));
    // Saved after the first step: its mark outlives the step itself.
    b.mark_saved();
    for i in 1..=UNDO_STEPS as u64 {
        let mut e = b.edit(Group::Single, sel(i));
        e.insert(i, b"b");
        e.end(sel(i + 1));
    }
    assert_eq!(b.steps(), UNDO_STEPS);
    assert!(!b.is_saved());
    for _ in 0..UNDO_STEPS {
        assert!(b.undo().is_some());
    }
    assert_eq!(b.undo(), None, "the first step is gone");
    assert_eq!(bytes_of(&b), b"a");
    assert!(
        b.is_saved(),
        "the state the first step left is the saved one"
    );
    for _ in 0..UNDO_STEPS {
        assert!(b.redo().is_some());
    }
    assert_eq!(b.len(), UNDO_STEPS as u64 + 1);
}

#[test]
fn eb5_undo_past_a_save_and_redo_back_copy_no_buffer() {
    let file = b"one\ntwo\n".to_vec();
    let p = file.as_ptr();
    let mut b = Buffer::<Sel>::new(Original::new(file, Utf8));
    type_text(&mut b, 4, b"new ", 1);
    let mut e = b.edit(Group::Single, sel(0));
    e.delete(0..4);
    e.end(sel(0));
    b.mark_saved();
    assert!(b.is_saved());
    let saved = bytes_of(&b);
    let blocks: Vec<*const u8> = b.add.blocks.iter().map(|k| k.bytes.as_ptr()).collect();
    let add = b.add_len();

    assert_eq!(b.undo(), Some(sel(0)));
    assert!(!b.is_saved(), "undo past a Save: unsaved again");
    assert_eq!(b.redo(), Some(sel(0)));
    assert!(b.is_saved(), "redo: the saved state again");
    assert_eq!(bytes_of(&b), saved);
    // Undo to the file as read, then back: the same buffers throughout.
    while b.undo().is_some() {}
    assert_eq!(bytes_of(&b), b"one\ntwo\n");
    while b.redo().is_some() {}
    assert_eq!(bytes_of(&b), saved);
    assert!(b.is_saved());
    assert_eq!(b.original().bytes().as_ptr(), p);
    let now: Vec<*const u8> = b.add.blocks.iter().map(|k| k.bytes.as_ptr()).collect();
    assert_eq!(now, blocks);
    assert_eq!(b.add_len(), add, "undo and redo append nothing");
}

#[test]
fn the_saved_mark_dies_with_the_redo_branch_that_held_it() {
    let mut b = open(b"x", Utf8);
    type_text(&mut b, 1, b"a", 1);
    b.mark_saved();
    b.undo();
    assert!(!b.is_saved());
    // A new edit drops the redo branch: the saved state can no longer be reached.
    type_text(&mut b, 1, b"a", 1);
    assert_eq!(bytes_of(&b), b"xa");
    assert!(!b.is_saved(), "same bytes, but not the saved step");
    b.undo();
    assert!(!b.is_saved());
}

#[test]
fn a_save_ends_a_typing_run() {
    let mut b = open(b"", Utf8);
    type_text(&mut b, 0, b"ab", 1);
    assert_eq!(b.steps(), 1);
    b.mark_saved();
    type_text(&mut b, 2, b"c", 1);
    assert_eq!(b.steps(), 2);
    assert_eq!(b.undo(), Some(sel(2)));
    assert!(b.is_saved());
}

#[test]
fn runs_join_only_while_they_are_unbroken() {
    let mut b = open(b"0123456789", Utf8);
    // Typing at two places: two steps.
    type_text(&mut b, 2, b"ab", 1);
    type_text(&mut b, 8, b"c", 1);
    assert_eq!(b.steps(), 2);
    // A sealed run does not join.
    b.seal();
    type_text(&mut b, 9, b"d", 1);
    assert_eq!(b.steps(), 3);
    // Del at one place joins; Backspace after it does not.
    for _ in 0..2 {
        let mut e = b.edit(Group::Delete, sel(0));
        e.delete(0..1);
        e.end(sel(0));
    }
    assert_eq!(b.steps(), 4);
    let mut e = b.edit(Group::Backspace, sel(3));
    e.delete(2..3);
    e.end(sel(2));
    assert_eq!(b.steps(), 5);
    // Overwrite typing (delete then insert at the caret) is one run.
    for (i, c) in [b"X", b"Y"].into_iter().enumerate() {
        let at = 4 + i as u64;
        let mut e = b.edit(Group::Typing, sel(at));
        e.delete(at..at + 1);
        e.insert(at, c);
        e.end(sel(at + 1));
    }
    assert_eq!(b.steps(), 6);
    let undone = bytes_of(&b);
    assert_eq!(b.undo(), Some(sel(4)));
    b.redo();
    assert_eq!(bytes_of(&b), undone);
}

#[test]
fn a_reread_starts_over_on_a_new_original() {
    let mut b = open(b"one\ntwo\n", Utf8);
    type_text(&mut b, 0, b"Z", 1);
    let old = b.pieces_in(0..b.len());
    let typed = b.pieces_in(0..1);
    b.reread(Original::new(b"three\n".to_vec(), Utf8));
    assert_eq!(b.generation(), 1);
    assert_eq!(bytes_of(&b), b"three\n");
    assert_eq!(b.steps(), 0);
    assert!(b.is_saved());
    assert_eq!(b.undo(), None);
    assert!(matches!(
        b.span(&b.pieces()[0]),
        Span::Original { k: 1, .. }
    ));
    // The old original's pieces belong to no buffer now; typed text is still the session's.
    let mut e = b.edit(Group::Single, sel(0));
    assert_eq!(e.insert_pieces(0, &old), Err(Stale));
    assert_eq!(e.insert_pieces(0, &typed), Ok(0..1));
    e.end(sel(1));
    assert_eq!(bytes_of(&b), b"Zthree\n");
}

#[test]
fn eb5_a_large_delete_adds_only_its_pieces() {
    let file = vec![b'x'; 8 << 20];
    let mut b = open(&file, Utf8);
    let mut e = b.edit(
        Group::Single,
        Sel {
            caret: 1,
            block: Some((1, file.len() as u64 - 1)),
        },
    );
    e.delete(1..file.len() as u64 - 1);
    e.end(sel(1));
    assert_eq!(b.len(), 2);
    assert_eq!(b.add_len(), 0);
    let step = b.undo.back().unwrap();
    let held: usize = step
        .changes
        .iter()
        .map(|c| c.removed.len() + c.inserted.len())
        .sum();
    assert_eq!(held, 1, "one piece of the original, no byte");
    b.undo();
    assert_eq!(b.len(), file.len() as u64);
    assert_eq!(b.pieces().len(), 1, "the split pieces merged back");
}

/// Random inserts, deletes, block moves, undos, redos and Saves against a plain `Vec<u8>`, in
/// every unit size, with the alphabet weighted to CR, LF and units holding a 0x0A byte that is
/// no LF.
#[test]
fn random_edits_agree_with_a_plain_vec() {
    for (encoding, seed) in [
        (Utf8, 1),
        (Latin1, 2),
        (Utf16Le, 3),
        (Utf16Be, 4),
        (Utf32Le, 5),
        (Utf32Be, 6),
    ] {
        for run in 0..120 {
            random_run(encoding, seed * 1000 + run);
        }
    }
}

/// One step of the model: the document before it and the states the step gives back.
struct Kept {
    doc: Vec<u8>,
    before: Sel,
    after: Sel,
}

fn random_run(encoding: Encoding, seed: u64) {
    let u = encoding.unit();
    let units: Vec<Vec<u8>> = [
        encoding.lf().to_vec(),
        encoding.cr().to_vec(),
        enc(encoding, "a"),
        enc(encoding, "\r\n"),
        match (u, encoding) {
            (1, _) => vec![0xFF],
            (2, Utf16Le) => vec![0x0A, 0xAC],
            (2, _) => vec![0xAC, 0x0A],
            (_, Utf32Le) => vec![0x0A, 0xAC, 0, 0],
            _ => vec![0, 0, 0xAC, 0x0A],
        },
        match u {
            1 => vec![b'\n'],
            2 => vec![0x0A, 0x0A],
            _ => vec![0x0A, 0x0A, 0x0A, 0],
        },
    ]
    .to_vec();
    let mut rng = Rng(seed);
    let text = |rng: &mut Rng, max: usize| -> Vec<u8> {
        let n = 1 + rng.below(max);
        (0..n)
            .flat_map(|_| units[rng.below(units.len())].clone())
            .collect()
    };
    let file = text(&mut rng, 24);
    let stride = [4, 8, 12][rng.below(3)];
    let block = [4, 8, 16][rng.below(3)];
    let mut b = Buffer::<Sel>::with_block(Original::strided(file.clone(), encoding, stride), block);
    let mut doc = file.clone();
    let mut undo: Vec<Kept> = Vec::new();
    let mut redo: Vec<Kept> = Vec::new();
    let mut saved = Some(file);
    let aligned = |rng: &mut Rng, len: usize| (rng.below(len / u + 1) * u) as u64;

    for op in 0..60u64 {
        let (before, after) = (sel(2 * op), sel(2 * op + 1));
        let steps = b.steps();
        let was = doc.clone();
        let mut edited = true;
        match rng.below(10) {
            0..=3 => {
                let at = aligned(&mut rng, doc.len());
                let bytes = text(&mut rng, 3);
                let group = [Group::Typing, Group::Single][rng.below(2)];
                let mut e = b.edit(group, before.clone());
                let got = e.insert(at, &bytes);
                e.end(after.clone());
                let w = oracle_whole(&doc, encoding, at as usize);
                doc.splice(w..w, bytes.iter().copied());
                assert_eq!(
                    got,
                    w as u64..(w + bytes.len()) as u64,
                    "seed {seed} op {op}"
                );
            }
            4..=5 => {
                let (x, y) = (aligned(&mut rng, doc.len()), aligned(&mut rng, doc.len()));
                let group = [Group::Backspace, Group::Delete, Group::Single][rng.below(3)];
                let mut e = b.edit(group, before.clone());
                let got = e.delete(x.min(y)..x.max(y));
                e.end(after.clone());
                let s = oracle_whole(&doc, encoding, x.min(y) as usize);
                let t = oracle_whole(&doc, encoding, x.max(y) as usize);
                let want = if s < t { s..t } else { s..s };
                doc.drain(want.clone());
                assert_eq!(
                    got,
                    want.start as u64..want.end as u64,
                    "seed {seed} op {op}"
                );
            }
            6 => {
                // A block move: its pieces deleted, then inserted elsewhere, one step.
                let (x, y) = (aligned(&mut rng, doc.len()), aligned(&mut rng, doc.len()));
                let range = x.min(y)..x.max(y);
                let mut e = b.edit(Group::Single, before.clone());
                let pieces = e.pieces_in(range.clone());
                let gone = e.delete(range.clone());
                let at = aligned(&mut rng, e.len() as usize);
                let got = e.insert_pieces(at, &pieces).unwrap();
                e.end(after.clone());
                let moved = doc[range.start as usize..range.end as usize].to_vec();
                let s = oracle_whole(&doc, encoding, range.start as usize);
                let t = oracle_whole(&doc, encoding, range.end as usize);
                if s < t {
                    doc.drain(s..t);
                }
                assert_eq!(gone.start, s as u64, "seed {seed} op {op}");
                let w = oracle_whole(&doc, encoding, at as usize);
                doc.splice(w..w, moved.iter().copied());
                assert_eq!(
                    got,
                    w as u64..(w + moved.len()) as u64,
                    "seed {seed} op {op}"
                );
            }
            7 => {
                edited = false;
                let got = b.undo();
                match undo.pop() {
                    Some(k) => {
                        assert_eq!(got, Some(k.before.clone()), "seed {seed} op {op}");
                        redo.push(Kept {
                            doc: std::mem::replace(&mut doc, k.doc),
                            ..k
                        });
                    }
                    None => assert_eq!(got, None, "seed {seed} op {op}"),
                }
            }
            8 => {
                edited = false;
                let got = b.redo();
                match redo.pop() {
                    Some(k) => {
                        assert_eq!(got, Some(k.after.clone()), "seed {seed} op {op}");
                        undo.push(Kept {
                            doc: std::mem::replace(&mut doc, k.doc),
                            ..k
                        });
                    }
                    None => assert_eq!(got, None, "seed {seed} op {op}"),
                }
            }
            _ => {
                edited = false;
                if rng.below(2) == 0 {
                    b.seal();
                } else {
                    b.mark_saved();
                    saved = Some(doc.clone());
                }
            }
        }
        // The model's steps follow the buffer's grouping: a new step holds the document before
        // it, a joined one keeps its own and takes the new after state.
        if edited && b.steps() > steps {
            undo.push(Kept {
                doc: was,
                before,
                after,
            });
            redo.clear();
        } else if edited && doc != was {
            undo.last_mut().expect("a joined step").after = after;
        }
        assert_eq!(b.steps(), undo.len(), "seed {seed} op {op}");
        assert_eq!(bytes_of(&b), doc, "seed {seed} op {op}");
        if b.is_saved() {
            assert_eq!(Some(&doc), saved.as_ref(), "seed {seed} op {op}: saved");
        }
        let lfs = oracle_lfs(&doc, encoding);
        assert_eq!(b.lines(), lfs + 1, "seed {seed} op {op}");
        for n in 0..=lfs + 1 {
            assert_eq!(
                b.line_start(n),
                oracle_line_start(&doc, encoding, n),
                "seed {seed} op {op} line {n}"
            );
        }
        for x in (0..=doc.len()).step_by(u) {
            assert_eq!(
                b.line_of(x as u64),
                oracle_lfs(&doc[..x], encoding),
                "seed {seed} op {op} at {x}"
            );
        }
        let mut read = Vec::new();
        let (x, y) = (aligned(&mut rng, doc.len()), aligned(&mut rng, doc.len()));
        b.read(x.min(y)..x.max(y), &mut read);
        assert_eq!(read, doc[x.min(y) as usize..x.max(y) as usize]);
        let mut add = Vec::new();
        for c in b.add_chunks(0..b.add_len()) {
            add.extend_from_slice(c);
        }
        assert_eq!(add.len() as u64, b.add_len());
        for p in b.pieces() {
            if let Span::Add(r) = b.span(p) {
                assert!(r.end <= b.add_len());
            }
        }
    }
}

/// The process's peak resident set, in bytes.
fn peak_rss() -> u64 {
    // SAFETY: getrusage fills the struct it is given; a zeroed `rusage` is a valid one.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let v = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        v
    } else {
        v * 1024
    }
}

const MIB: u64 = 1 << 20;

/// 100 MiB of lines 1–80 bytes long (about 41 on average, as *editor* §3.1's file), built in
/// place without a doubling `Vec`.
fn hundred_mib() -> Vec<u8> {
    let n = 100 * MIB as usize;
    let mut file = vec![b'\n'; n];
    let mut rng = Rng(0x0ED_B0F);
    let mut at = 0;
    while at < n {
        let len = (1 + rng.below(80)).min(n - at);
        for (i, b) in file[at..at + len].iter_mut().enumerate() {
            *b = b'a' + ((i + len) % 26) as u8;
        }
        // The byte after the line stays the LF it was filled with.
        at += len + 1;
    }
    file
}

/// EB-1's second item, by hand in a release build, alone in its process:
/// `cargo test --release --lib -- --ignored --exact edit::buffer::tests::eb1_100_mib_within_1_ms_and_1_1x`.
/// *editor* §3.1's prototype measured a process of its own; this one is the test binary, all of
/// sopen resident before the file is made, so the bound holds what the file and the buffer add
/// to it, and the line prints the process's own peak beside it.
#[test]
#[ignore = "100 MiB, timed and measured: run alone in a release build"]
fn eb1_100_mib_within_1_ms_and_1_1x() {
    let start = peak_rss();
    let file = hundred_mib();
    let size = file.len() as u64;
    let opened = Instant::now();
    let mut b = Buffer::<Sel>::new(Original::new(file, Utf8));
    let open_took = opened.elapsed();
    let mut rng = Rng(7);
    let mut worst = Duration::ZERO;
    let mut time = |f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        worst = worst.max(t.elapsed());
    };
    let mid = size / 2;
    for i in 0..1000u64 {
        time(&mut || {
            let mut e = b.edit(Group::Typing, sel(mid + i));
            e.insert(mid + i, b"x");
            e.end(sel(mid + i + 1));
        });
    }
    for _ in 0..10_000 {
        let at = rng.below(b.len() as usize) as u64;
        time(&mut || {
            let mut e = b.edit(Group::Single, sel(at));
            e.insert(at, b"yz");
            e.end(sel(at + 2));
        });
    }
    for _ in 0..10_000 {
        let at = rng.below(b.len() as usize - 1) as u64;
        time(&mut || {
            let mut e = b.edit(Group::Single, sel(at));
            e.delete(at..at + 1);
            e.end(sel(at));
        });
    }
    let lines = b.lines();
    for i in 0..20_000 {
        let line = rng.below(lines as usize) as u64;
        let at = rng.below(b.len() as usize) as u64;
        time(&mut || {
            if i % 2 == 0 {
                assert!(b.line_start(line).is_some());
            } else {
                assert!(b.line_of(at) < lines);
            }
        });
    }
    let peak = peak_rss();
    let added = peak - start;
    let bound = size as f64 * 1.1 + (10 * MIB) as f64;
    let mib = |n: f64| n / MIB as f64;
    eprintln!(
        "EB-1 100 MiB: open {open_took:?}, {} pieces, {} steps, worst operation {worst:?}; \
         peak RSS {:.1} MiB, of which {:.1} MiB before the file and {:.1} MiB added \
         (bound {:.1} MiB)",
        b.pieces().len(),
        b.steps(),
        mib(peak as f64),
        mib(start as f64),
        mib(added as f64),
        mib(bound)
    );
    assert!(
        worst <= Duration::from_millis(1),
        "worst operation {worst:?}"
    );
    assert!(added as f64 <= bound);
}

/// EB-5's last item at its size, by hand in a release build, alone in its process:
/// `cargo test --release --lib -- --ignored --exact edit::buffer::tests::eb5_a_200_mb_delete_adds_only_its_pieces`.
#[test]
#[ignore = "200 MB: run alone in a release build"]
fn eb5_a_200_mb_delete_adds_only_its_pieces() {
    let size = 200_000_000;
    let file = vec![b'x'; size];
    let mut b = Buffer::<Sel>::new(Original::new(file, Utf8));
    let at_open = peak_rss();
    let t = Instant::now();
    let mut e = b.edit(Group::Single, sel(1));
    e.delete(1..size as u64 - 1);
    e.end(sel(1));
    b.undo();
    b.redo();
    let took = t.elapsed();
    let grew = peak_rss() - at_open;
    eprintln!("EB-5 200 MB delete, undo, redo: {took:?}, peak RSS grew {grew} bytes");
    assert_eq!(b.len(), 2);
    assert!(grew < MIB, "the delete copied bytes: {grew}");
}
