//! The encode side (12 §7.2; §14 EB-2): EB-2's bytes, the refusals with the first character
//! and its place, the line units and no BOM, every byte of the one-byte encodings and every
//! two-byte code of CP949 back to itself, every scalar value held exactly when it is in the
//! encoding's repertoire, and `decode(encode(s)) == s` over random text of each repertoire.

use super::*;
use crate::encoding::Cell;
use encoding_rs::EUC_KR;
use Encoding::*;

const FFFD: char = char::REPLACEMENT_CHARACTER;

fn enc(e: Encoding, text: &str) -> Vec<u8> {
    e.encode(text)
        .unwrap_or_else(|r| panic!("{} {text:?}: {r}", e.label()))
}

fn refused(e: Encoding, text: &str) -> Refused {
    match e.encode(text) {
        Ok(bytes) => panic!("{} {text:?} gave {bytes:02x?}", e.label()),
        Err(r) => r,
    }
}

/// The whole decode of `bytes`: its characters, after checking that it covers them all and
/// that the cells' lengths add up to them.
fn decoded(e: Encoding, bytes: &[u8]) -> Vec<Cell> {
    let (cells, used) = e.decode(bytes, true);
    assert_eq!(used, bytes.len(), "{} {bytes:02x?}", e.label());
    let total: usize = cells.iter().map(|c| usize::from(c.len)).sum();
    assert_eq!(total, bytes.len(), "{} {bytes:02x?}", e.label());
    cells
}

fn text_of(cells: &[Cell]) -> String {
    cells.iter().map(|c| c.ch).collect()
}

/// Every Unicode scalar value.
fn scalars() -> impl Iterator<Item = char> {
    (0..=0x10FFFFu32).filter_map(char::from_u32)
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

    fn pick<T: Copy>(&mut self, of: &[T]) -> T {
        of[self.below(of.len())]
    }
}

/// What `encoding` holds, by its definition (§7.2), independent of the encoder: ASCII up to
/// U+007F, Latin-1 up to U+00FF, UCS-2 up to U+FFFF, CP437 ASCII and the characters its upper
/// half decodes to, CP949 ASCII and what one of its two-byte codes decodes to (`cp949_codes`),
/// the Unicode encodings everything.
fn holds(e: Encoding, c: char, cp437: &[char], cp949: &[char]) -> bool {
    match e {
        Ascii => c.is_ascii(),
        Latin1 => u32::from(c) <= 0xFF,
        Ucs2Le | Ucs2Be => u32::from(c) <= 0xFFFF,
        Cp437 => c.is_ascii() || cp437.contains(&c),
        Cp949 => c.is_ascii() || cp949.binary_search(&c).is_ok(),
        Utf8 | Utf16Le | Utf16Be | Utf32Le | Utf32Be => true,
    }
}

/// The characters CP437's upper half decodes to, in byte order.
fn cp437_high() -> Vec<char> {
    (0x80..=0xFFu8)
        .map(|b| decoded(Cp437, &[b])[0].ch)
        .collect()
}

/// Every two-byte code of CP949 that decodes to one character, by its whole decode (encoding_rs's
/// EUC-KR): the code and the character.
fn cp949_codes() -> Vec<([u8; 2], char)> {
    let mut out = Vec::new();
    for lead in 0x81..=0xFEu8 {
        for trail in 0x00..=0xFFu8 {
            let cells = decoded(Cp949, &[lead, trail]);
            // An unassigned code is one U+FFFD of its two bytes; a byte no trail is, its own.
            if let [Cell { ch, len: 2 }] = cells[..] {
                if ch != FFFD {
                    out.push(([lead, trail], ch));
                }
            }
        }
    }
    out
}

/// CP949's two-byte characters, sorted.
fn cp949_chars() -> Vec<char> {
    let mut out: Vec<char> = cp949_codes().into_iter().map(|(_, c)| c).collect();
    out.sort_unstable();
    out
}

// EB-2, as §14 writes it.

