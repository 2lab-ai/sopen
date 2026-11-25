//! CP949 / EUC-KR (`.prd/08-viewer.md` §7.4; §11 EA-3, EA-4 and EA-5 — the model's part — and
//! EA-7's CP949 cuts; §12): the decoder held to encoding_rs's own whole-input decode, the sync
//! bytes and the phase, every rule of §7.3 held to the whole decode — exhaustively over the
//! bytes that matter and over random Korean and random bytes — and the default's step 3 (D39)
//! over §7.2's corpus, generated here (no fixtures).

use super::*;
use encoding_rs::{DecoderResult, BIG5, EUC_JP, EUC_KR, GBK, SHIFT_JIS, WINDOWS_1252};
use Encoding::{Cp949, Utf16Le, Utf8};

const FFFD: char = char::REPLACEMENT_CHARACTER;

/// A sync byte (§7.4), the contract's three ranges: never a trail, never a lead.
fn sync(b: u8) -> bool {
    matches!(b, 0x00..=0x40 | 0x5B..=0x60 | 0x7B..=0x7F)
}

/// encoding_rs's whole decode (WHATWG EUC-KR = windows-949): the oracle of the characters.
fn oracle(bytes: &[u8]) -> String {
    EUC_KR.decode_without_bom_handling(bytes).0.into_owned()
}

/// Whether encoding_rs, fed `bytes` from a character start, still holds a lead at their end:
/// the oracle of the phase.
fn oracle_pending(bytes: &[u8]) -> bool {
    let mut d = EUC_KR.new_decoder_without_bom_handling();
    let mut out = vec![0u16; bytes.len() + 8];
    let mut src = bytes;
    loop {
        let (r, read, _) = d.decode_to_utf16_without_replacement(src, &mut out, false);
        src = &src[read..];
        if r == DecoderResult::InputEmpty {
            break;
        }
    }
    let (end, _, _) = d.decode_to_utf16_without_replacement(&[], &mut out, true);
    matches!(end, DecoderResult::Malformed(..))
}

fn text_of(cells: &[Cell]) -> String {
    cells.iter().map(|c| c.ch).collect()
}

fn cells(bytes: &[u8]) -> Vec<(char, u8)> {
    Cp949
        .decode(bytes, true)
        .0
        .iter()
        .map(|c| (c.ch, c.len))
        .collect()
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

    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len())]
    }
}

