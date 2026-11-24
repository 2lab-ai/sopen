//! The encoding core (08 §7; §11 EA-1, EA-2, EA-4, EA-5 and EA-7, the model's part; §12): every
//! ranged read — a grid cut, a restart, a jump, a place not wrapped — is compared with decoding
//! the whole input, exhaustively over small alphabets of the bytes that matter and over random
//! valid and invalid input, and UTF-8 with today's text view, copied below.

use super::*;
use Encoding::*;

const FFFD: char = char::REPLACEMENT_CHARACTER;

// Today's UTF-8 code in the text view (`src/gui/text.rs`, `src/gui/text/wrap.rs` at 05752c4),
// copied verbatim as the oracle the core must equal: it is private to the view. VM-2 moves the
// view onto the core, so a drift would show there as well.

/// `text.rs:1993-2002`.
fn today_cut_char_start(bytes: &[u8]) -> usize {
    let from = bytes.len().saturating_sub(4);
    let Some(i) = (from..bytes.len()).rev().find(|&i| bytes[i] & 0xC0 != 0x80) else {
        return bytes.len();
    };
    match std::str::from_utf8(&bytes[i..]) {
        Err(e) if e.valid_up_to() == 0 && e.error_len().is_none() => i,
        _ => bytes.len(),
    }
}

/// `wrap.rs:52-84`.
fn today_decode(bytes: &[u8], whole: bool) -> (Vec<Cell>, usize) {
    let end = if whole {
        bytes.len()
    } else {
        today_cut_char_start(bytes)
    };
    let mut out = Vec::with_capacity(end);
    let mut rest = &bytes[..end];
    while !rest.is_empty() {
        let (valid, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => {
                let ok = e.valid_up_to();
                let s = std::str::from_utf8(&rest[..ok]).unwrap_or_default();
                (s, e.error_len().unwrap_or(rest.len() - ok))
            }
        };
        out.extend(valid.chars().map(|ch| Cell {
            ch,
            len: ch.len_utf8() as u8,
        }));
        rest = &rest[valid.len()..];
        if bad > 0 {
            out.push(Cell {
                ch: char::REPLACEMENT_CHARACTER,
                len: bad as u8,
            });
            rest = &rest[bad..];
        }
    }
    (out, end)
}

/// `text.rs:1315-1319`, over a file: the cut at `g` of a piece that starts at `lo` (four bytes
/// read, where the core reads three).
fn today_grid_cut(file: &[u8], g: usize, lo: usize) -> usize {
    let from = g.saturating_sub(4).max(lo);
    from + today_cut_char_start(&file[from..g])
}

/// `wrap.rs:615-620` and `648-652` (a restart), `870-878` and `1026-1033` (a place not wrapped,
/// past the line's first byte): the continuation bytes skipped from `x`.
fn today_skip(file: &[u8], x: usize) -> usize {
    file[x..]
        .iter()
        .take(3)
        .take_while(|&&b| b & 0xC0 == 0x80)
        .count()
}

/// A small deterministic generator (splitmix64).
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

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }
}

/// Every string of up to `max` symbols of `alphabet`.
fn strings<T: Copy>(alphabet: &[T], max: usize) -> Vec<Vec<T>> {
    let mut out = vec![Vec::new()];
    let mut last = vec![Vec::new()];
    for _ in 0..max {
        let next: Vec<Vec<T>> = last
            .iter()
            .flat_map(|s: &Vec<T>| {
                alphabet.iter().map(move |&a| {
                    let mut t = s.clone();
                    t.push(a);
                    t
                })
            })
            .collect();
        out.extend(next.iter().cloned());
        last = next;
    }
    out
}

/// `units` as the bytes of a 2- or 4-byte encoding, in its order.
fn unit_bytes(e: Encoding, units: &[u32]) -> Vec<u8> {
    let be = e.big_endian();
    units
        .iter()
        .flat_map(|&u| match (e.unit(), be) {
            (2, false) => (u as u16).to_le_bytes().to_vec(),
            (2, true) => (u as u16).to_be_bytes().to_vec(),
            (_, false) => u.to_le_bytes().to_vec(),
            (_, true) => u.to_be_bytes().to_vec(),
        })
        .collect()
}

/// `text` in `e`: UTF-8, UTF-16 / UCS-2 (pairs for what is above U+FFFF), UTF-32, Latin-1.
fn encode(e: Encoding, text: &str) -> Vec<u8> {
    match e.unit() {
        2 => unit_bytes(e, &text.encode_utf16().map(u32::from).collect::<Vec<_>>()),
        4 => unit_bytes(e, &text.chars().map(u32::from).collect::<Vec<_>>()),
        _ if e == Latin1 => text.chars().map(|c| c as u8).collect(),
        _ => text.as_bytes().to_vec(),
    }
}

fn text_of(cells: &[Cell]) -> String {
    cells.iter().map(|c| c.ch).collect()
}

/// The whole-file decode by std where std has one, and by §7.1's table elsewhere: an oracle of
/// the characters the core must give.
fn oracle(e: Encoding, file: &[u8]) -> Vec<char> {
    let be = e.big_endian();
    let partial = |n: usize| (!file.len().is_multiple_of(n)).then_some(FFFD);
    match e {
        Ascii => file
            .iter()
            .map(|&b| if b < 0x80 { char::from(b) } else { FFFD })
            .collect(),
        Utf8 => String::from_utf8_lossy(file).chars().collect(),
        Utf16Le | Utf16Be => {
            let units = file.as_chunks::<2>().0.iter().map(|u| u16_of(u, be));
            char::decode_utf16(units)
                .map(|r| r.unwrap_or(FFFD))
                .chain(partial(2))
                .collect()
        }
        Ucs2Le | Ucs2Be => file
            .as_chunks::<2>()
            .0
            .iter()
            .map(|u| char::from_u32(u32::from(u16_of(u, be))).unwrap_or(FFFD))
            .chain(partial(2))
            .collect(),
        Utf32Le | Utf32Be => file
            .as_chunks::<4>()
            .0
            .iter()
            .map(|u| char::from_u32(u32_of(u, be)).unwrap_or(FFFD))
            .chain(partial(4))
            .collect(),
        Latin1 => file.iter().map(|&b| char::from(b)).collect(),
        Cp437 => file
            .iter()
            .map(|&b| {
                if b < 0x80 {
                    char::from(b)
                } else {
                    chrome::cp437(b)
                }
            })
            .collect(),
        Cp949 => unreachable!("VM-4's"),
    }
}

/// The encodings VM-1 decodes: all but CP949, whose tests are VM-4's (`cp949_tests.rs`).
const VM1: [Encoding; 10] = [
    Ascii, Utf8, Utf16Le, Utf16Be, Ucs2Le, Ucs2Be, Utf32Le, Utf32Be, Latin1, Cp437,
];