#[test]
fn eb2_a_han_in_each_unicode_encoding() {
    assert_eq!(enc(Utf8, "A한"), [0x41, 0xED, 0x95, 0x9C]);
    assert_eq!(enc(Utf16Le, "A한"), [0x41, 0x00, 0x5C, 0xD5]);
    assert_eq!(enc(Utf16Be, "A한"), [0x00, 0x41, 0xD5, 0x5C]);
    assert_eq!(
        enc(Utf32Le, "A한"),
        [0x41, 0x00, 0x00, 0x00, 0x5C, 0xD5, 0x00, 0x00]
    );
    // The rest of the byte orders, and UCS-2, which is UTF-16 inside the BMP.
    assert_eq!(
        enc(Utf32Be, "A한"),
        [0x00, 0x00, 0x00, 0x41, 0x00, 0x00, 0xD5, 0x5C]
    );
    assert_eq!(enc(Ucs2Le, "A한"), [0x41, 0x00, 0x5C, 0xD5]);
    assert_eq!(enc(Ucs2Be, "A한"), [0x00, 0x41, 0xD5, 0x5C]);
}

#[test]
fn eb2_u1f600_is_a_pair_in_utf16_and_refused_in_ucs2() {
    assert_eq!(enc(Utf16Le, "😀"), [0x3D, 0xD8, 0x00, 0xDE]);
    assert_eq!(enc(Utf16Be, "😀"), [0xD8, 0x3D, 0xDE, 0x00]);
    assert_eq!(enc(Utf32Le, "😀"), [0x00, 0xF6, 0x01, 0x00]);
    assert_eq!(enc(Utf32Be, "😀"), [0x00, 0x01, 0xF6, 0x00]);
    assert_eq!(enc(Utf8, "😀"), [0xF0, 0x9F, 0x98, 0x80]);
    for e in [Ucs2Le, Ucs2Be] {
        assert_eq!(
            refused(e, "😀"),
            Refused {
                ch: '😀',
                at: 0,
                encoding: e
            }
        );
    }
}

#[test]
fn eb2_the_one_byte_encodings() {
    assert_eq!(refused(Ascii, "한").ch, '한');
    assert_eq!(enc(Latin1, "é"), [0xE9]);
    assert_eq!(refused(Latin1, "한").ch, '한');
    assert_eq!(enc(Cp437, "░"), [0xB0]);
}

#[test]
fn eb2_cp949_writes_encoding_rs_bytes_and_refuses_what_it_cannot_map() {
    let (oracle, _, unmappable) = EUC_KR.encode("똠");
    assert!(!unmappable);
    assert_eq!(enc(Cp949, "똠"), &oracle[..]);
    // 똠 is not in KS X 1001: UHC's code for it.
    assert_eq!(enc(Cp949, "똠"), [0x8C, 0x63]);
    assert_eq!(
        refused(Cp949, "😀"),
        Refused {
            ch: '😀',
            at: 0,
            encoding: Cp949
        }
    );
}

// The refusals: the first character the encoding cannot hold, and its byte offset in the text.

#[test]
fn a_refusal_names_the_first_character_and_its_byte_offset() {
    let cases: [(Encoding, &str, char, usize); 9] = [
        // §7.2's paste: refused whole, naming 한 — byte 3, after `a` and the two bytes of é.
        (Latin1, "aé한b", '한', 3),
        (Ascii, "aé한b", 'é', 1),
        (Latin1, "a한b😀", '한', 1),
        (Ucs2Le, "한😀x😀", '😀', 3),
        (Ucs2Be, "ab\r\n😀", '😀', 4),
        // ☺ and ⌂ are the skin's glyphs for 0x01 and 0x7F, which CP437 decodes as U+0001 and
        // U+007F: no byte decodes to them.
        (Cp437, "x☺", '☺', 1),
        (Cp437, "░⌂", '⌂', 3),
        (Cp949, "가😀𠀀", '😀', 3),
        (Cp949, "한글 𠀀", '𠀀', 7),
    ];
    for (e, text, ch, at) in cases {
        assert_eq!(
            e.encode(text),
            Err(Refused {
                ch,
                at,
                encoding: e
            }),
            "{} {text:?}",
            e.label()
        );
        // What comes before it is what the encoding holds of the text.
        assert!(e.encode(&text[..at]).is_ok(), "{} {text:?}", e.label());
    }
}