/// Every string of up to `max` bytes of `alphabet`.
fn strings(alphabet: &[u8], max: usize) -> Vec<Vec<u8>> {
    let mut out = vec![Vec::new()];
    let mut last = vec![Vec::new()];
    for _ in 0..max {
        let next: Vec<Vec<u8>> = last
            .iter()
            .flat_map(|s: &Vec<u8>| {
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

/// `text` as CP949 bytes, by encoding_rs's encoder (a syllable outside KS X 1001 as UHC).
fn cp949(text: &str) -> Vec<u8> {
    let (bytes, _, unmappable) = EUC_KR.encode(text);
    assert!(!unmappable, "{text}");
    bytes.into_owned()
}

/// The 11,172 Hangul syllables split as CP949 writes them: KS X 1001's 2,350 (both bytes ≥
/// 0xA1) and the 8,822 only UHC has.
fn syllables() -> (Vec<char>, Vec<char>) {
    let (ks, uhc): (Vec<char>, Vec<char>) = (0xAC00..=0xD7A3u32)
        .filter_map(char::from_u32)
        .partition(|c| cp949(&c.to_string()).iter().all(|&b| b >= 0xA1));
    assert_eq!((ks.len(), uhc.len()), (2350, 8822));
    (ks, uhc)
}

/// KS X 1001's 4,888 hanja (rows 0xCA–0xFD).
fn hanja() -> Vec<char> {
    let mut out = Vec::new();
    for lead in 0xCA..=0xFDu8 {
        for trail in 0xA1..=0xFEu8 {
            let s = oracle(&[lead, trail]);
            assert!(!s.contains(FFFD), "{lead:02x} {trail:02x}");
            out.extend(s.chars());
        }
    }
    assert_eq!(out.len(), 4888);
    out
}

/// Bytes heavy in the ones CP949's rules look at: sync bytes of each range, letters a trail
/// may be (or not, after a lead from 0xC7), 0x80 and 0xFF, each range of leads.
fn noise(rng: &mut Rng, n: usize) -> Vec<u8> {
    const HOT: [u8; 18] = [
        0x00, 0x0A, 0x20, 0x40, 0x41, 0x53, 0x5B, 0x61, 0x7B, 0x7F, 0x80, 0x81, 0xA0, 0xA1, 0xB0,
        0xC6, 0xC7, 0xFF,
    ];
    (0..n)
        .map(|_| match rng.below(3) {
            0 => rng.byte(),
            _ => rng.pick(&HOT),
        })
        .collect()
}

/// A random line of Korean in CP949: KS X 1001 and UHC syllables, hanja, ASCII, a stray byte now
/// and then; `spaced`: with spaces and punctuation (sync bytes), else none at all.
fn korean(
    rng: &mut Rng,
    ks: &[char],
    uhc: &[char],
    hanja: &[char],
    n: usize,
    spaced: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    while out.len() < n {
        match rng.below(12) {
            0 if spaced => out.push(b' '),
            1 if spaced => out.extend_from_slice(b". "),
            2 => out.extend(cp949(&rng.pick(uhc).to_string())),
            3 => out.extend(cp949(&rng.pick(hanja).to_string())),
            4 => out.push(b'a' + rng.below(26) as u8),
            5 if rng.below(4) == 0 => out.push(rng.byte() | 0x80),
            _ => out.extend(cp949(&rng.pick(ks).to_string())),
        }
    }
    out
}

/// The decoder over `file` against encoding_rs: the same characters, each its bytes (an ASCII
/// character one, any other two, a U+FFFD the bytes encoding_rs counted bad); a piece not
/// `whole` leaves a pending lead for the next; the phase is encoding_rs's own pending state —
/// an odd run of 0x81–0xFE bytes at the end.
fn check_decode(file: &[u8]) {
    let ctx = || format!("{file:02x?}");
    let (all, used) = Cp949.decode(file, true);
    assert_eq!(used, file.len(), "{}", ctx());
    assert_eq!(text_of(&all), oracle(file), "{}", ctx());
    assert_eq!(starts(&all)[all.len()], file.len(), "{}", ctx());
    for c in &all {
        match c.ch {
            FFFD => assert!(matches!(c.len, 1 | 2), "{}", ctx()),
            ch if ch.is_ascii() => assert_eq!(c.len, 1, "{}", ctx()),
            _ => assert_eq!(c.len, 2, "{}", ctx()),
        }
    }
    // Every start splits the whole decode in two.
    let whole = oracle(file);
    for &s in &starts(&all) {
        let two = oracle(&file[..s]) + &oracle(&file[s..]);
        assert_eq!(two, whole, "{} at {s}", ctx());
    }
    let pending = oracle_pending(file);
    let run = file
        .iter()
        .rev()
        .take_while(|&&b| (0x81..=0xFE).contains(&b))
        .count();
    assert_eq!(
        pending,
        run % 2 == 1,
        "{}: the phase is the run's parity",
        ctx()
    );
    assert_eq!(Cp949.phase(file), Some(pending), "{}", ctx());
    let (piece, end) = Cp949.decode(file, false);
    assert_eq!(end, file.len() - usize::from(pending), "{}", ctx());
    assert_eq!(piece[..], all[..piece.len()], "{}", ctx());
    assert_eq!(starts(&piece)[piece.len()], end, "{}", ctx());
}

/// Every rule of §7.3 over `file` (one line from byte 0), held to its whole decode (§7.4):
/// - rule 1, the grid cut at `g`: g − 1 when a lead is pending there, else g — from the line
///   start (anchored), or from the last sync byte in [g − W, g); with neither, none;
/// - rules 2 and 4: the first character start at or after the place — from the line start, or a
///   sync byte within W before it; with neither, `left` has none and a restart takes the
///   window's first sync byte, or none;
/// - rule 3: a jump's place is the byte itself (one-byte units).
fn check_rules(file: &[u8]) {
    let n = file.len();
    let all = Cp949.decode(file, true).0;
    let bounds = starts(&all);
    let ctx = || format!("{file:02x?}");
    let is_start = |x: usize| bounds.binary_search(&x).is_ok();
    let first = |x: usize| bounds[bounds.partition_point(|&b| b < x)];
    let synced = |lo: usize, hi: usize| file[lo..hi].iter().any(|&b| sync(b));

    for g in 0..=n {
        let ctx = || format!("{} grid point {g}", ctx());
        let want = g - usize::from(oracle_pending(&file[..g]));
        assert!(is_start(want), "{}: {want}", ctx());
        assert_eq!(Cp949.decode(&file[..g], false).1, want, "{}", ctx());
        let line = Around::anchored(0, &file[..g]);
        assert_eq!(
            Cp949.grid_cut(line, g as u64),
            Some(want as u64),
            "{}",
            ctx()
        );
        let lo = g.saturating_sub(W);
        let near = Around::new(lo as u64, &file[lo..g]);
        let got = Cp949.grid_cut(near, g as u64);
        let proven = synced(lo, g).then_some(want as u64);
        assert_eq!(got, proven, "{}", ctx());
        let mut pieces = Cp949.decode(&file[..want], true).0;
        pieces.extend(Cp949.decode(&file[want..], true).0);
        assert_eq!(pieces, all, "{}", ctx());
    }

    for x in 0..=n {
        let ctx = || format!("{} place {x}", ctx());
        let ahead = (x + AHEAD).min(n);
        let line = Around::anchored(0, &file[..ahead]);
        let exact = Some(first(x) as u64);
        assert_eq!(Cp949.left_start(line, x as u64), exact, "{}", ctx());
        assert_eq!(Cp949.restart_start(line, x as u64), exact, "{}", ctx());
        let lo = x.saturating_sub(W);
        let near = Around::new(lo as u64, &file[lo..ahead]);
        let proven = synced(lo, x).then_some(first(x) as u64);
        assert_eq!(Cp949.left_start(near, x as u64), proven, "{}", ctx());
        // A restart's window from the place: its first sync byte, a character start.
        let ahead_sync = file[x..].iter().position(|&b| sync(b)).map(|i| x + i);
        if let Some(s) = ahead_sync {
            assert!(
                is_start(s) && is_start(s + 1),
                "{}: sync byte at {s}",
                ctx()
            );
        }
        let window = Around::new(x as u64, &file[x..]);
        let got = Cp949.restart_start(window, x as u64);
        assert_eq!(got, ahead_sync.map(|s| s as u64), "{}", ctx());
        // With bytes before it too: exact from a sync byte there, else the same.
        let wide = Around::new(lo as u64, &file[lo..]);
        let want = proven.or(ahead_sync.map(|s| s as u64));
        assert_eq!(Cp949.restart_start(wide, x as u64), want, "{}", ctx());
    }

    for p in 0..n {
        let near = Around::new(0, file);
        assert_eq!(Cp949.jump_place(near, p as u64), p as u64, "{}", ctx());
    }
}

/// The text model's pieces over a line: cut at every `grid` bytes (rule 1) from the line
/// start, a piece after a cut nothing proves shown from its first sync byte (placeholder marks
/// before it, D28). Every character shown is the whole decode's, at its place.
fn check_chain(file: &[u8], grid: usize) {
    let all = Cp949.decode(file, true).0;
    let bounds = starts(&all);
    let at = |x: usize| bounds.binary_search(&x).expect("a character start");
    // (start, proven): where the piece is shown from, and whether a rule proved it.
    let (mut start, mut proven) = (0usize, true);
    let mut g = grid;
    let mut shown_bytes = 0;
    loop {
        let end = g.min(file.len());
        let from = if proven {
            Some(start)
        } else {
            let window = Around::new(start as u64, &file[start..end]);
            Cp949
                .restart_start(window, start as u64)
                .map(|s| s as usize)
        };
        if let Some(from) = from {
            let (shown, used) = Cp949.decode(&file[from..end], end == file.len());
            let k = at(from);
            assert_eq!(
                shown[..],
                all[k..k + shown.len()],
                "grid {grid} piece at {start}"
            );
            shown_bytes += used;
        }
        if end == file.len() {
            break;
        }
        let lo = g.saturating_sub(W).max(start);
        let near = Around {
            at: lo as u64,
            bytes: &file[lo..g],
            anchored: proven && lo == start,
        };
        match Cp949.grid_cut(near, g as u64) {
            Some(c) => (start, proven) = (c as usize, true),
            None => (start, proven) = (g, false),
        }
        g += grid;
    }
    // The first piece, from the line start, is proven; with a sync byte in every W bytes, every
    // cut is, and the whole line is characters.
    assert!(
        shown_bytes + 1 >= grid.min(file.len()),
        "grid {grid}: {shown_bytes}"
    );
    if file.windows(W).all(|w| w.iter().any(|&b| sync(b))) {
        assert_eq!(shown_bytes, file.len(), "grid {grid}");
    }
}

// ---------------------------------------------------------------------------------------------
// The entry.

#[test]
fn cp949_is_decoded_and_offered() {
    assert!(Cp949.supported());
    assert_eq!(Encoding::from_label("CP949"), Some(Cp949));
    assert_eq!(Encoding::from_label("cp949"), Some(Cp949));
    assert!(Cp949.needs_phase());
    assert_eq!(Cp949.look_back(), W);
    assert_eq!(Cp949.label(), "CP949");
}

// ---------------------------------------------------------------------------------------------
// EA-4 (the model's part): invalid input as encoding_rs says, the next character in place.

#[test]
fn ea4_cp949_invalid_input() {
    // A lead from 0xC7 takes no letter as its trail; below it, a letter is the UHC extension.
    assert_eq!(cells(&[0xD0, 0x41]), [(FFFD, 1), ('A', 1)]);
    assert_eq!(cells(&[0xB0, 0x41]), [('캚', 2)]);
    assert_eq!(cells(&[0x81, 0x41]), [('갂', 2)]);
    assert_eq!(
        cells(&[0xD0, 0x41, 0xB0, 0x41]),
        [(FFFD, 1), ('A', 1), ('캚', 2)]
    );
    // A lead before a sync byte: one U+FFFD, the sync byte its own character.
    assert_eq!(cells(&[0xB0, 0x20]), [(FFFD, 1), (' ', 1)]);
    assert_eq!(cells(&[0xB0, 0x0A, 0x41]), [(FFFD, 1), ('\n', 1), ('A', 1)]);
    // 갊 is `b0 a7` (KS X 1001).
    assert_eq!(cells(&[0xB0, 0xA7]), [('갊', 2)]);
    assert_eq!(cp949("갊"), [0xB0, 0xA7]);
    // 0x80 and 0xFF lead nothing: alone one U+FFFD each; after a lead, one two-byte U+FFFD.
    assert_eq!(cells(&[0x80, 0x41, 0xFF]), [(FFFD, 1), ('A', 1), (FFFD, 1)]);
    assert_eq!(cells(&[0xB0, 0x80, 0x41]), [(FFFD, 2), ('A', 1)]);
    assert_eq!(cells(&[0xB0, 0xFF, 0x41]), [(FFFD, 2), ('A', 1)]);
    // An unassigned pair (KS X 1001's row 0xC9 is user-defined): one two-byte U+FFFD.
    assert_eq!(cells(&[0xC9, 0xA1, 0x41]), [(FFFD, 2), ('A', 1)]);
    // A lead as the last byte: one U+FFFD when the piece is whole, left for the next one when
    // it is not.
    assert_eq!(cells(&[0x41, 0xB0]), [('A', 1), (FFFD, 1)]);
    assert_eq!(Cp949.decode(&[0x41, 0xB0], false).1, 1);
    assert_eq!(Cp949.decode(&[0x41, 0xB0, 0xA7], false).1, 3);
}

// ---------------------------------------------------------------------------------------------
// The decoder and the phase against encoding_rs.

#[test]
fn the_decoder_and_the_phase_equal_encoding_rs_exhaustively() {
    let alphabet = [
        0x0A, 0x20, 0x41, 0x53, 0x7B, 0x80, 0x81, 0xA1, 0xB0, 0xC6, 0xC7, 0xFF,
    ];
    for file in strings(&alphabet, 4) {
        check_decode(&file);
    }
    for file in strings(&[0x20, 0x41, 0x81, 0xB0, 0xFF], 6) {
        check_decode(&file);
    }
    // Every pair of bytes.
    for lead in 0..=255u8 {
        for trail in 0..=255u8 {
            check_decode(&[lead, trail, b'A']);
        }
    }
}

#[test]
fn the_decoder_and_the_phase_equal_encoding_rs_over_random_input() {
    let (ks, uhc) = syllables();
    let hanja = hanja();
    let mut rng = Rng(0x0857_0949);
    for i in 0..400 {
        let file = match i % 3 {
            0 => noise(&mut rng, 300),
            1 => korean(&mut rng, &ks, &uhc, &hanja, 300, true),
            _ => korean(&mut rng, &ks, &uhc, &hanja, 300, false),
        };
        check_decode(&file);
        for k in [1, 2, 3, 150, 299] {
            check_decode(&file[..k.min(file.len())]);
        }
    }
}

/// §7.4's measure: decoding in two pieces cut at a sync byte — or right after it — equals
/// decoding whole, at every cut; cut at a byte from 0x81 inside Korean text, it does not.
#[test]
fn a_cut_at_a_sync_byte_equals_the_whole_decode() {
    let (ks, uhc) = syllables();
    let hanja = hanja();
    let mut rng = Rng(0x0857_5ACE);
    let (mut cuts, mut differed) = (0usize, 0usize);
    for i in 0..600 {
        let file = match i % 2 {
            0 => noise(&mut rng, 200),
            _ => korean(&mut rng, &ks, &uhc, &hanja, 200, true),
        };
        let whole = oracle(&file);
        for (s, &b) in file.iter().enumerate() {
            if sync(b) {
                for cut in [s, s + 1] {
                    let two = oracle(&file[..cut]) + &oracle(&file[cut..]);
                    assert_eq!(two, whole, "{file:02x?} cut at {cut}");
                    cuts += 1;
                }
            } else if b >= 0x81 && i % 2 == 1 {
                differed += usize::from(oracle(&file[..s]) + &oracle(&file[s..]) != whole);
            }
        }
    }
    assert!(cuts > 10_000, "{cuts}");
    assert!(differed > 1000, "{differed}");
}

// ---------------------------------------------------------------------------------------------
// Ranged equals whole (§12): every rule, exhaustively and over random input.

#[test]
fn cp949_ranged_equals_whole_exhaustively() {
    // Sync bytes of each range, a letter a lead may take (or not), 0x80, each kind of lead.
    let alphabet = [0x0A, 0x20, 0x41, 0x5B, 0x80, 0x81, 0xB0, 0xC7, 0xFF];
    for file in strings(&alphabet, 4) {
        check_rules(&file);
    }
    for file in strings(&[0x20, 0x41, 0x81, 0xB0], 6) {
        check_rules(&file);
    }
}

#[test]
fn cp949_ranged_equals_whole_over_random_input() {
    let (ks, uhc) = syllables();
    let hanja = hanja();
    let mut rng = Rng(0x0857_0007);
    for i in 0..120 {
        let file = match i % 3 {
            0 => noise(&mut rng, 40),
            1 => korean(&mut rng, &ks, &uhc, &hanja, 40, true),
            _ => korean(&mut rng, &ks, &uhc, &hanja, 40, false),
        };
        check_rules(&file);
    }
    // Lines longer than W: a place with no sync byte within W has no proof but the line start.
    for i in 0..6 {
        let mut file = vec![b'a'];
        file.extend(korean(&mut rng, &ks, &uhc, &hanja, 700, i % 2 == 0));
        check_rules(&file);
    }
}

/// EA-7 (the model's part): pieces cut on a grid from the line start, a character across each
/// grid point given once; past a stretch with no sync byte, the pieces show marks up to the next
/// sync byte and never a character the whole decode lacks.
#[test]
fn ea7_a_chain_of_cp949_grid_pieces_equals_the_whole_line() {
    let (ks, uhc) = syllables();
    let hanja = hanja();
    let mut rng = Rng(0x0857_0C4A);
    for i in 0..30 {
        let mut file = if i % 2 == 0 { vec![b'a'] } else { Vec::new() };
        file.extend(korean(&mut rng, &ks, &uhc, &hanja, 3000, i % 3 != 0));
        for grid in [7, 64, 255, 256, 300, 1024] {
            check_chain(&file, grid);
        }
    }
}

/// Phase checkpoints (§7.4, D28): along a line with no sync byte, one bit per grid point — a
/// lead pending there — each from the start the one before proved; every start so proved is a
/// character start of the whole decode, and decoding from it gives the whole decode.
#[test]
fn checkpoints_chained_along_a_line_give_its_character_starts() {
    let (ks, uhc) = syllables();
    let hanja = hanja();
    let mut rng = Rng(0x0857_C4EC);
    for odd in [false, true] {
        // One ASCII byte first: every even offset is inside a syllable.
        let mut file = if odd { vec![b'a'] } else { Vec::new() };
        file.extend(korean(&mut rng, &ks, &uhc, &hanja, 5000, false));
        assert!(!file[1..].iter().any(|&b| sync(b)));
        let all = Cp949.decode(&file, true).0;
        let bounds = starts(&all);
        for grid in [37usize, 256, 1000] {
            let mut proof = 0usize;
            let mut g = grid;
            let mut odd_starts = 0;
            while g < file.len() {
                let bit = Cp949.phase(&file[proof..g]).unwrap();
                let s = g - usize::from(bit);
                let k = bounds
                    .binary_search(&s)
                    .unwrap_or_else(|_| panic!("grid {grid}: {s} is no character start"));
                assert_eq!(Cp949.decode(&file[s..], true).0[..], all[k..]);
                // The cut a checkpoint proves is rule 1's from the line start.
                let line = Around::anchored(0, &file[..g]);
                assert_eq!(Cp949.grid_cut(line, g as u64), Some(s as u64));
                odd_starts += usize::from(bit);
                proof = s;
                g += grid;
            }
            assert!(
                odd_starts > 0,
                "grid {grid}: some grid point inside a character"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// EA-3 and EA-5 (the model's part): the default's step 3 (D39).

/// Korean prose, `text`, as a file of CRLF lines.
fn prose(words: &[&str], lines: usize) -> String {
    (0..lines)
        .map(|i| {
            let line: Vec<&str> = (0..8)
                .map(|j| words[(i * 7 + j * 3) % words.len()])
                .collect();
            format!("{:04} {}.\r\n", i, line.join(" "))
        })
        .collect()
}

const WORDS: [&str; 16] = [
    "갊",
    "오늘은",
    "날씨가",
    "좋습니다",
    "내일",
    "다시",
    "만나요",
    "한국어",
    "문장을",
    "읽고",
    "파일의",
    "줄을",
    "센다",
    "사람들이",
    "학교에서",
    "공부한다",
];

#[test]
fn ea3_korean_with_crlf_is_cp949_guessed() {
    let text = prose(&WORDS, 300);
    let file = cp949(&text);
    assert!(file.windows(2).any(|w| w == [0xB0, 0xA7]), "갊 is b0 a7");
    let p = pick(&file, false);
    assert_eq!(
        p,
        Pick {
            encoding: Cp949,
            guessed: true
        }
    );
    assert_eq!(p.label(Cp949, &file), "CP949 (guessed)");
    assert_eq!(p.label(Utf8, &file), "UTF-8");
    assert_eq!(text_of(&Cp949.decode(&file, true).0), text);
    // The first 8 KiB alone, the file going on: the same.
    assert_eq!(pick(&file[..SNIFF_BYTES], true), p);
}

/// The sample of §7.2's step-3 corpus `sample` is the first 8 KiB of: it is cut there at four
/// different places of its characters (0–3 ASCII bytes before it), the same answer each time.
fn picked_each_cut(name: &str, body: &[u8]) -> Pick {
    let mut answers = Vec::new();
    for skew in 0..4 {
        let mut file = vec![b'x'; skew];
        file.extend_from_slice(body);
        assert!(file.len() > SNIFF_BYTES + 4, "{name}: {} bytes", file.len());
        let head = &file[..SNIFF_BYTES];
        let got = pick(&file, false);
        assert_eq!(pick(head, true), got, "{name}");
        answers.push(got);
    }
    assert!(
        answers.windows(2).all(|w| w[0] == w[1]),
        "{name}: {answers:?}"
    );
    answers[0]
}

/// The share of `sample`'s decoded non-ASCII characters that are Hangul syllables.
fn hangul_share(sample: &[u8]) -> f64 {
    let text = oracle(sample);
    let other = text.chars().filter(|c| !c.is_ascii()).count();
    let hangul = text
        .chars()
        .filter(|c| ('\u{AC00}'..='\u{D7A3}').contains(c))
        .count();
    hangul as f64 / other.max(1) as f64
}

/// Words: Hangul ones from `ks` (each syllable from `uhc` one time in `uhc_every`, when given)
/// and hanja words one time in `hanja_every` (when given), with spaces and CRLF.
fn generated(
    rng: &mut Rng,
    ks: &[char],
    uhc: &[char],
    hanja: &[char],
    uhc_every: Option<usize>,
    hanja_every: Option<usize>,
    bytes: usize,
) -> String {
    let mut out = String::new();
    let mut words = 0;
    while cp949(&out).len() < bytes {
        let hanja_word = hanja_every.is_some_and(|k| rng.below(k) == 0);
        for _ in 0..1 + rng.below(3) {
            let c = if hanja_word {
                rng.pick(hanja)
            } else if uhc_every.is_some_and(|k| rng.below(k) == 0) {
                rng.pick(uhc)
            } else {
                rng.pick(ks)
            };
            out.push(c);
        }
        words += 1;
        out.push_str(if words % 10 == 0 { ".\r\n" } else { " " });
    }
    out
}

/// §7.2's step-3 corpus, generated: → CP949 every Korean CP949 / EUC-KR sample up to 8 %
/// hanja; → not, everything else — UTF-8 then, there being no pattern of step 2. Each sample
/// cut at four different 8 KiB boundaries, the same answer each time.
#[test]
fn ea5_step_3_over_the_corpus() {
    let (ks, uhc) = syllables();
    let hanja = hanja();
    let mut rng = Rng(0x0857_0003);
    let found = Pick {
        encoding: Cp949,
        guessed: true,
    };
    let none = Pick {
        encoding: Utf8,
        guessed: false,
    };
    let big = 12 * 1024;
    let korean_text = prose(&WORDS, 400);
    let c_source: String = (0..300)
        .map(|i| {
            format!(
                "/* {i}: 파일을 읽고 줄을 센다 */\nint count_{i}(const char *path) {{\n    \
                 return lines(path); // 줄 수를 돌려준다\n}}\n"
            )
        })
        .collect();
    let news = generated(&mut rng, &ks, &uhc, &hanja, None, Some(16), big);
    let hanja_each: String = generated(&mut rng, &ks, &uhc, &hanja, None, None, big / 2)
        .split(' ')
        .map(|w| format!("{w} {}{}", rng.pick(&hanja), rng.pick(&hanja)))
        .collect::<Vec<_>>()
        .join(" ");
    let with_uhc = generated(&mut rng, &ks, &uhc, &hanja, Some(40), None, big);
    let uhc_mostly = generated(&mut rng, &uhc, &ks, &hanja, Some(4), None, big);
    let one_syllable = format!("{} 한 {}", "ascii text ".repeat(600), "more ".repeat(600));
    let chinese = "中华人民共和国的社会主义生活水平不断提高，上海是一个大城市。".repeat(300);
    let traditional = "中華民國的歷史悠久，臺灣是一個美麗的島嶼。".repeat(400);
    let japanese = "日本語の文章です。漢字とかなを含みます。東京は大きい都市です。".repeat(300);
    let hiragana: String = (0..9000)
        .map(|i| match i % 60 {
            59 => '\n',
            _ => char::from_u32(0x3041 + (i * 7 % 83) as u32).unwrap(),
        })
        .collect();
    let french = "Café crème, déjà vu: naïve façade à la française. ".repeat(300);
    let smart = "“Smart quotes” — the euro € sign, café au lait. ".repeat(300);
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let font: &[u8] = include_bytes!("../../assets/fonts/PxPlus_IBM_VGA_8x16.ttf");
    let encoded = |e: &'static encoding_rs::Encoding, text: &str| e.encode(text).0.into_owned();
    let corpus: Vec<(&str, Vec<u8>, Pick)> = vec![
        ("Korean prose in EUC-KR", cp949(&korean_text), found),
        ("Korean prose in CP949 (some UHC)", cp949(&with_uhc), found),
        ("C source with Korean comments", cp949(&c_source), found),
        ("Korean news with some hanja", cp949(&news), found),
        ("Korean with a hanja word each", cp949(&hanja_each), none),
        ("a file with one syllable", cp949(&one_syllable), none),
        ("UHC-only syllables mostly", cp949(&uhc_mostly), none),
        ("Korean UTF-8", korean_text.clone().into_bytes(), none),
        (
            "BOM-less Korean UTF-16LE",
            korean_text
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect(),
            none,
        ),
        ("GBK", encoded(GBK, &chinese), none),
        ("Big5", encoded(BIG5, &traditional), none),
        ("EUC-JP", encoded(EUC_JP, &japanese), none),
        ("Shift-JIS with kanji", encoded(SHIFT_JIS, &japanese), none),
        ("hiragana-only EUC-JP", encoded(EUC_JP, &hiragana), none),
        (
            "hiragana-only Shift-JIS",
            encoded(SHIFT_JIS, &hiragana),
            none,
        ),
        ("Latin-1", french.chars().map(|c| c as u8).collect(), none),
        ("Windows-1252", encoded(WINDOWS_1252, &smart), none),
        ("random bytes", (0..big).map(|_| rng.byte()).collect(), none),
        (
            "this test's executable",
            exe[..exe.len().min(big)].to_vec(),
            none,
        ),
        ("a TrueType font", font.to_vec(), none),
    ];
    let mut why = std::collections::HashMap::new();
    for (name, body, want) in corpus {
        assert_eq!(picked_each_cut(name, &body), want, "{name}");
        why.insert(name, cp949::criteria(&body[..SNIFF_BYTES], true));
    }
    // (b) stops these: CP949 errors.
    for name in [
        "BOM-less Korean UTF-16LE",
        "GBK",
        "Big5",
        "Shift-JIS with kanji",
        "Latin-1",
        "Windows-1252",
        "random bytes",
        "this test's executable",
        "a TrueType font",
    ] {
        assert!(!why[name][1], "{name}: {:?}", why[name]);
    }
    // Each of the others by the criterion it fails. (This EUC-JP decodes with no error, its
    // kanji as Hangul and hanja: 36 % Hangul, under (d).)
    let stopped = |name: &str, by: &[usize]| {
        let mut want = [true; 6];
        for &i in by {
            want[i] = false;
        }
        assert_eq!(why[name], want, "{name}");
    };
    stopped("Korean with a hanja word each", &[3]);
    stopped("a file with one syllable", &[2]);
    stopped("UHC-only syllables mostly", &[4]);
    stopped("EUC-JP", &[3, 5]);
    assert!(!why["Korean UTF-8"][0]);
    assert!(!why["hiragana-only EUC-JP"][2], "no syllable");
    // The near misses: hiragana-only Shift-JIS decodes as UHC syllables with no error, stopped
    // by (e) alone (none of them in KS X 1001); a GB2312 text whose every character decodes in
    // CP949 is about 2/3 Hangul, stopped by (d) alone.
    stopped("hiragana-only Shift-JIS", &[4, 5]);
    let sjis = encoded(SHIFT_JIS, &hiragana);
    let syllables = oracle(&sjis[..SNIFF_BYTES])
        .chars()
        .filter(|c| ('\u{AC00}'..='\u{D7A3}').contains(c))
        .count();
    assert!(syllables > 4000, "{syllables}");
    let gb2312: Vec<u8> = (0..big / 2)
        .flat_map(|i| match i % 40 {
            39 => vec![b'\n'],
            _ if rng.below(3) < 2 => {
                vec![0xB0 + rng.below(0x19) as u8, 0xA1 + rng.below(0x59) as u8]
            }
            _ => vec![0xCA + rng.below(0x0E) as u8, 0xA1 + rng.below(0x59) as u8],
        })
        .collect();
    assert!(GBK
        .decode_without_bom_handling_and_without_replacement(&gb2312)
        .is_some());
    assert!(
        !oracle(&gb2312).contains(FFFD),
        "every character decodes in CP949"
    );
    assert_eq!(picked_each_cut("a GB2312 text", &gb2312), none);
    assert_eq!(
        cp949::criteria(&gb2312[..SNIFF_BYTES], true),
        [true, true, true, false, true, false]
    );
    let share = hangul_share(&gb2312[..SNIFF_BYTES]);
    assert!((0.60..0.72).contains(&share), "{share}");
    // The samples are what they say.
    let share = |text: &str| hangul_share(&cp949(text)[..SNIFF_BYTES]);
    assert!(
        (0.88..0.96).contains(&share(&news)),
        "news: {}",
        share(&news)
    );
    assert!(
        (0.40..0.60).contains(&share(&hanja_each)),
        "{}",
        share(&hanja_each)
    );
    assert!(
        cp949(&korean_text)
            .iter()
            .all(|&b| !(0x80..0xA1).contains(&b)),
        "EUC-KR only"
    );
    assert!(
        cp949(&with_uhc).iter().any(|&b| (0x81..0xA1).contains(&b)),
        "some UHC"
    );
}

/// D39's thresholds, each at its edge: (a) not valid UTF-8, (b) no CP949 error, (c) at least 4
/// syllables, (d) at least 3/4 of the non-ASCII characters, (e) at least 3/4 of them KS X 1001.
#[test]
fn ea5_step_3_thresholds() {
    let guessed = |sample: &[u8], truncated: bool| pick(sample, truncated).encoding == Cp949;
    // (c): 3 syllables are not enough, 4 are.
    assert!(!guessed(&cp949("가나다 abc"), false));
    assert!(guessed(&cp949("가나다라 abc"), false));
    // (d): hanja among them — 6 of 8 is 3/4, 5 of 7 is under it.
    assert!(guessed(&cp949("가나다라 韓"), false));
    assert!(guessed(&cp949("가나다라마바 韓國"), false));
    assert!(!guessed(&cp949("가나다라마 韓國"), false));
    // (e): UHC-only syllables (갂 = 81 41, 캚 = b0 41) — 3 of 4 in KS X 1001 is enough, 2 of 4
    // is not.
    assert_eq!(cp949("갂캚"), [0x81, 0x41, 0xB0, 0x41]);
    assert!(guessed(&cp949("가나다 갂"), false));
    assert!(!guessed(&cp949("가나 갂캚 abc"), false));
    // (b): one CP949 error anywhere, and it is not CP949.
    let mut bad = cp949("가나다라마바사");
    bad.push(0x80);
    assert!(!guessed(&bad, false));
    // A lead as the sample's last byte is no error when the file goes on, one when it ends.
    let mut cut = cp949("가나다라마바사");
    cut.push(0xB0);
    assert!(guessed(&cut, true));
    assert!(!guessed(&cut, false));
    // (a): syllables from leads 0xC2–0xC8 with trails 0xA1–0xBF are valid UTF-8 (two-byte
    // characters): not CP949 — unless one byte pair is not.
    let utf8_too: Vec<u8> = [0xC2u8, 0xC4, 0xC6, 0xC8]
        .iter()
        .flat_map(|&lead| [lead, 0xA1, lead, 0xB0, lead, 0xBF])
        .collect();
    assert!(std::str::from_utf8(&utf8_too).is_ok());
    assert!(!oracle(&utf8_too).contains(FFFD));
    assert!(!guessed(&utf8_too, false));
    let mut not_utf8 = utf8_too.clone();
    not_utf8.extend(cp949("한"));
    assert!(std::str::from_utf8(&not_utf8).is_err());
    assert!(guessed(&not_utf8, false));
    // A UTF-8 character cut by the sample's end is no error when the file goes on (kind.rs's
    // allowance): still valid UTF-8, so not CP949.
    let mut utf8_cut = utf8_too.clone();
    utf8_cut.push(0xC2);
    assert!(!guessed(&utf8_cut, true));
    // A BOM or the NUL pattern come first: they are not step 3's.
    let mut bom = Utf8.bom().to_vec();
    bom.extend(cp949("가나다라마바사"));
    assert_eq!(pick(&bom, false).encoding, Utf8);
    let wide: Vec<u8> = "plain English"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    assert_eq!(pick(&wide, false).encoding, Utf16Le);
}

// ---------------------------------------------------------------------------------------------
// D39 (f) (DEC 2026-11-25): Korean puts ASCII spaces between words; Chinese prose has next to none.

const DOKRIP_ORIGINAL: &str = "吾等은 玆에 我 朝鮮의 獨立國임과 朝鮮人의 自主民임을 宣言하노라. \
此로써 世界萬邦에 告하야 人類平等의 大義를 克明하며, 此로써 子孫萬代에 誥하야 民族自存의 正權을 \
永有케 하노라. 半萬年 歷史의 權威를 仗하야 此를 宣言함이며, 二千萬 民衆의 誠忠을 合하야 此를 \
佈明함이며, 民族의 恒久如一한 自由發展을 爲하야 此를 主張함이며, 人類的 良心의 發露에 基因한 \
世界改造의 大機運에 順應幷進하기 爲하야 此를 提起함이니, 是ㅣ 天의 明命이며, 時代의 大勢ㅣ며, \
全人類 共存同生權의 正當한 發動이라, 天下何物이던지 此를 沮止抑制치 못할지니라.";

const DOKRIP_MODERN: &str = "우리는 오늘 조선이 독립한 나라이며, 조선 사람이 자주적인 민족임을 \
선언한다. 이로써 세계 모든 나라에 알려 인류가 모두 평등하다는 큰 뜻을 분명히 하고, 이로써 자손 \
만대에 깨우쳐 민족의 독자적 생존의 정당한 권리를 영원히 누리게 하려는 것이다. 반만년 역사의 \
권위에 의지하여 이를 선언하며, 이천만 민중의 정성을 모아 이를 두루 밝히며, 민족의 한결같은 \
자유 발전을 위하여 이를 주장하며, 인류의 양심에서 나온 세계 개조의 큰 기회와 시운에 맞추어 \
함께 나아가기 위하여 이를 제기하니, 이는 하늘의 지시이며 시대의 큰 추세이며, 전 인류 공동 생존권의 \
정당한 발동이기에, 세상의 어떤 힘이라도 이를 막고 억누르지 못할 것이다.";

const HUNMIN_MODERN: &str = "나라의 말이 중국과 달라 문자와 서로 통하지 아니하므로 이런 까닭으로 \
어리석은 백성이 이르고자 할 바가 있어도 마침내 제 뜻을 능히 펴지 못하는 사람이 많으니라. 내가 \
이를 위하여 가엾게 여겨 새로 스물여덟 자를 만드노니 사람마다 하여금 쉬이 익혀 날로 씀에 편안케 \
하고자 할 따름이니라.";

const HUNMIN_HANMUN: &str = "國之語音 異乎中國 與文字不相流通 故愚民 有所欲言 而終不得伸其情者 \
多矣 予爲此憫然 新制二十八字 欲使人人易習 便於日用耳";

const HEONBEOP: &str = "유구한 역사와 전통에 빛나는 우리 대한국민은 3·1운동으로 건립된 \
대한민국임시정부의 법통과 불의에 항거한 4·19민주이념을 계승하고, 조국의 민주개혁과 평화적 \
통일의 사명에 입각하여 정의·인도와 동포애로써 민족의 단결을 공고히 하고, 모든 사회적 폐습과 \
불의를 타파하며, 자율과 조화를 바탕으로 자유민주적 기본질서를 더욱 확고히 하여 정치·경제·사회·\
문화의 모든 영역에 있어서 각인의 기회를 균등히 하고, 능력을 최고도로 발휘하게 하며, 자유와 \
권리에 따르는 책임과 의무를 완수하게 하여, 안으로는 국민생활의 균등한 향상을 기하고 밖으로는 \
항구적인 세계평화와 인류공영에 이바지함으로써 우리들과 우리들의 자손의 안전과 자유와 행복을 \
영원히 확보할 것을 다짐하면서 1948년 7월 12일에 제정되고 8차에 걸쳐 개정된 헌법을 이제 국회의 \
의결을 거쳐 국민투표에 의하여 개정한다.";

const DAODEJING: &str = "道可道，非常道。名可名，非常名。無名天地之始；有名萬物之母。故常無欲，\
以觀其妙；常有欲，以觀其徼。此兩者，同出而異名，同謂之玄。玄之又玄，衆妙之門。\n天下皆知美之為美，\
斯惡已。皆知善之為善，斯不善已。故有無相生，難易相成，長短相較，高下相傾，音聲相和，前後相隨。\
是以聖人處無為之事，行不言之教；萬物作焉而不辭，生而不有，為而不恃，功成而弗居。夫唯弗居，是以不去。\n\
不尚賢，使民不爭；不貴難得之貨，使民不為盜；不見可欲，使民心不亂。是以聖人之治，虛其心，實其腹，\
弱其志，強其骨。常使民無知無欲。使夫智者不敢為也。為無為，則無不治。";

const DAODEJING_SIMPLIFIED: &str = "道可道，非常道。名可名，非常名。无名天地之始；有名万物之母。\
故常无欲，以观其妙；常有欲，以观其徼。此两者，同出而异名，同谓之玄。玄之又玄，众妙之门。\n\
天下皆知美之为美，斯恶已。皆知善之为善，斯不善已。故有无相生，难易相成，长短相较，高下相倾，\
音声相和，前后相随。是以圣人处无为之事，行不言之教；万物作焉而不辞，生而不有，为而不恃，功成而弗居。";

const LUNYU: &str =
    "子曰：「學而時習之，不亦說乎？有朋自遠方來，不亦樂乎？人不知而不慍，不亦君子乎？」\n\
有子曰：「其為人也孝弟，而好犯上者，鮮矣；不好犯上，而好作亂者，未之有也。君子務本，本立而道生。\
孝弟也者，其為仁之本與！」\n子曰：「巧言令色，鮮矣仁！」\n曾子曰：「吾日三省吾身：為人謀而不忠乎？\
與朋友交而不信乎？傳不習乎？」\n子曰：「道千乘之國，敬事而信，節用而愛人，使民以時。」";

const CHUSHIBIAO: &str = "先帝創業未半，而中道崩殂；今天下三分，益州疲弊，此誠危急存亡之秋也。\
然侍衛之臣不懈於內，忠志之士忘身於外者，蓋追先帝之殊遇，欲報之於陛下也。誠宜開張聖聽，以光先帝遺德，\
恢弘志士之氣；不宜妄自菲薄，引喻失義，以塞忠諫之路也。\n宮中府中，俱為一體；陟罰臧否，不宜異同。\
若有作姦犯科及為忠善者，宜付有司論其刑賞，以昭陛下平明之理；不宜偏私，使內外異法也。";

/// `text` in CP949, leaving out what CP949 cannot hold (a few rare hanja), not as `&#…;`.
fn cp949_lossy(text: &str) -> Vec<u8> {
    let kept: String = text
        .chars()
        .filter(|c| !EUC_KR.encode(&c.to_string()).2)
        .collect();
    cp949(&kept)
}

/// `text` over and over, a newline between, to `bytes` or a little past.
fn repeated(text: &str, bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes {
        out.push_str(text);
        out.push('\n');
    }
    out
}

/// A sample's two-byte characters and ASCII characters (printable, and whitespace) as CP949
/// decodes its first 8 KiB.
fn two_byte_and_ascii(sample: &[u8]) -> (usize, usize) {
    let cells = Cp949
        .decode(&sample[..sample.len().min(SNIFF_BYTES)], true)
        .0;
    let two = cells.iter().filter(|c| c.len == 2).count();
    let ascii = cells
        .iter()
        .filter(|c| c.len == 1 && matches!(c.ch, ' '..='~' | '\t' | '\n' | '\r' | '\x0B' | '\x0C'))
        .count();
    (two, ascii)
}

/// D39 (f) over §7.2's excerpts (public domain, encoded here): each sample's ASCII characters
/// per two-byte character, as CP949 decodes its first 8 KiB, and the criteria. Korean prose is
/// 0.33–0.40 (spaces between words); Chinese prose next to none — 0.012 in simplified GBK, up to
/// 0.12 where CP949 reads GBK and Big5 trail bytes as ASCII, all of it CP949 errors (b) anyway.
/// An .srt of Chinese subtitles has ASCII timestamps (2.2): (d) stops it, not (f).
#[test]
fn ea5_f_korean_puts_spaces_between_words_and_chinese_none() {
    let srt: String = (1..200)
        .map(|i| {
            format!(
                "{i}\r\n00:{:02}:{:02},000 --> 00:{:02}:{:02},500\r\n\
                 你好，我们今天学习编码。这是第{i}行字幕。\r\n\r\n",
                i / 60 % 60,
                i % 60,
                i / 60 % 60,
                (i + 2) % 60
            )
        })
        .collect();
    let gbk = |text: &str| GBK.encode(&repeated(text, 3000)).0.into_owned();
    let big5 = |text: &str| BIG5.encode(&repeated(text, 3000)).0.into_owned();
    let ko = |text: &str| cp949_lossy(&repeated(text, 3000));
    let ratio = |body: &[u8]| {
        let (two, ascii) = two_byte_and_ascii(body);
        ascii as f64 / two as f64
    };
    let criteria = |body: &[u8]| {
        cp949::criteria(
            &body[..body.len().min(SNIFF_BYTES)],
            body.len() > SNIFF_BYTES,
        )
    };
    // Korean: well above a tenth; the modern texts are guessed, by every criterion.
    for (name, body, lo) in [
        ("기미독립선언서 현대어", ko(DOKRIP_MODERN), 0.35),
        ("훈민정음 서문 현대어", ko(HUNMIN_MODERN), 0.35),
        ("헌법 전문", ko(HEONBEOP), 0.33),
    ] {
        assert!(ratio(&body) >= lo, "{name}: {}", ratio(&body));
        assert_eq!(criteria(&body), [true; 6], "{name}");
        assert_eq!(pick(&body, false).encoding, Cp949, "{name}");
    }
    // Thick with hanja: (d) stops it, (f) would not; in hanja alone, (c) and (d).
    let original = ko(DOKRIP_ORIGINAL);
    assert!(ratio(&original) >= 0.3, "{}", ratio(&original));
    assert_eq!(criteria(&original), [true, true, true, false, true, true]);
    let hanmun = ko(HUNMIN_HANMUN);
    assert_eq!(criteria(&hanmun), [true, true, false, false, true, true]);
    // Chinese: under a tenth where the bytes are what they say; every one not CP949.
    let simplified = gbk(DAODEJING_SIMPLIFIED);
    assert!(ratio(&simplified) < 0.02, "{}", ratio(&simplified));
    for (name, body) in [
        ("道德經 GBK", gbk(DAODEJING)),
        ("道德經 Big5", big5(DAODEJING)),
        ("道德经 GBK", simplified),
        ("論語 GBK", gbk(LUNYU)),
        ("論語 Big5", big5(LUNYU)),
        ("出師表 GBK", gbk(CHUSHIBIAO)),
        ("出師表 Big5", big5(CHUSHIBIAO)),
    ] {
        assert!(ratio(&body) < 0.13, "{name}: {}", ratio(&body));
        assert!(!criteria(&body)[1], "{name}: CP949 errors");
        assert_eq!(pick(&body, false).encoding, Utf8, "{name}");
    }
    let srt = GBK.encode(&srt).0.into_owned();
    assert!(ratio(&srt) > 2.0, "{}", ratio(&srt));
    assert_eq!(criteria(&srt), [true, true, true, false, true, true]);
    assert_eq!(pick(&srt, false).encoding, Utf8);
}

/// D39 (f)'s floor, both ways: below 32 two-byte characters the guess is (a)–(e), Korean first —
/// GBK's `你好你好` (`C4E3 BAC3 C4E3 BAC3`, KS X 1001 Hangul to CP949) is guessed CP949, F8 the
/// way out (08's Tensions); at 32 and more, a sample with no ASCII is not guessed — GBK's
/// `你好吗啊` over and over, and Korean with no space at all alike — while spaced Korean is.
#[test]
fn ea5_f_holds_from_32_two_byte_characters() {
    let nihao = GBK.encode("你好你好").0.into_owned();
    assert_eq!(nihao, [0xC4, 0xE3, 0xBA, 0xC3, 0xC4, 0xE3, 0xBA, 0xC3]);
    assert_eq!(
        pick(&nihao, false).encoding,
        Cp949,
        "below the floor: Korean first"
    );
    let short = GBK.encode(&"你好吗啊".repeat(7)).0.into_owned();
    assert_eq!(
        pick(&short, false).encoding,
        Cp949,
        "28 two-byte characters"
    );
    let long = GBK.encode(&"你好吗啊".repeat(8)).0.into_owned();
    assert_eq!(
        cp949::criteria(&long, false),
        [true, true, true, true, true, false]
    );
    assert_eq!(pick(&long, false).encoding, Utf8, "32: (f) alone stops it");
    let unspaced = cp949(&"가나다라".repeat(8));
    assert!(!cp949::criteria(&unspaced, false)[5]);
    assert_eq!(
        pick(&unspaced, false).encoding,
        Utf8,
        "Korean with no space at all"
    );
    let spaced = cp949(&"가나다라 ".repeat(8));
    assert_eq!(pick(&spaced, false).encoding, Cp949);
    let one_space = cp949(&format!(
        "{} {}",
        "가나다라".repeat(5),
        "가나다라".repeat(4)
    ));
    assert_eq!(
        pick(&one_space, false).encoding,
        Utf8,
        "one space to 36: under a tenth"
    );
    let enough = cp949(&"가나다 라마바 사아자 차카타 ".repeat(3));
    assert_eq!(pick(&enough, false).encoding, Cp949);
}