/// UTF-8 by std's own `Utf8Chunks`, an oracle of the lengths: each valid character its bytes,
/// each invalid sequence one U+FFFD as long as std says it is.
fn utf8_chunks(file: &[u8]) -> Vec<Cell> {
    let mut out = Vec::new();
    for chunk in file.utf8_chunks() {
        out.extend(chunk.valid().chars().map(|ch| Cell {
            ch,
            len: ch.len_utf8() as u8,
        }));
        if !chunk.invalid().is_empty() {
            out.push(Cell {
                ch: FFFD,
                len: chunk.invalid().len() as u8,
            });
        }
    }
    out
}

/// The start of every character of `cells`, then their end.
fn starts(cells: &[Cell]) -> Vec<usize> {
    let mut out = Vec::with_capacity(cells.len() + 1);
    let mut at = 0;
    for c in cells {
        out.push(at);
        at += usize::from(c.len);
    }
    out.push(at);
    out
}

/// Every rule of §7.3 over `file`, one line from byte 0, held to its whole decode: from each
/// start in `los` (character starts the model knows, as a line's start), every piece's end, grid
/// point, restart window, `left` and jump — each read with exactly the bytes the core asks for
/// (`look_back` before the place, `AHEAD` after), and again anchored with the whole line.
fn check_rules(e: Encoding, file: &[u8], los: &[usize]) {
    let n = file.len();
    let unit = e.unit();
    let lb = e.look_back();
    let all = e.decode(file, true).0;
    let ctx = || format!("{e:?} {file:02x?}");
    assert_eq!(starts(&all)[all.len()], n, "{}", ctx());
    assert_eq!(text_of(&all).chars().collect::<Vec<_>>(), oracle(e, file));
    let bounds = starts(&all);
    let index = |x: usize| bounds.binary_search(&x).ok();
    let from = |x: usize| &all[index(x).expect("a character start")..];
    if e == Utf8 {
        assert_eq!(e.decode(file, true), today_decode(file, true), "{}", ctx());
        assert_eq!(all, utf8_chunks(file), "{}", ctx());
    }
    for &lo in los {
        let line = &file[lo..];
        let ctx = || format!("{e:?} {file:02x?} from {lo}");
        assert!(index(lo).is_some(), "{}", ctx());
        assert_eq!(e.decode(line, true).0, from(lo), "{}", ctx());
        let near_at = |x: usize, ahead: usize| {
            let start = x.saturating_sub(lb).max(lo);
            let end = (x + ahead).min(n);
            Around {
                at: start as u64,
                bytes: &file[start..end.max(start)],
                anchored: start == lo,
            }
        };
        let whole_line = Around::anchored(lo as u64, line);

        // A piece not `whole` leaves a cut character for the next one.
        for k in lo..=n {
            let (cells, end) = e.decode(&file[lo..k], false);
            let end = lo + end;
            let ctx = || format!("{} piece to {k}", ctx());
            assert!(end <= k && index(end).is_some(), "{}", ctx());
            assert_eq!(
                cells,
                all[index(lo).unwrap()..index(end).unwrap()],
                "{}",
                ctx()
            );
            if e == Utf8 {
                let piece = &file[lo..k];
                assert_eq!(e.decode(piece, false), today_decode(piece, false));
                assert_eq!(e.decode(piece, true), today_decode(piece, true));
                assert_eq!(e.decode(piece, true).0, utf8_chunks(piece), "{}", ctx());
            }
        }

        // Rule 1: one cut, used by both pieces (D36).
        for g in (lo..=n).filter(|g| g.is_multiple_of(unit)) {
            let ctx = || format!("{} grid point {g}", ctx());
            let c = e.grid_cut(near_at(g, 0), g as u64).unwrap() as usize;
            assert_eq!(
                e.grid_cut(whole_line, g as u64),
                Some(c as u64),
                "{}",
                ctx()
            );
            assert!(lo <= c && c <= g && g - c <= 3, "{}: {c}", ctx());
            assert!(
                c.is_multiple_of(unit) && index(c).is_some(),
                "{}: {c}",
                ctx()
            );
            assert_eq!(lo + e.decode(&file[lo..g], false).1, c, "{}", ctx());
            let mut pieces = e.decode(&file[lo..c], true).0;
            pieces.extend(e.decode(&file[c..], true).0);
            assert_eq!(pieces, from(lo), "{}", ctx());
            if e == Utf8 {
                assert_eq!(c, today_grid_cut(file, g, lo), "{}", ctx());
            }
        }

        // Rules 2 and 4: the first character start at or after a place.
        for x in lo..=n {
            let near = near_at(e.align_down(x as u64) as usize, AHEAD + x % unit);
            let rules = [
                (
                    "restart",
                    e.align_up(x as u64),
                    e.restart_start(near, x as u64),
                ),
                ("left", e.align_down(x as u64), e.left_start(near, x as u64)),
            ];
            for (rule, floor, s) in rules {
                let ctx = || format!("{} {rule} at {x}", ctx());
                let s = s.unwrap();
                let again = match rule {
                    "restart" => e.restart_start(whole_line, x as u64),
                    _ => e.left_start(whole_line, x as u64),
                };
                assert_eq!(again, Some(s), "{}", ctx());
                assert!(
                    s >= floor && s <= floor + 3 && s.is_multiple_of(unit as u64),
                    "{}",
                    ctx()
                );
                let s = (s as usize).min(n);
                assert!(index(s).is_some(), "{}: {s}", ctx());
                assert_eq!(e.decode(&file[s..], true).0, from(s), "{}", ctx());
                // The first character start at or after the rounded place…
                let floor = (floor as usize).min(n);
                let first = bounds[bounds.partition_point(|&b| b < floor)];
                if e == Utf8 {
                    let today = if x == lo { lo } else { x + today_skip(file, x) };
                    assert_eq!(s, today, "{}", ctx());
                    // …but for the stray continuation bytes today's skip passes over, each its
                    // own U+FFFD: never a real character's start.
                    let passed = &all[index(first).unwrap()..index(s).unwrap()];
                    let stray = |(i, c): (usize, &Cell)| {
                        c.ch == FFFD && c.len == 1 && file[first + i] & 0xC0 == 0x80
                    };
                    assert!(passed.iter().enumerate().all(stray), "{}: {s}", ctx());
                } else {
                    assert_eq!(s, first, "{}", ctx());
                }
            }
        }

        // Rule 3: a jump's place picks the character it falls in.
        for p in lo..n {
            let ctx = || format!("{} jump to {p}", ctx());
            let q0 = e.align_down(p as u64) as usize;
            let q = e.jump_place(near_at(q0, AHEAD), p as u64) as usize;
            assert_eq!(e.jump_place(whole_line, p as u64) as usize, q, "{}", ctx());
            assert!(
                lo <= q && q <= p && q.is_multiple_of(unit),
                "{}: {q}",
                ctx()
            );
            if unit == 1 {
                assert_eq!(q, p, "{}", ctx());
            } else {
                let i = index(q).unwrap_or_else(|| panic!("{}: {q}", ctx()));
                assert!(p < q + usize::from(all[i].len), "{}: {q}", ctx());
                assert!(q == q0 || q + 2 == q0, "{}: {q}", ctx());
            }
        }
    }
}