#[test]
fn a_refusal_says_which_character_and_which_encoding() {
    assert_eq!(
        refused(Latin1, "한").to_string(),
        "한 cannot be written in Latin-1"
    );
    assert_eq!(
        refused(Ucs2Le, "😀").to_string(),
        "😀 cannot be written in UCS-2LE"
    );
    assert_eq!(
        refused(Cp949, "😀").to_string(),
        "😀 cannot be written in CP949"
    );
}

#[test]
fn cp949_refuses_what_encoding_rs_would_write_as_a_character_reference() {
    // encoding_rs's own `encode` writes an unmappable character as `&#…;` and only flags it.
    for text in ["😀", "\u{4E02}", "𠀀", "\u{FFFD}", "\u{80}"] {
        let (oracle, _, unmappable) = EUC_KR.encode(text);
        assert!(unmappable, "{text:?}");
        assert!(oracle.starts_with(b"&#"), "{text:?} {oracle:02x?}");
        let r = refused(Cp949, text);
        assert_eq!((r.ch, r.at), (text.chars().next().unwrap(), 0), "{text:?}");
    }
    // A CJK ideograph outside KS X 1001's hanja, after one inside it: the first is written, the
    // second refused.
    assert!(Cp949.encode("漢").is_ok());
    assert_eq!(refused(Cp949, "漢\u{4E02}").at, "漢".len());
    // The text `&#128512;` itself is ASCII: written as it is, never taken for a refusal.
    assert_eq!(enc(Cp949, "&#128512;"), b"&#128512;");
}

// The line units, and no BOM.

#[test]
fn line_ends_are_the_encodings_own_units_and_no_bom_is_written() {
    for e in Encoding::ALL {
        let ctx = e.label();
        assert_eq!(enc(e, ""), b"", "{ctx}");
        assert_eq!(enc(e, "\n"), e.lf(), "{ctx}");
        assert_eq!(enc(e, "\r"), e.cr(), "{ctx}");
        assert_eq!(enc(e, "\r\n"), [e.cr(), e.lf()].concat(), "{ctx}");
        // One unit for `A`: nothing before it.
        let a = enc(e, "A");
        assert_eq!(a.len(), e.unit(), "{ctx}");
        assert_eq!(text_of(&decoded(e, &a)), "A", "{ctx}");
        assert_eq!(e.bom_len(&enc(e, "A\n")), 0, "{ctx}");
        // The buffer finds the LF unit where the text has its LF.
        let bytes = enc(e, "a\r\nb\nc");
        let first = e.find_lf(&bytes, 0).unwrap();
        assert_eq!(first, 2 * e.unit(), "{ctx}");
        assert!(e.ends_with_cr(&bytes[..first], 0), "{ctx}");
        let next = first + e.lf().len();
        let second = next + e.find_lf(&bytes[next..], next as u64).unwrap();
        assert_eq!(second, 4 * e.unit(), "{ctx}");
    }
}

// Bytes → character → the same bytes, exhaustively where the repertoire is small.

#[test]
fn every_byte_of_ascii_and_latin1_comes_back() {
    for b in 0..=0xFFu8 {
        let ch = decoded(Latin1, &[b])[0].ch;
        assert_eq!(enc(Latin1, &ch.to_string()), [b], "{b:02x}");
        let ch = decoded(Ascii, &[b])[0].ch;
        if b < 0x80 {
            assert_eq!(enc(Ascii, &ch.to_string()), [b], "{b:02x}");
        } else {
            assert_eq!(ch, FFFD);
            assert_eq!(refused(Ascii, &ch.to_string()).ch, FFFD);
        }
    }
}

