//! The encode side (`.prd/12-editor.md` §7.2, ED-enc): typed and pasted text as bytes of the
//! file's encoding, which the editor's add buffer holds as they are (12 §7.1). One encoder per
//! encoding of 08 §7.1, each the inverse of its decoder: what one writes, [`Encoding::decode`]
//! reads back as the same characters (`tests.rs` holds every scalar value and every byte or
//! code of the small repertoires to that).
//!
//! - UTF-8 as it is; UTF-16LE / BE by `encode_utf16`, a character above U+FFFF as its pair;
//!   UTF-32 by `u32`. encoding_rs writes no UTF-16 (12 §7.2), so these are sopen's own, as std
//!   decodes them (08 §7.4).
//! - UCS-2 refuses characters above U+FFFF, ASCII above U+007F, Latin-1 above U+00FF.
//! - CP437: U+0000–U+007F as the byte, as its decoder reads that half — the skin's glyphs for
//!   the control bytes (☺, ⌂) are drawn, never decoded, so they are refused — and the inverse
//!   of [`chrome::cp437`] above.
//! - CP949: encoding_rs's EUC-KR encoder (WHATWG EUC-KR, windows-949: KS X 1001 and UHC)
//!   without replacement, so a character it reports unmappable is refused, never written as
//!   the `&#…;` its own `encode` would put there.
//!
//! A character the encoding cannot hold is refused, never replaced (12 §7.2): nothing of the
//! text is written, and [`Refused`] names the first such character. CR and LF are characters
//! like any other, so a line end is written as the encoding's own CR and LF units
//! ([`Encoding::cr`], [`Encoding::lf`]). No BOM is written: a file's BOM stays where it was
//! read, and no edit reaches it (12 §7.2).

use super::Encoding;
use crate::gui::chrome;
use encoding_rs::{EncoderResult, EUC_KR};
use std::sync::OnceLock;

/// A character the encoding cannot hold, the first in the text: nothing of the text was
/// written. `at`: its byte offset in the text (as `Utf8Error::valid_up_to` counts), so
/// `text[..at]` is what the encoding holds of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    pub ch: char,
    pub at: usize,
    pub encoding: Encoding,
}

/// `한 cannot be written in Latin-1`: 12 §7.2's status line without its tail, which says what
/// was not done (typed, pasted).
impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} cannot be written in {}",
            self.ch,
            self.encoding.label()
        )
    }
}

impl std::error::Error for Refused {}

impl Encoding {
    /// 12 §7.2's encode side: `text` as this encoding's bytes, with no BOM, or the first
    /// character it cannot hold.
    pub fn encode(self, text: &str) -> Result<Vec<u8>, Refused> {
        let be = self.big_endian();
        match self {
            Encoding::Utf8 => Ok(text.as_bytes().to_vec()),
            Encoding::Utf16Le | Encoding::Utf16Be => {
                Ok(text.encode_utf16().flat_map(|u| two(u, be)).collect())
            }
            Encoding::Ucs2Le | Encoding::Ucs2Be => self.each(text, |c| {
                u16::try_from(u32::from(c)).ok().map(|u| two(u, be))
            }),
            Encoding::Utf32Le | Encoding::Utf32Be => {
                Ok(text.chars().flat_map(|c| four(u32::from(c), be)).collect())
            }
            Encoding::Ascii => self.each(text, |c| {
                u8::try_from(c).ok().filter(u8::is_ascii).map(|b| [b])
            }),
            Encoding::Latin1 => self.each(text, |c| u8::try_from(c).ok().map(|b| [b])),
            Encoding::Cp437 => self.each(text, |c| cp437_byte(c).map(|b| [b])),
            Encoding::Cp949 => cp949(text),
        }
    }

    /// `text` character by character, each written as `put` gives it; the first it gives
    /// none for is refused.
    fn each<const N: usize>(
        self,
        text: &str,
        put: impl Fn(char) -> Option<[u8; N]>,
    ) -> Result<Vec<u8>, Refused> {
        let mut out = Vec::with_capacity(text.len() * N);
        for (at, ch) in text.char_indices() {
            let Some(bytes) = put(ch) else {
                return Err(Refused {
                    ch,
                    at,
                    encoding: self,
                });
            };
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }
}

/// A 2-byte unit in the encoding's byte order.
fn two(u: u16, be: bool) -> [u8; 2] {
    if be {
        u.to_be_bytes()
    } else {
        u.to_le_bytes()
    }
}

/// A 4-byte unit in the encoding's byte order.
fn four(u: u32, be: bool) -> [u8; 4] {
    if be {
        u.to_be_bytes()
    } else {
        u.to_le_bytes()
    }
}

/// CP437's byte for `c`: ASCII as itself, else the byte of the upper half whose glyph it is.
fn cp437_byte(c: char) -> Option<u8> {
    if c.is_ascii() {
        return Some(c as u8);
    }
    // The upper half by glyph, sorted: 128 distinct characters, none ASCII (`tests.rs`).
    static HIGH: OnceLock<[(char, u8); 128]> = OnceLock::new();
    let high = HIGH.get_or_init(|| {
        let mut table = std::array::from_fn(|i| {
            let b = 0x80 + i as u8;
            (chrome::cp437(b), b)
        });
        table.sort_unstable();
        table
    });
    let i = high.binary_search_by_key(&c, |&(g, _)| g).ok()?;
    Some(high[i].1)
}

/// CP949 by encoding_rs's EUC-KR encoder, without replacement. `read` counts the unmappable
/// character too (encoding_rs's own replacing loop resumes after it), so it starts at `read`
/// less its length.
fn cp949(text: &str) -> Result<Vec<u8>, Refused> {
    let mut encoder = EUC_KR.new_encoder();
    let room = encoder.max_buffer_length_from_utf8_without_replacement(text.len());
    let mut out = Vec::with_capacity(room.unwrap_or(text.len()));
    let mut read = 0;
    loop {
        let (result, n) =
            encoder.encode_from_utf8_to_vec_without_replacement(&text[read..], &mut out, true);
        read += n;
        match result {
            EncoderResult::InputEmpty => return Ok(out),
            // Only when the first reserve came short: room for the rest, at least a character.
            EncoderResult::OutputFull => out.reserve((2 * (text.len() - read)).max(16)),
            EncoderResult::Unmappable(ch) => {
                return Err(Refused {
                    ch,
                    at: read - ch.len_utf8(),
                    encoding: Encoding::Cp949,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests;