/// A line cut into pieces at every `grid` bytes, each piece starting at the last one's cut,
/// gives the whole decode — "ranged equals whole" (§12).
fn check_chain(e: Encoding, file: &[u8], grid: usize) {
    let lb = e.look_back();
    let (mut start, mut got) = (0, Vec::new());
    let mut g = grid;
    while g < file.len() {
        let from = g.saturating_sub(lb).max(start);
        let near = Around {
            at: from as u64,
            bytes: &file[from..g],
            anchored: from == start,
        };
        let c = e.grid_cut(near, g as u64).unwrap() as usize;
        got.extend(e.decode(&file[start..c], true).0);
        start = c;
        g += grid;
    }
    got.extend(e.decode(&file[start..], true).0);
    assert_eq!(got, e.decode(file, true).0, "{e:?} grid {grid}");
}

/// The LF and CR unit searches against a brute force, read from `at`.
fn check_lf(e: Encoding, bytes: &[u8], at: u64) {
    let n = e.unit();
    let is = |i: usize, unit: &[u8]| {
        (at + i as u64).is_multiple_of(n as u64) && bytes.get(i..i + n) == Some(unit)
    };
    let lfs: Vec<usize> = (0..bytes.len()).filter(|&i| is(i, e.lf())).collect();
    let ctx = || format!("{e:?} at {at} {bytes:02x?}");
    assert_eq!(e.find_lf(bytes, at), lfs.first().copied(), "{}", ctx());
    assert_eq!(e.rfind_lf(bytes, at), lfs.last().copied(), "{}", ctx());
    let cr = bytes.len() >= n && is(bytes.len() - n, e.cr());
    assert_eq!(e.ends_with_cr(bytes, at), cr, "{}", ctx());
}

/// Every character start of a short file as a line start, the rest as places.
fn every_start(e: Encoding, file: &[u8]) -> Vec<usize> {
    let all = e.decode(file, true).0;
    let mut los = starts(&all);
    los.pop();
    los.push(file.len());
    los.retain(|&x| x % e.unit() == 0);
    los
}

// ---------------------------------------------------------------------------------------------
// Ranged equals whole: exhaustive over the bytes and units that matter.

#[test]
fn utf8_ranged_equals_whole_and_today_exhaustively() {
    // ASCII, LF, continuations of each range, a lead of each length and its constraints
    // (E0 > 9F, ED < A0, F0 > 8F, F4 < 90), and bytes that never start a character.
    let alphabet = [
        0x41, 0x0A, 0x80, 0x90, 0xA0, 0xBF, 0xC0, 0xC2, 0xE0, 0xED, 0xF0, 0xF4, 0xF5,
    ];
    for file in strings(&alphabet, 4) {
        check_rules(Utf8, &file, &every_start(Utf8, &file));
    }
    // Longer runs over the leads that take three continuation bytes.
    for file in strings(&[0x41, 0x80, 0x90, 0xE3, 0xF0, 0xF4], 6) {
        check_rules(Utf8, &file, &[0]);
    }
}

#[test]
fn utf16_ranged_equals_whole_exhaustively() {
    // ASCII, LF, CR, units holding a 0x0A byte (U+0A00, 갊 U+AC0A), both ends of each
    // surrogate range; then an odd trailing byte.
    let alphabet = [
        0x0041, 0x000A, 0x000D, 0x0A00, 0xAC0A, 0xD800, 0xDBFF, 0xDC00, 0xDFFF,
    ];
    for e in [Utf16Le, Utf16Be, Ucs2Le, Ucs2Be] {
        for units in strings(&alphabet, 4) {
            let mut file = unit_bytes(e, &units);
            check_rules(e, &file, &every_start(e, &file));
            check_lf(e, &file, 0);
            file.push(0x0A);
            check_rules(e, &file, &every_start(e, &file));
            check_lf(e, &file, 1);
        }
    }
}

#[test]
fn utf32_ranged_equals_whole_exhaustively() {
    // Valid values at the ends, past 10FFFF, a surrogate, a value made of 0x0A bytes; then a
    // partial last unit of one to three bytes.
    let alphabet = [
        0x41, 0x0A, 0x0D, 0x1F600, 0x10FFFF, 0x110000, 0xD800, 0x0A0A0A0A,
    ];
    for e in [Utf32Le, Utf32Be] {
        for units in strings(&alphabet, 3) {
            let file = unit_bytes(e, &units);
            for tail in 0..4 {
                let mut file = file.clone();
                file.extend(std::iter::repeat_n(0x0A, tail));
                check_rules(e, &file, &every_start(e, &file));
                for at in 0..4 {
                    check_lf(e, &file, at);
                }
            }
        }
    }
}

#[test]
fn one_byte_encodings_ranged_equal_whole_over_every_byte() {
    for e in [Ascii, Latin1, Cp437] {
        let every: Vec<u8> = (0..=255).collect();
        check_rules(e, &every[..64], &[0, 7, 63]);
        check_rules(e, &every[120..200], &[0, 1]);
        for b in 0..=255u8 {
            check_rules(e, &[b, b'\n', b], &[0, 1, 2]);
        }
        check_lf(e, b"a\r\nb\n\r", 0);
    }
}

// ---------------------------------------------------------------------------------------------
// Ranged equals whole: random valid and invalid input.

/// A line of mostly valid UTF-8 with bad sequences among it.
fn utf8_mix(rng: &mut Rng, pieces: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..pieces {
        let ch = |rng: &mut Rng, lo: u32, hi: u32| loop {
            if let Some(c) = char::from_u32(lo + rng.below((hi - lo) as usize) as u32) {
                break c;
            }
        };
        let mut buf = [0u8; 4];
        match rng.below(10) {
            0 | 1 => out.push(b'a' + rng.below(26) as u8),
            2 => out.extend_from_slice(b"\r\n"),
            3 => out.extend_from_slice(ch(rng, 0x80, 0x800).encode_utf8(&mut buf).as_bytes()),
            4 => out.extend_from_slice(ch(rng, 0xAC00, 0xD7A4).encode_utf8(&mut buf).as_bytes()),
            5 => out.extend_from_slice(ch(rng, 0x10000, 0x110000).encode_utf8(&mut buf).as_bytes()),
            6 => {
                // A character cut short.
                let c = ch(rng, 0x800, 0x110000).encode_utf8(&mut buf).len();
                let keep = 1 + rng.below(c - 1);
                out.extend_from_slice(&buf[..keep]);
            }
            7 => out.push(0x80 | rng.byte() & 0x3F),
            _ => out.push(rng.byte()),
        }
    }
    out
}

/// Units of a line in a 2-byte encoding: characters, pairs, lone halves, any unit.
fn utf16_mix(rng: &mut Rng, units: usize) -> Vec<u32> {
    let mut out = Vec::new();
    for _ in 0..units {
        match rng.below(10) {
            0 | 1 => out.push(u32::from(b'a') + rng.below(26) as u32),
            2 => out.extend([0x0D, 0x0A]),
            3 => out.push(0xAC00 + rng.below(11172) as u32),
            4 => out.push(0xAC0A),
            5 => out.extend([
                0xD800 + rng.below(0x400) as u32,
                0xDC00 + rng.below(0x400) as u32,
            ]),
            6 => out.push(0xD800 + rng.below(0x400) as u32),
            7 => out.push(0xDC00 + rng.below(0x400) as u32),
            _ => out.push(rng.below(0x10000) as u32),
        }
    }
    out
}