#[test]
fn every_byte_of_cp437_comes_back() {
    // The upper half is 128 distinct characters, none of them ASCII, so the inverse is one.
    let high = cp437_high();
    let mut sorted = high.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 128);
    assert!(high.iter().all(|c| !c.is_ascii()));
    for b in 0..=0xFFu8 {
        let ch = decoded(Cp437, &[b])[0].ch;
        assert_eq!(enc(Cp437, &ch.to_string()), [b], "{b:02x} {ch:?}");
    }
    // The lower half is ASCII, control bytes included, as the decoder reads it.
    assert_eq!(enc(Cp437, "\u{1}\u{7F}\t"), [0x01, 0x7F, 0x09]);
}

#[test]
fn every_two_byte_code_of_cp949_comes_back() {
    let codes = cp949_codes();
    // UHC's 8,822 syllables beside KS X 1001's 8,224 characters, and the euro and registered
    // signs CP949 added (0xA2E6, 0xA2E7).
    assert_eq!(codes.len(), 17_048);
    for (code, ch) in codes {
        assert_eq!(enc(Cp949, &ch.to_string()), code, "{code:02x?} {ch:?}");
    }
    let mut chars = cp949_chars();
    let n = chars.len();
    chars.dedup();
    assert_eq!(chars.len(), n, "no character has two codes");
}

// Every scalar value: held exactly when it is in the repertoire, and then decoded back to
// itself from the bytes it took; refused otherwise, at 0, never written some other way.

fn check_every_scalar(e: Encoding, expected: usize) {
    let (cp437, cp949) = (cp437_high(), cp949_chars());
    let mut held = 0;
    let mut buf = [0u8; 4];
    for c in scalars() {
        let text = c.encode_utf8(&mut buf);
        match e.encode(text) {
            Ok(bytes) => {
                held += 1;
                assert!(holds(e, c, &cp437, &cp949), "{} {c:?}", e.label());
                let cells = decoded(e, &bytes);
                assert_eq!(cells.len(), 1, "{} {c:?} {bytes:02x?}", e.label());
                assert_eq!(cells[0].ch, c, "{} {bytes:02x?}", e.label());
                // CP949 writes one byte or one two-byte code: never a `&#…;`.
                if e == Cp949 && !c.is_ascii() {
                    assert_eq!(bytes.len(), 2, "{c:?} {bytes:02x?}");
                }
            }
            Err(r) => {
                assert!(!holds(e, c, &cp437, &cp949), "{} {c:?}", e.label());
                assert_eq!(
                    r,
                    Refused {
                        ch: c,
                        at: 0,
                        encoding: e
                    }
                );
            }
        }
    }
    assert_eq!(held, expected, "{}", e.label());
}

/// Every scalar value but the 2,048 surrogates.
const UNICODE: usize = 0x11_0000 - 0x800;

#[test]
fn every_scalar_in_ascii() {
    check_every_scalar(Ascii, 0x80);
}

#[test]
fn every_scalar_in_latin1() {
    check_every_scalar(Latin1, 0x100);
}

#[test]
fn every_scalar_in_cp437() {
    check_every_scalar(Cp437, 0x100);
}

#[test]
fn every_scalar_in_ucs2() {
    check_every_scalar(Ucs2Le, 0x1_0000 - 0x800);
    check_every_scalar(Ucs2Be, 0x1_0000 - 0x800);
}

#[test]
fn every_scalar_in_utf8() {
    check_every_scalar(Utf8, UNICODE);
}

#[test]
fn every_scalar_in_utf16() {
    check_every_scalar(Utf16Le, UNICODE);
    check_every_scalar(Utf16Be, UNICODE);
}

#[test]
fn every_scalar_in_utf32() {
    check_every_scalar(Utf32Le, UNICODE);
    check_every_scalar(Utf32Be, UNICODE);
}

#[test]
fn every_scalar_in_cp949() {
    check_every_scalar(Cp949, 0x80 + 17_048);
}

// decode(encode(s)) == s over random text of each repertoire.

