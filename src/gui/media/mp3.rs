//! Whether a remote mp3 may seek fast (06 §7.4): its first frame carries a LAME `Info` tag, so it
//! is constant-bitrate and ffmpeg's `-fflags +fastseek` lands within one frame (26 ms at
//! 44.1 kHz). Every other mp3 seeks exactly, which reads the file up to the target: the media
//! window places cuts at the play position, so a play position seconds off would put the cuts in
//! the wrong place. sopen reads the tag itself, where ffmpeg's mp3 demuxer reads it
//! (`xing_offtbl` in libavformat/mp3dec.c), and anything it does not read as `Info` means an
//! exact seek: a misreading costs bytes, never precision.

use crate::remote::Cancelled;
use crate::stream::Bytes;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the mp3 `file` (read through a `play` reader) starts, after its ID3v2 tags, with a
/// frame whose LAME tag says `Info`.
/// - **ID3v2 tags:** while the bytes start `ID3` with major version 3 or 4, that tag is skipped:
///   10 bytes of header, the size in header bytes 6–9 (synchsafe: each byte below 0x80, 7 bits
///   each), and 10 more where a version-4 tag sets its footer flag (bit 4 of byte 5). Another
///   version, or a size byte of 0x80 or more, is an invalid tag.
/// - **The first frame** must start right after the tags (ffmpeg resyncs over junk; this does
///   not): an 11-bit sync, layer III, a valid bitrate and sample-rate index. Its tag is read
///   4 + 32 bytes in (MPEG-1 stereo), 4 + 17 (MPEG-1 mono, MPEG-2 and 2.5 stereo) or 4 + 9
///   (MPEG-2 and 2.5 mono), with no adjustment for a CRC, as ffmpeg reads it — and only if the
///   whole frame, by its header's bitrate, rate and padding, is in the file.
///
/// A read that fails or a file that ends first is `false`. `cancel` is the reads' (a stream's
/// reader looks at it while it waits): once it is set, the read ends and so does this, as
/// [`Cancelled`] — a stalled stream does not hold whoever stopped the open.
pub fn info_tag(file: &mut dyn Bytes, cancel: &AtomicBool) -> Result<bool, Cancelled> {
    let mut read = |off: u64, len: usize| -> Result<Option<Vec<u8>>, Cancelled> {
        let mut buf = vec![0u8; len];
        match file.read_at(off, &mut buf, cancel) {
            Ok(n) if n == len => Ok(Some(buf)),
            _ if cancel.load(Ordering::SeqCst) => Err(Cancelled),
            _ => Ok(None),
        }
    };
    let mut off = 0u64;
    let head = loop {
        let Some(head) = read(off, 10)? else {
            return Ok(false);
        };
        if &head[..3] != b"ID3" {
            break head;
        }
        if !matches!(head[3], 3 | 4) || head[6..10].iter().any(|b| *b >= 0x80) {
            return Ok(false);
        }
        let size = head[6..10]
            .iter()
            .fold(0u64, |n, b| (n << 7) | u64::from(*b));
        let footer = if head[3] == 4 && head[5] & 0x10 != 0 {
            10
        } else {
            0
        };
        off += 10 + size + footer;
    };
    let Some((side, len)) = frame(&head[..4]) else {
        return Ok(false);
    };
    // The tag is believed only in a whole frame: a file that ends inside it is cut short.
    Ok(read(off + len - 1, 1)?.is_some()
        && read(off + 4 + side, 4)?.is_some_and(|tag| tag == b"Info"))
}

/// Kbit/s by bitrate index, layer III: MPEG-1, then MPEG-2 and 2.5.
const BITRATES: [[u64; 15]; 2] = [
    [
        0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
    ],
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];
/// Hz by sample-rate index: MPEG-1, MPEG-2, MPEG-2.5.
const RATES: [[u64; 3]; 3] = [
    [44_100, 48_000, 32_000],
    [22_050, 24_000, 16_000],
    [11_025, 12_000, 8_000],
];

/// Of a valid MPEG audio layer III frame header: the side information's length, which is where
/// its Xing / Info tag starts after the 4 header bytes, and the whole frame's length in bytes;
/// `None` for anything else.
fn frame(h: &[u8]) -> Option<(u64, u64)> {
    let side = side_info(h)?;
    let version = (h[1] >> 3) & 3;
    let mpeg1 = version == 3;
    let kbps = BITRATES[usize::from(!mpeg1)][usize::from(h[2] >> 4)];
    let rate = RATES[match version {
        3 => 0,
        2 => 1,
        _ => 2,
    }][usize::from((h[2] >> 2) & 3)];
    let padding = u64::from((h[2] >> 1) & 1);
    // 1152 samples a frame for MPEG-1, 576 for MPEG-2 and 2.5: 144 or 72 bytes per kbit/s.
    let per = if mpeg1 { 144_000 } else { 72_000 };
    Some((side, per * kbps / rate + padding))
}

/// The side information's length of a valid MPEG audio layer III frame header, which is where
/// its Xing / Info tag starts after the 4 header bytes; `None` for anything else.
fn side_info(h: &[u8]) -> Option<u64> {
    if h[0] != 0xFF || h[1] & 0xE0 != 0xE0 {
        return None;
    }
    // 3: MPEG-1; 2: MPEG-2; 0: MPEG-2.5; 1 is reserved.
    let version = (h[1] >> 3) & 3;
    let layer = (h[1] >> 1) & 3;
    let bitrate = h[2] >> 4;
    let rate = (h[2] >> 2) & 3;
    if version == 1 || layer != 1 || bitrate == 0 || bitrate == 15 || rate == 3 {
        return None;
    }
    let mono = h[3] >> 6 == 3;
    Some(match (version == 3, mono) {
        (true, false) => 32,
        (true, true) | (false, false) => 17,
        (false, true) => 9,
    })
}

#[cfg(test)]
mod tests;