/// Units of a line in UTF-32: characters, values past 10FFFF, surrogates, any value.
fn utf32_mix(rng: &mut Rng, units: usize) -> Vec<u32> {
    (0..units)
        .map(|_| match rng.below(8) {
            0 | 1 => u32::from(b'a') + rng.below(26) as u32,
            2 => 0x0A,
            3 => 0xAC00 + rng.below(11172) as u32,
            4 => 0x10000 + rng.below(0x100000) as u32,
            5 => 0x110000 + rng.below(0x1000) as u32,
            6 => 0xD800 + rng.below(0x800) as u32,
            _ => rng.next() as u32,
        })
        .collect()
}

/// Bytes heavy in the ones the rules look at.
fn noise(rng: &mut Rng, n: usize) -> Vec<u8> {
    const HOT: [u8; 10] = [0x00, 0x0A, 0x0D, 0x80, 0xBF, 0xC2, 0xD8, 0xDC, 0xE3, 0xF0];
    (0..n)
        .map(|_| match rng.below(3) {
            0 => rng.byte(),
            _ => HOT[rng.below(HOT.len())],
        })
        .collect()
}

/// A random line for `e`, short enough for every place to be tried.
fn random_line(e: Encoding, rng: &mut Rng, size: usize) -> Vec<u8> {
    let mut file = match (rng.below(4), e.unit()) {
        (0, _) => noise(rng, size),
        (_, 1) => utf8_mix(rng, size / 2),
        (_, 2) => unit_bytes(e, &utf16_mix(rng, size / 3)),
        _ => unit_bytes(e, &utf32_mix(rng, size / 5)),
    };
    if rng.below(4) == 0 {
        file.extend((0..rng.below(4)).map(|_| rng.byte()));
    }
    file
}

#[test]
fn ranged_equals_whole_over_random_input() {
    let mut rng = Rng(0x0857_0001);
    for e in VM1 {
        for _ in 0..150 {
            let file = random_line(e, &mut rng, 48);
            let mut los = vec![0];
            let starts = every_start(e, &file);
            los.push(starts[rng.below(starts.len())]);
            check_rules(e, &file, &los);
            for at in 0..4 {
                check_lf(e, &file, at);
            }
        }
    }
}