/// A random character `e` holds: often ASCII and the line units, else one of its repertoire
/// (`wide`: its characters beyond ASCII).
fn held_char(rng: &mut Rng, e: Encoding, wide: &[char]) -> char {
    match rng.below(8) {
        0 => '\n',
        1 => '\r',
        2 | 3 => char::from(0x20 + rng.below(0x5F) as u8),
        4 => char::from(rng.below(0x80) as u8),
        _ if wide.is_empty() => char::from(rng.below(0x80) as u8),
        _ => match e {
            Utf8 | Utf16Le | Utf16Be | Utf32Le | Utf32Be => loop {
                // Each plane as often as the BMP's ranges, so pairs and 4-byte UTF-8 are common.
                let v = match rng.below(3) {
                    0 => rng.below(0x800) as u32,
                    1 => rng.below(0x1_0000) as u32,
                    _ => rng.below(0x11_0000) as u32,
                };
                if let Some(c) = char::from_u32(v) {
                    break c;
                }
            },
            _ => rng.pick(wide),
        },
    }
}

/// Each encoding's characters beyond ASCII, for [`held_char`] (the Unicode ones draw their own).
fn wide_of(e: Encoding) -> Vec<char> {
    match e {
        Ascii => Vec::new(),
        Latin1 => (0x80..=0xFFu8).map(char::from).collect(),
        Ucs2Le | Ucs2Be => (0x80..=0xFFFFu32).filter_map(char::from_u32).collect(),
        Cp437 => cp437_high(),
        Cp949 => cp949_chars(),
        Utf8 | Utf16Le | Utf16Be | Utf32Le | Utf32Be => vec!['한'],
    }
}

#[test]
fn decode_of_encode_is_the_text_over_random_text() {
    let mut rng = Rng(0x0E0C_0DE5);
    for e in Encoding::ALL {
        let wide = wide_of(e);
        for _ in 0..2_000 {
            let n = rng.below(48);
            let text: String = (0..n).map(|_| held_char(&mut rng, e, &wide)).collect();
            let bytes = enc(e, &text);
            assert_eq!(text_of(&decoded(e, &bytes)), text, "{} {text:?}", e.label());
            // Character by character: each its own bytes, CR and LF as the encoding's units.
            let mut buf = [0u8; 4];
            let each: Vec<u8> = text
                .chars()
                .flat_map(|c| enc(e, c.encode_utf8(&mut buf)))
                .collect();
            assert_eq!(bytes, each, "{} {text:?}", e.label());
        }
    }
}

#[test]
fn the_first_character_not_held_is_refused_over_random_text() {
    let mut rng = Rng(0x0E0C_8EF5);
    // Characters each encoding does not hold, beside those it does.
    let outside: [(Encoding, &[char]); 7] = [
        (Ascii, &['é', '\u{80}', '한', '😀', FFFD]),
        (Latin1, &['\u{100}', '한', '😀', FFFD]),
        (Cp437, &['☺', '⌂', '\u{80}', '\u{100}', '한', '😀']),
        (Ucs2Le, &['😀', '𠀀', '\u{10FFFF}']),
        (Ucs2Be, &['😀', '𠀀', '\u{10000}']),
        (Cp949, &['😀', '𠀀', '\u{4E02}', '\u{80}', FFFD, 'é']),
        (Latin1, &['\u{FFFF}', '\u{10000}']),
    ];
    let (cp437, cp949) = (cp437_high(), cp949_chars());
    for (e, out) in outside {
        assert!(
            out.iter().all(|&c| !holds(e, c, &cp437, &cp949)),
            "{}",
            e.label()
        );
        let wide = wide_of(e);
        for _ in 0..2_000 {
            let n = rng.below(32);
            let mut text: String = (0..n).map(|_| held_char(&mut rng, e, &wide)).collect();
            let at = text
                .char_indices()
                .map(|(i, _)| i)
                .nth(rng.below(n + 1))
                .unwrap_or(text.len());
            let ch = rng.pick(out);
            text.insert(at, ch);
            // Characters after it, held or not, do not change what is named.
            for _ in 0..rng.below(3) {
                text.push(rng.pick(out));
            }
            assert_eq!(
                e.encode(&text),
                Err(Refused {
                    ch,
                    at,
                    encoding: e
                }),
                "{} {text:?}",
                e.label()
            );
        }
    }
}
