//! CP949 / EUC-KR (`.prd/08-viewer.md` §7.4, VM-4): Microsoft's Unified Hangul Code, a superset
//! of EUC-KR / KS X 1001, decoded by encoding_rs's `EUC_KR` — the WHATWG Encoding Standard's
//! EUC-KR, which is windows-949. One-byte units: a character is one byte (0x00–0x7F) or a lead
//! (0x81–0xFE) and its trail; what is not one is a U+FFFD of the bytes encoding_rs counts bad.
//!
//! No byte below 0x41 is a trail, so 0x0A and 0x0D are whole characters: lines are found by the
//! byte, as in UTF-8. A character start inside a line is taken only from what proves it (D28): a
//! start the text model knows before the place — a line's start within REACH, a piece's, a phase
//! checkpoint's ([`Around::anchored`]) — or a **sync byte** within [`W`] before it (0x00–0x40,
//! 0x5B–0x60, 0x7B–0x7F: never a trail, never a lead, so a character starts at it and right
//! after it, whatever came before; `euc_kr.rs` gives a lead before one a U+FFFD and reads the
//! sync byte again on its own). Decoding runs forward from the proof; with none, the rules give
//! `None` and the text model shows placeholder marks up to the next sync byte.
//!
//! The phase, a checkpoint's bit: decoded from a character start, a lead is pending at the end
//! exactly when the bytes end in an odd run of 0x81–0xFE. After a lead every byte ends it (taken
//! as its trail, or — below 0x80 and no trail of that lead — read again on its own, a character
//! either way), and a byte outside 0x81–0xFE begins nothing (`cp949_tests.rs` holds this to
//! encoding_rs's own state over every pair of bytes and random input).

use super::{Around, Cell, W};
use encoding_rs::{DecoderResult, EUC_KR};

/// Whether CP949 is decoded (VM-4 filled the slot VM-1 left).
pub const SUPPORTED: bool = true;

const FFFD: char = char::REPLACEMENT_CHARACTER;

/// A sync byte (§7.4): never a trail (trails are 0x41–0x5A, 0x61–0x7A, 0x81–0xFE), never a lead.
fn is_sync(b: u8) -> bool {
    matches!(b, 0x00..=0x40 | 0x5B..=0x60 | 0x7B..=0x7F)
}

/// A byte that begins a two-byte character where one begins: 0x81–0xFE.
fn is_lead(b: u8) -> bool {
    (0x81..=0xFE).contains(&b)
}

/// Whether a lead is pending at the end of `bytes`, decoded from a character start: an odd run
/// of 0x81–0xFE ends them.
fn pending(bytes: &[u8]) -> bool {
    bytes.iter().rev().take_while(|&&b| is_lead(b)).count() % 2 == 1
}

/// The decoder: encoding_rs's `EUC_KR` over `bytes`, which start at a character start. Each
/// character takes one byte (ASCII) or two; each bad sequence is one U+FFFD of its one or two
/// bytes. `whole`: a lead as the last byte is one U+FFFD; otherwise it is left for the next
/// piece.
pub fn decode(bytes: &[u8], whole: bool) -> (Vec<Cell>, usize) {
    let end = bytes.len() - usize::from(!whole && pending(bytes));
    let mut out = Vec::with_capacity(end);
    let mut decoder = EUC_KR.new_decoder_without_bom_handling();
    let mut units = [0u16; 1024];
    let mut rest = &bytes[..end];
    loop {
        let (result, read, written) =
            decoder.decode_to_utf16_without_replacement(rest, &mut units, true);
        // EUC-KR writes no surrogate: each unit is a character, ASCII from one byte and any
        // other from two (a pair that decodes to ASCII is unassigned, so bad).
        out.extend(units[..written].iter().map(|&u| Cell {
            ch: char::from_u32(u32::from(u)).unwrap_or(FFFD),
            len: if u < 0x80 { 1 } else { 2 },
        }));
        rest = &rest[read..];
        match result {
            DecoderResult::InputEmpty => break,
            DecoderResult::OutputFull => {}
            DecoderResult::Malformed(bad, extra) => {
                // (EUC-KR reads nothing past a bad sequence: a byte it did not take as a trail
                // is read again.)
                debug_assert_eq!(extra, 0);
                out.push(Cell { ch: FFFD, len: bad });
            }
        }
    }
    (out, end)
}

/// Where decoding toward `x` may begin (§7.4): the last sync byte within [`W`] before it, else
/// the start `near` is anchored at; `None` when nothing proves one. (A known start and a sync
/// byte give the same characters from the sync byte on: the nearer is decoded from.)
fn proof(near: &Around, x: u64) -> Option<u64> {
    let w = near.before(x, W);
    let lo = x - w.len() as u64;
    match w.iter().rposition(|&b| is_sync(b)) {
        Some(i) => Some(lo + i as u64),
        None => (near.anchored && near.at <= x).then_some(near.at),
    }
}