#[test]
fn a_chain_of_grid_pieces_equals_the_whole_line() {
    let mut rng = Rng(0x0857_0002);
    for e in VM1 {
        for _ in 0..40 {
            let file = random_line(e, &mut rng, 3000);
            for grid in [4, 8, 12, 64, 1024] {
                check_chain(e, &file, grid);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// EA-1 (the model's part): lines end at the LF unit, never at a 0x0A byte inside another.

#[test]
fn ea1_lf_unit_search_over_every_hangul_syllable() {
    let syllables: Vec<char> = (0xAC00..=0xD7A3).filter_map(char::from_u32).collect();
    assert_eq!(syllables.len(), 11_172);
    // 44 of them carry a 0x0A byte in UTF-16 (U+AC0A 갊 = `0a ac` LE, `ac 0a` BE: trace T2).
    let with_0a = syllables
        .iter()
        .filter(|&&c| encode(Utf16Le, &c.to_string()).contains(&0x0A))
        .count();
    assert_eq!(with_0a, 44);
    let lines: Vec<String> = syllables
        .chunks(7)
        .map(|c| c.iter().collect::<String>())
        .collect();
    let text = lines.join("\r\n") + "\r\n";
    for e in [Utf16Le, Utf16Be, Ucs2Le, Ucs2Be, Utf32Le, Utf32Be] {
        let file = encode(e, &text);
        assert!(file.iter().filter(|&&b| b == 0x0A).count() > lines.len());
        assert_eq!(split_lines(e, &file, 0), lines, "{e:?}");
        // Backwards, as a line start is looked for before a place.
        let mut back = Vec::new();
        let mut hi = file.len() - e.unit();
        while let Some(i) = e.rfind_lf(&file[..hi], 0) {
            back.push(i);
            hi = i;
        }
        let mut forward: Vec<usize> = Vec::new();
        let mut pos = 0;
        while let Some(i) = e.find_lf(&file[pos..], pos as u64) {
            forward.push(pos + i);
            pos += i + e.unit();
        }
        forward.pop();
        back.reverse();
        assert_eq!(back, forward, "{e:?}");
    }
}

/// The lines of `file` from byte `from`, each to its LF unit, its CR unit dropped, decoded.
fn split_lines(e: Encoding, file: &[u8], from: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = from;
    while let Some(i) = e.find_lf(&file[pos..], pos as u64) {
        assert_eq!(pos % e.unit(), 0, "{e:?}: a line at {pos}");
        let mut line = &file[pos..pos + i];
        if e.ends_with_cr(line, pos as u64) {
            line = &line[..line.len() - e.unit()];
        }
        let text = text_of(&e.decode(line, true).0);
        assert!(!text.contains(['\r', '\0', FFFD]), "{e:?}: {text:?}");
        out.push(text);
        pos += i + e.unit();
    }
    assert_eq!(pos, file.len(), "{e:?}: the text ends with its LF");
    out
}

#[test]
fn ea1_three_thousand_crlf_lines_of_hangul_and_ascii() {
    let lines: Vec<String> = (0..3000)
        .map(|i| match i % 7 {
            0 => format!("{i:04} 갊 사이 ascii {i}"),
            _ => format!("{i:04} 한글과 ASCII line"),
        })
        .collect();
    let text = lines.join("\r\n") + "\r\n";
    for e in [Utf16Le, Utf16Be, Utf32Le, Utf32Be] {
        let mut file = e.bom().to_vec();
        file.extend(encode(e, &text));
        let p = pick(&file, false);
        assert_eq!(
            p,
            Pick {
                encoding: e,
                guessed: false
            }
        );
        assert_eq!(p.label(e, &file), format!("{} (BOM)", e.label()));
        let got = split_lines(e, &file, e.bom_len(&file));
        assert_eq!(got.len(), 3000, "{e:?}");
        assert_eq!(got, lines, "{e:?}");
        assert!(got.iter().step_by(7).all(|l| l.contains('갊')));
    }
}

// ---------------------------------------------------------------------------------------------
// EA-2 (the model's part): the 24 GB jump.

#[test]
fn ea2_a_utf32le_jump_past_24_gb_is_aligned() {
    let place = 0x3_0000_0003u64;
    let near_at = place - 8;
    let bytes = crate::remote::pattern(near_at, 16);
    let q = Utf32Le.jump_place(Around::new(near_at, &bytes), place);
    assert_eq!(q, 0x3_0000_0000);
    let row = crate::remote::pattern(q, 64);
    let (cells, used) = Utf32Le.decode(&row, false);
    assert_eq!(used, 64);
    assert!(cells.iter().all(|c| c.len == 4));
    let units: Vec<u32> = row
        .as_chunks::<4>()
        .0
        .iter()
        .map(|u| u32_of(u, false))
        .collect();
    let want: Vec<char> = units.iter().map(|&u| char::from_u32(u).unwrap()).collect();
    assert_eq!(cells.iter().map(|c| c.ch).collect::<Vec<_>>(), want);
    // A grid cut, a restart and `left` there stay on the unit too.
    let g = 0x3_0000_4000u64;
    let at = crate::remote::pattern(g - 4, 8);
    assert_eq!(Utf32Le.grid_cut(Around::new(g - 4, &at), g), Some(g));
    assert_eq!(
        Utf32Le.restart_start(Around::new(g - 4, &at), g - 3),
        Some(g)
    );
    assert_eq!(Utf32Le.left_start(Around::new(g - 4, &at), g + 3), Some(g));
}

// ---------------------------------------------------------------------------------------------
// EA-4 (the model's part): invalid input, each as §7.1 says, the next character in place.

fn cells(e: Encoding, bytes: &[u8]) -> Vec<(char, u8)> {
    e.decode(bytes, true)
        .0
        .iter()
        .map(|c| (c.ch, c.len))
        .collect()
}

#[test]
fn ea4_utf16_unpaired_surrogates_are_one_fffd_per_unit() {
    for e in [Utf16Le, Utf16Be] {
        let of = |units: &[u32]| cells(e, &unit_bytes(e, units));
        assert_eq!(of(&[0xD800, 0x41]), [(FFFD, 2), ('A', 2)]);
        assert_eq!(of(&[0x41, 0xDC00, 0x42]), [('A', 2), (FFFD, 2), ('B', 2)]);
        assert_eq!(of(&[0xDC00, 0xD800]), [(FFFD, 2), (FFFD, 2)]);
        assert_eq!(of(&[0xD800, 0xD83D, 0xDE00]), [(FFFD, 2), ('😀', 4)]);
        assert_eq!(of(&[0xD83D, 0xDE00, 0xDE00]), [('😀', 4), (FFFD, 2)]);
        // Not whole, a high surrogate at the end waits for the next piece.
        let piece = unit_bytes(e, &[0x41, 0xD83D]);
        assert_eq!(e.decode(&piece, false).1, 2);
        assert_eq!(e.decode(&piece, true).0[1], Cell { ch: FFFD, len: 2 });
    }
}

#[test]
fn ea4_a_ucs2_surrogate_pair_is_two_fffd() {
    for (ucs2, utf16) in [(Ucs2Le, Utf16Le), (Ucs2Be, Utf16Be)] {
        let pair = unit_bytes(ucs2, &[0xD83D, 0xDE00, 0x41]);
        assert_eq!(cells(ucs2, &pair), [(FFFD, 2), (FFFD, 2), ('A', 2)]);
        assert_eq!(cells(utf16, &pair), [('😀', 4), ('A', 2)]);
        assert_eq!(cells(ucs2, &unit_bytes(ucs2, &[0xAC0A])), [('갊', 2)]);
    }
}

#[test]
fn ea4_a_utf32_value_above_10ffff_or_a_surrogate_is_one_fffd() {
    for e in [Utf32Le, Utf32Be] {
        let units = [0x110000, 0x41, 0xD800, 0x10FFFF, 0xFFFF_FFFF, 0x1F600];
        let want = [
            (FFFD, 4),
            ('A', 4),
            (FFFD, 4),
            ('\u{10FFFF}', 4),
            (FFFD, 4),
            ('😀', 4),
        ];
        assert_eq!(cells(e, &unit_bytes(e, &units)), want);
    }
}

#[test]
fn ea4_a_partial_last_unit_is_one_fffd() {
    for e in [Utf16Le, Utf16Be, Ucs2Le, Ucs2Be, Utf32Le, Utf32Be] {
        for tail in 1..e.unit() {
            let mut file = encode(e, "AB");
            file.extend(std::iter::repeat_n(0x42, tail));
            let got = cells(e, &file);
            let (first, last) = (got[..2].to_vec(), got[2]);
            assert_eq!(first, [('A', e.unit() as u8), ('B', e.unit() as u8)]);
            assert_eq!(last, (FFFD, tail as u8), "{e:?}");
            assert_eq!(e.decode(&file, false).1, 2 * e.unit(), "{e:?}: left");
        }
    }
}

#[test]
fn ea4_ascii_shows_every_byte_from_0x80_as_one_fffd() {
    let every: Vec<u8> = (0..=255).collect();
    let got = cells(Ascii, &every);
    assert_eq!(got.len(), 256);
    for (b, (ch, len)) in got.into_iter().enumerate() {
        let want = if b < 0x80 { char::from(b as u8) } else { FFFD };
        assert_eq!((ch, len), (want, 1), "{b:#04x}");
    }
    assert_eq!(cells(Ascii, b"\x80A"), [(FFFD, 1), ('A', 1)]);
}

#[test]
fn ea4_latin1_and_cp437_over_all_256_bytes() {
    let every: Vec<u8> = (0..=255).collect();
    let latin1 = cells(Latin1, &every);
    let cp437 = cells(Cp437, &every);
    for b in 0..=255u8 {
        let i = usize::from(b);
        assert_eq!(latin1[i], (char::from(b), 1), "{b:#04x}");
        let want = if b < 0x80 {
            char::from(b)
        } else {
            chrome::cp437(b)
        };
        assert_eq!(cp437[i], (want, 1), "{b:#04x}");
    }
    // The skin's table (`chrome.rs:620-647`) for the upper half; C0 and DEL stay themselves
    // here and are drawn as their glyphs (`chrome::shown_char`).
    let at = |b: u8| cp437[usize::from(b)].0;
    assert_eq!(
        [at(0x80), at(0xB0), at(0xDB), at(0xE1), at(0xFF)],
        ['Ç', '░', '█', 'ß', '\u{A0}']
    );
    assert_eq!([at(0x01), at(0x7F)], ['\u{1}', '\u{7F}']);
    assert_eq!([latin1[0xE9].0, latin1[0x85].0], ['é', '\u{85}']);
}

// ---------------------------------------------------------------------------------------------
// EA-5 (the model's part): the default, the BOM rule, the label.

#[test]
fn ea5_a_bom_the_longest_first_whatever_the_size() {
    let picked = |head: &[u8]| pick(head, false);
    let bom = |encoding| Pick {
        encoding,
        guessed: false,
    };
    let file = [0xFF, 0xFE, 0x00, 0x00, 0x41, 0x00, 0x00, 0x00, 0x42];
    assert_eq!(picked(&file), bom(Utf32Le));
    assert_eq!(Utf32Le.bom_len(&file), 4);
    assert_eq!(cells(Utf32Le, &file[4..]), [('A', 4), (FFFD, 1)]);
    for (head, e) in [
        (&[0xFF, 0xFE, 0x00, 0x00][..], Utf32Le),
        (&[0x00, 0x00, 0xFE, 0xFF], Utf32Be),
        (&[0xEF, 0xBB, 0xBF], Utf8),
        (&[0xFF, 0xFE], Utf16Le),
        (&[0xFE, 0xFF], Utf16Be),
        (&[0xFF, 0xFE, 0x41, 0x00], Utf16Le),
        (&[0xFF, 0xFE, 0x00], Utf16Le),
        (&[0xFE, 0xFF, 0x00, 0x41, 0x00], Utf16Be),
    ] {
        assert_eq!(picked(head), bom(e), "{head:02x?}");
    }
    assert_eq!(
        picked(&[0xEF, 0xBB]),
        Pick {
            encoding: Utf8,
            guessed: false
        }
    );
    assert_eq!(
        picked(&[]),
        Pick {
            encoding: Utf8,
            guessed: false
        }
    );
}

#[test]
fn ea5_a_bom_is_skipped_only_under_its_own_encoding() {
    let mut sh = vec![0xEF, 0xBB, 0xBF];
    sh.extend_from_slice(b"#!/bin/sh\necho hi\n");
    assert_eq!(Utf8.bom_len(&sh), 3);
    let line1 = text_of(&Utf8.decode(&sh[3..13], true).0);
    assert_eq!(line1, "#!/bin/sh\n");
    for e in [Latin1, Ascii, Cp437, Utf16Le, Ucs2Be, Utf32Le] {
        assert_eq!(e.bom_len(&sh), 0, "{e:?}");
    }
    assert!(text_of(&Latin1.decode(&sh, true).0).starts_with("ï»¿#!"));
    let lens = |head: &[u8]| Encoding::ALL.map(|e| e.bom_len(head));
    // ASCII, UTF-8, UTF-16LE/BE, UCS-2LE/BE, UTF-32LE/BE, Latin-1, CP437, CP949.
    assert_eq!(
        lens(&[0xFF, 0xFE, 0x41, 0x00]),
        [0, 0, 2, 0, 2, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        lens(&[0xFE, 0xFF, 0x00, 0x41]),
        [0, 0, 0, 2, 0, 2, 0, 0, 0, 0, 0]
    );
    // UTF-32LE's BOM starts with UTF-16LE's, which is skipped as such under UTF-16LE.
    assert_eq!(
        lens(&[0xFF, 0xFE, 0x00, 0x00]),
        [0, 0, 2, 0, 2, 0, 4, 0, 0, 0, 0]
    );
    assert_eq!(
        lens(&[0x00, 0x00, 0xFE, 0xFF]),
        [0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0]
    );
    // A place inside a skipped BOM: Go to `0x1` under UCS-2LE is offset 0, before line 1's
    // start, which VM-2 shows as line 1 (D37).
    let ucs2 = [0xFF, 0xFE, 0x41, 0x00];
    assert_eq!(Ucs2Le.jump_place(Around::anchored(0, &ucs2), 1), 0);
}

#[test]
fn ea5_the_label_says_bom_and_guessed() {
    let mut sh = vec![0xEF, 0xBB, 0xBF];
    sh.extend_from_slice(b"#!/bin/sh\n");
    let p = pick(&sh, false);
    assert_eq!(p.label(Utf8, &sh), "UTF-8 (BOM)");
    assert_eq!(p.label(Latin1, &sh), "Latin-1");
    assert_eq!(p.label(Utf8, &sh), "UTF-8 (BOM)");

    let mut le = Utf16Le.bom().to_vec();
    le.extend(encode(Utf16Le, "hello\r\n"));
    let p = pick(&le, false);
    assert_eq!(p.label(Utf16Le, &le), "UTF-16LE (BOM)");
    assert_eq!(p.label(Ucs2Le, &le), "UCS-2LE (BOM)");
    assert_eq!(p.label(Utf16Be, &le), "UTF-16BE");

    let bare = encode(Utf16Le, "plain English, no BOM\n");
    let p = pick(&bare, false);
    assert_eq!(p.label(Utf16Le, &bare), "UTF-16LE (guessed)");
    assert_eq!(p.label(Ucs2Le, &bare), "UCS-2LE");
    assert_eq!(p.label(Utf16Le, &bare), "UTF-16LE (guessed)");

    let p = pick(b"fn main() {}\n", false);
    assert_eq!(p.label(Utf8, b"fn main() {}\n"), "UTF-8");
}

/// §7.2's corpus for steps 1–2, as its python model measured it (2026-11-24).
#[test]
fn ea5_the_nul_pattern_over_the_corpus() {
    let mut rng = Rng(0x0857_0005);
    let english = "The quick brown fox jumps over the lazy dog, again and again.\n".repeat(200);
    let korean = "안녕하세요 오늘은 날씨가 좋습니다. 내일 다시 만나요.\n".repeat(200);
    let log: String = (0..400)
        .map(|i| {
            format!(
                "2026-11-24 10:{:02}:{:02} INFO user={i:04} 사용자가 로그인 성공함\r\n",
                i / 60 % 60,
                i % 60
            )
        })
        .collect();
    let cp949: Vec<u8> = (0..4096)
        .flat_map(|i| match i % 5 {
            0 => vec![b' '],
            _ => vec![0xB0 + rng.below(0x19) as u8, 0xA1 + rng.below(0x5E) as u8],
        })
        .collect();
    let latin1 = encode(
        Latin1,
        &"Café crème, déjà vu: naïve façade à la française. ".repeat(200),
    );
    let random: Vec<u8> = (0..8192).map(|_| rng.byte()).collect();
    let float32: Vec<u8> = (0..2048)
        .flat_map(|i| (i as f32 * 0.01).sin().to_le_bytes())
        .collect();
    let int16 =
        |top: u16| -> Vec<u8> { (0..4096u16).flat_map(|i| (i % top).to_le_bytes()).collect() };
    let int32 = |lo: u32, top: u32| -> Vec<u8> {
        (0..2048u32)
            .flat_map(|i| (lo + i % (top - lo)).to_le_bytes())
            .collect()
    };
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let font: &[u8] = include_bytes!("../../assets/fonts/PxPlus_IBM_VGA_8x16.ttf");

    let found = |encoding| Pick {
        encoding,
        guessed: true,
    };
    let none = Pick {
        encoding: Utf8,
        guessed: false,
    };
    let corpus: Vec<(&str, Vec<u8>, Pick)> = vec![
        (
            "English UTF-16LE",
            encode(Utf16Le, &english),
            found(Utf16Le),
        ),
        (
            "English UTF-16BE",
            encode(Utf16Be, &english),
            found(Utf16Be),
        ),
        (
            "English UTF-32LE",
            encode(Utf32Le, &english),
            found(Utf32Le),
        ),
        (
            "English UTF-32BE",
            encode(Utf32Be, &english),
            found(Utf32Be),
        ),
        (
            "a Korean-and-ASCII log, UTF-16LE",
            encode(Utf16Le, &log),
            found(Utf16Le),
        ),
        ("Korean UTF-16LE", encode(Utf16Le, &korean), none),
        ("Korean UTF-32LE", encode(Utf32Le, &korean), none),
        ("English UTF-8", english.clone().into_bytes(), none),
        ("Korean UTF-8", korean.clone().into_bytes(), none),
        // No pattern; step 3's, CP949 once VM-4 fills it.
        ("CP949 (KS X 1001 pairs)", cp949, found(Cp949)),
        ("Latin-1", latin1, none),
        ("random bytes", random, none),
        ("zeros", vec![0; 8192], none),
        ("float32", float32, none),
        (
            "this test's executable",
            exe[..exe.len().min(8192)].to_vec(),
            none,
        ),
        ("a TrueType font", font.to_vec(), none),
        ("int16 0–1023", int16(1024), none),
        ("int32 0–4095", int32(0, 4096), none),
        // Taken for text: such an extension-less binary opens as text; 2 shows it.
        ("int16 0–127", int16(128), found(Utf16Le)),
        ("int32 0x20–0x7E", int32(0x20, 0x7F), found(Utf32Le)),
    ];
    for (name, bytes, want) in corpus {
        // The answer is the first 8 KiB's (D27).
        let head = &bytes[..bytes.len().min(SNIFF_BYTES)];
        for got in [pick(&bytes, false), pick(head, bytes.len() > SNIFF_BYTES)] {
            if want == found(Cp949) {
                // Steps 1–2 find nothing; step 4 answers until step 3 is filled. Every other
                // sample without a pattern stays UTF-8 then too, so a wrong CP949 pick shows.
                assert!([none, want].contains(&got), "{name}: {got:?}");
            } else {
                assert_eq!(got, want, "{name}");
            }
        }
    }
    // The log is the corpus's 79 %: ASCII units are ≥ 3/4 of it, not much more.
    let units = encode(Utf16Le, &log);
    let sample = &units[..SNIFF_BYTES];
    let ascii = sample
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|u| matches!(u16_of(&u[..], false), 0x09 | 0x0A | 0x0D | 0x20..=0x7E))
        .count();
    let share = ascii as f64 / (SNIFF_BYTES / 2) as f64;
    assert!((0.78..0.80).contains(&share), "{share}");
}

#[test]
fn ea5_the_nul_pattern_thresholds() {
    let utf16 = |units: &[u32]| pick(&unit_bytes(Utf16Le, units), false).encoding;
    // At least 4 whole units, at least 3/4 of them ASCII units (D17).
    assert_eq!(utf16(&[0x41, 0x42, 0x43, 0xFFFF]), Utf16Le);
    assert_eq!(utf16(&[0x41, 0x09, 0x0D, 0x0A]), Utf16Le);
    assert_eq!(utf16(&[0x41, 0x42, 0x43]), Utf8);
    assert_eq!(utf16(&[0x41, 0x42, 0xFFFF, 0xFFFF]), Utf8);
    let mix = |ascii: usize, other: usize| {
        let units: Vec<u32> = std::iter::repeat_n(0x61, ascii)
            .chain(std::iter::repeat_n(0xFFFF, other))
            .collect();
        utf16(&units)
    };
    assert_eq!(mix(5, 2), Utf8, "71 % is under 3/4");
    assert_eq!(mix(6, 2), Utf16Le, "75 % is 3/4");
    assert_eq!(mix(74, 26), Utf8);
    assert_eq!(mix(75, 25), Utf16Le);
    // Two of four, were DEL or VT counted they would be four.
    assert_eq!(
        utf16(&[0x41, 0x42, 0x7F, 0x7F]),
        Utf8,
        "DEL is no ASCII unit"
    );
    assert_eq!(utf16(&[0x41, 0x42, 0x0B, 0x0B]), Utf8, "nor VT");
    assert_eq!(utf16(&[0x41, 0x42, 0x20, 0x7E]), Utf16Le);
    let mut odd = unit_bytes(Utf16Le, &[0x41, 0x42, 0x43]);
    odd.push(0x44);
    assert_eq!(pick(&odd, false).encoding, Utf8, "a partial unit is none");
    // UTF-32 is tried first: in UTF-16LE its text is half NUL units.
    assert_eq!(pick(&encode(Utf32Le, "abcd"), false).encoding, Utf32Le);
    assert_eq!(pick(&encode(Utf32Be, "abcd"), false).encoding, Utf32Be);
    assert_eq!(pick(&encode(Utf16Be, "abcd"), false).encoding, Utf16Be);
}

#[test]
fn ea5_the_sample_is_the_sniffs_8_kib() {
    let mut file = vec![b'a'; SNIFF_BYTES];
    file.extend(encode(Utf16Le, &"wide text\n".repeat(1000)));
    assert_eq!(pick(&file, false).encoding, Utf8);
    let mut file = encode(Utf16Le, &"wide text\n".repeat(1000));
    file.extend(std::iter::repeat_n(0xFF, 64 * 1024));
    assert_eq!(pick(&file, false).encoding, Utf16Le);
}

// ---------------------------------------------------------------------------------------------
// EA-7 (the model's part): cuts and resyncs equal the whole-file decode.

#[test]
fn ea7_a_utf8_character_across_a_grid_point_is_given_once() {
    let file = "ab😀cd가".as_bytes();
    for g in 3..6 {
        let near = Around::new(g as u64 - 3, &file[g - 3..g]);
        assert_eq!(Utf8.grid_cut(near, g as u64), Some(2), "{g}");
    }
    // 가 is bytes 8–10.
    for g in 9..11 {
        let near = Around::new(g as u64 - 3, &file[g - 3..g]);
        assert_eq!(Utf8.grid_cut(near, g as u64), Some(8), "{g}");
    }
    for g in [6, 7, 8, 11] {
        let near = Around::new(g as u64 - 3, &file[g - 3..g]);
        assert_eq!(Utf8.grid_cut(near, g as u64), Some(g as u64), "{g}");
    }
    for g in 0..=file.len() {
        check_chain(Utf8, file, g.max(4));
        let near = Around::anchored(0, &file[..g]);
        let c = Utf8.grid_cut(near, g as u64).unwrap() as usize;
        let mut text = text_of(&Utf8.decode(&file[..c], true).0);
        text += &text_of(&Utf8.decode(&file[c..], true).0);
        assert_eq!(text, "ab😀cd가", "{g}");
    }
}

#[test]
fn ea7_a_utf16_pair_across_a_grid_point_stays_one_character() {
    for e in [Utf16Le, Utf16Be] {
        let file = encode(e, "a😀b");
        let near = Around::new(2, &file[2..4]);
        assert_eq!(e.grid_cut(near, 4), Some(2));
        assert_eq!(text_of(&e.decode(&file[..2], true).0), "a");
        assert_eq!(text_of(&e.decode(&file[2..], true).0), "😀b");
        // A high surrogate before a grid point that pairs with nothing is cut the same way.
        let lone = unit_bytes(e, &[0x41, 0xD83D, 0x42]);
        assert_eq!(e.grid_cut(Around::new(2, &lone[2..4]), 4), Some(2));
        assert_eq!(cells(e, &lone[2..]), [(FFFD, 2), ('B', 2)]);
    }
}

#[test]
fn ea7_a_standalone_low_surrogate_at_a_grid_point_and_a_jump_is_one_fffd() {
    for e in [Utf16Le, Utf16Be] {
        let file = unit_bytes(e, &[0x41, 0xDE00, 0x42]);
        assert_eq!(e.grid_cut(Around::new(0, &file[..2]), 2), Some(2));
        for p in [2, 3] {
            let q = e.jump_place(Around::new(0, &file), p);
            assert_eq!(q, 2, "{e:?} {p}");
            assert_eq!(cells(e, &file[2..]), [(FFFD, 2), ('B', 2)]);
        }
        let x = e.restart_start(Around::new(0, &file), 2);
        assert_eq!(x, Some(2), "a restart keeps it too");
        // So does `left`, at the unit and at its second byte (D38: only a valid pair's low
        // half is moved past).
        for left in [2, 3] {
            let s = e.left_start(Around::new(0, &file), left);
            assert_eq!(s, Some(2), "{e:?} left {left}");
        }
    }
}

#[test]
fn ea7_a_jump_onto_a_pairs_low_half_shows_the_pair_whole() {
    for e in [Utf16Le, Utf16Be] {
        let file = encode(e, "a😀b");
        for p in 2..6 {
            assert_eq!(e.jump_place(Around::new(0, &file), p), 2, "{e:?} {p}");
        }
        assert_eq!(e.jump_place(Around::new(0, &file), 6), 6);
        assert_eq!(text_of(&e.decode(&file[2..], true).0), "😀b");
        // A restart or `left` there moves past the pair instead (rules 2 and 4).
        assert_eq!(e.restart_start(Around::new(0, &file), 4), Some(6));
        assert_eq!(e.left_start(Around::new(0, &file), 5), Some(6));
    }
}

/// Right ×1–3, End and a one-byte sideways wheel step over a line: every `left` the view can
/// reach shows the whole-file decode from its first shown character, and stays on the unit.
#[test]
fn ea7_not_wrapped_every_left_equals_the_whole_decode() {
    let text = "a😀bc😀😀d 가나 e\u{FFFF}";
    for e in [Utf16Le, Utf16Be, Utf8] {
        let line = encode(e, text);
        let all = e.decode(&line, true).0;
        let bounds = starts(&all);
        for left in 0..=line.len() as u64 {
            let s = e.left_start(Around::anchored(0, &line), left).unwrap();
            assert_eq!(s % e.unit() as u64, 0, "{e:?} {left}");
            // The first character start at or after `left` rounded down: the row shows from
            // the character `left` is in, or the next — never further on.
            let floor = e.align_down(left) as usize;
            let first = bounds[bounds.partition_point(|&b| b < floor)];
            assert_eq!(s as usize, first, "{e:?} {left}");
            let i = bounds
                .binary_search(&(s as usize))
                .expect("a character start");
            assert_eq!(
                e.decode(&line[s as usize..], true).0,
                all[i..],
                "{e:?} {left}"
            );
        }
    }
}

/// The review's counterexample: `E3 81 81` at 9..12 read only up to 11, cut at 12. The bytes
/// before 12 were not read, so they are not taken to be 9..11 (which would cut at 10, inside
/// nothing the whole decode has); a debug build stops on the caller's slip.
#[test]
fn a_place_past_the_bytes_read_has_nothing_before_it() {
    let mut file = vec![b'a'; 9];
    file.extend([0xE3, 0x81, 0x81]);
    let short = Around::new(0, &file[..11]);
    match std::panic::catch_unwind(|| Utf8.grid_cut(short, 12)) {
        Ok(cut) => assert_eq!(cut, Some(12)),
        Err(slip) => {
            let said = slip.downcast_ref::<String>().cloned().unwrap_or_default();
            assert!(said.contains("were not read"), "{said}");
        }
    }
    assert_eq!(Utf8.grid_cut(Around::new(0, &file), 12), Some(12));
    assert_eq!(Utf8.grid_cut(Around::new(0, &file[..11]), 11), Some(9));
    // A UTF-16 place at or past the bytes read has no unit, so no pair, and is no slip.
    let pair = encode(Utf16Le, "😀");
    assert_eq!(
        Utf16Le.restart_start(Around::new(0, &pair[..2]), 2),
        Some(2)
    );
    assert_eq!(Utf16Le.left_start(Around::new(0, &pair[..2]), 3), Some(2));
}

// ---------------------------------------------------------------------------------------------
// The list, the look-back, the CP949 slot.

#[test]
fn the_list_and_its_labels() {
    let labels = Encoding::ALL.map(Encoding::label);
    assert_eq!(
        labels,
        [
            "ASCII", "UTF-8", "UTF-16LE", "UTF-16BE", "UCS-2LE", "UCS-2BE", "UTF-32LE", "UTF-32BE",
            "Latin-1", "CP437", "CP949",
        ]
    );
    for e in VM1 {
        assert!(e.supported(), "{e:?}");
        assert_eq!(Encoding::from_label(e.label()), Some(e));
        assert_eq!(Encoding::from_label(&e.label().to_lowercase()), Some(e));
    }
    assert_eq!(Encoding::from_label("UTF-7"), None);
}

#[test]
fn the_units_and_the_look_back() {
    let units = Encoding::ALL.map(Encoding::unit);
    assert_eq!(units, [1, 1, 2, 2, 2, 2, 4, 4, 1, 1, 1]);
    let back = Encoding::ALL.map(Encoding::look_back);
    assert_eq!(back, [0, 3, 2, 2, 0, 0, 0, 0, 0, 0, 256]);
    assert_eq!(W, 256);
    for e in Encoding::ALL {
        assert_eq!(e.lf().len(), e.unit());
        assert_eq!(e.cr().len(), e.unit());
    }
}

#[test]
fn a_change_of_encoding_keeps_the_index_only_with_the_lf_unit() {
    let groups = [
        &[Utf8, Ascii, Latin1, Cp437, Cp949][..],
        &[Utf16Le, Ucs2Le],
        &[Utf16Be, Ucs2Be],
        &[Utf32Le],
        &[Utf32Be],
    ];
    for (i, a) in groups.iter().enumerate() {
        for (j, b) in groups.iter().enumerate() {
            for (&x, &y) in a.iter().flat_map(|x| b.iter().map(move |y| (x, y))) {
                assert_eq!(x.same_lines(y), i == j, "{x:?} {y:?}");
            }
        }
    }
}

#[test]
fn the_phase_is_a_no_op_but_for_cp949() {
    for e in VM1 {
        assert!(!e.needs_phase(), "{e:?}");
        assert_eq!(e.phase(&[0xB0]), None, "{e:?}");
    }
    assert!(Cp949.needs_phase());
}

/// CP949's entry, whatever its slot holds (the empty slot's own test is in the slot): one-byte
/// units, the byte 0x0A, no BOM, a jump's place kept as it is.
#[test]
fn cp949s_entry() {
    assert_eq!(
        (Cp949.unit(), Cp949.lf(), Cp949.cr()),
        (1, &b"\n"[..], &b"\r"[..])
    );
    assert!(Cp949.bom().is_empty());
    let bytes = [0xB0, 0xA1, b'A', 0x0A];
    for p in 0..4 {
        assert_eq!(Cp949.jump_place(Around::new(0, &bytes), p), p);
    }
    assert_eq!(Cp949.find_lf(&bytes, 0), Some(3));
}