/// The first character start at or after `x`, decoding forward from `p`, a character start at
/// or before it, over what `near` read: `x` itself when no lead is pending there ([`pending`]
/// over the bytes from `p`), else after the character the lead at x − 1 begins — the lead
/// alone, or it and the byte at `x` (the end of what was read is a line's end).
fn first_start(near: &Around, p: u64, x: u64) -> u64 {
    if !pending(near.before(x, (x - p) as usize)) {
        return x;
    }
    let (cells, _) = decode(near.after(x - 1, 2), true);
    x - 1 + cells.first().map_or(1, |c| u64::from(c.len))
}

/// §7.3 rule 1 (D36): decode forward from the proof before `g` — a sync byte within W, else the
/// start `near` is anchored at (the line start within REACH, or a checkpoint) — and cut at
/// g − 1 when a lead is pending there, else at `g`. `None`: nothing proves a start (D28).
pub fn grid_cut(near: Around, g: u64) -> Option<u64> {
    let p = proof(&near, g)?;
    let bytes = near.before(g, (g - p) as usize);
    Some(g - u64::from(pending(bytes)))
}

/// §7.3 rule 2: the exact first start at or after `x` where something before it proves one (as
/// for [`grid_cut`]); else the window's first sync byte, a character start; else `None`.
pub fn restart_start(near: Around, x: u64) -> Option<u64> {
    if near.anchored && x <= near.at {
        return Some(near.at);
    }
    if let Some(p) = proof(&near, x) {
        return Some(first_start(&near, p, x));
    }
    let ahead = near.after(x, usize::MAX);
    let i = ahead.iter().position(|&b| is_sync(b))?;
    Some(x + i as u64)
}

/// §7.3 rule 4: decode forward from the proof before `left` (as for [`grid_cut`]) to the first
/// start at or after it; `None` with no proof — placeholder marks.
pub fn left_start(near: Around, left: u64) -> Option<u64> {
    if near.anchored && left <= near.at {
        return Some(near.at);
    }
    proof(&near, left).map(|p| first_start(&near, p, left))
}

/// The phase checkpoint's bit (D28): a lead pending at the end of `bytes`, which start at a
/// character start.
pub fn phase(bytes: &[u8]) -> Option<bool> {
    Some(pending(bytes))
}

/// (f) asks a sample of at least this many two-byte characters…
const F_FLOOR: usize = 32;
/// …for one ASCII character (printable or whitespace) to this many of them: Korean puts spaces
/// between words (0.35–0.40 of them in §7.2's Korean prose), Chinese prose next to none (0.012).
const F_PER_ASCII: usize = 10;

/// §7.2 step 3: D39's (a)–(f) all hold ([`criteria`]).
pub fn guess(sample: &[u8], truncated: bool) -> bool {
    criteria(sample, truncated).iter().all(|&c| c)
}

/// D39's (a)–(f) over the sample (`truncated`: the file goes on past it): (a) it is not valid
/// UTF-8, with `kind.rs`'s allowance (a character cut by the sample's end is no error when the
/// file goes on); (b) it decodes as CP949 with no error (a lead as its last byte is none either
/// when the file goes on); (c) it holds at least 4 Hangul syllables (U+AC00–U+D7A3); (d) they
/// are at least 3/4 of its decoded non-ASCII characters; (e) at least 3/4 of them are KS X
/// 1001's 2,350 (both bytes ≥ 0xA1); (f) with at least 32 two-byte characters, its ASCII
/// characters (printable and whitespace) are at least a tenth of them (DEC 2026-11-25: GBK's
/// `你好吗啊` is all KS X 1001 Hangul; below 32 the guess stays Korean-first).
pub(super) fn criteria(sample: &[u8], truncated: bool) -> [bool; 6] {
    let utf8 = match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => truncated && e.error_len().is_none(),
    };
    let body = &sample[..sample.len() - usize::from(truncated && pending(sample))];
    let (cells, _) = decode(body, true);
    let (mut bad, mut other, mut hangul, mut ks) = (0usize, 0usize, 0usize, 0usize);
    let (mut two, mut ascii) = (0usize, 0usize);
    let mut at = 0;
    for c in &cells {
        let bytes = &body[at..at + usize::from(c.len)];
        at += usize::from(c.len);
        bad += usize::from(c.ch == FFFD);
        other += usize::from(!c.ch.is_ascii());
        two += usize::from(c.len == 2);
        ascii += usize::from(
            c.len == 1 && matches!(c.ch, ' '..='~' | '\t' | '\n' | '\r' | '\x0B' | '\x0C'),
        );
        if ('\u{AC00}'..='\u{D7A3}').contains(&c.ch) {
            hangul += 1;
            ks += usize::from(bytes.iter().all(|&b| b >= 0xA1));
        }
    }
    [
        !utf8,
        bad == 0,
        hangul >= 4,
        4 * hangul >= 3 * other,
        4 * ks >= 3 * hangul,
        two < F_FLOOR || F_PER_ASCII * ascii >= two,
    ]
}
