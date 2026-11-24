//! The LAME tag ffmpeg's mp3 muxer writes into an mp3's first frame (the `Xing` / `Info`
//! frame), read, and mended where ffmpeg 9.1 gets it wrong.
//!
//! An mp3 says in that tag how many samples of encoder delay its decode starts with, and a
//! decoder skips them (and the decoder's own 529). libmp3lame primes with LAME's 576, so
//! ffmpeg's encoder starts its packets 576 + 529 = 1105 samples early. At the MPEG-2 and 2.5
//! rates (24 kHz and below) a frame is 576 samples: ffmpeg 9.1 (measured 2026-11-24; 9.0.2
//! writes 576) marks the 1105 over the first two packets, its muxer takes the first's only
//! and writes 576 − 529 = 47 — and a decoder that reads the tag (ffmpeg's 9.0.2 and 9.1,
//! measured) then gives 529 samples of priming before the sound: a Save, Export, cut or
//! separation of a 22.05 kHz mp3 started 24 ms late and ran 24 ms long. MP4, MOV and
//! Matroska carry the delay in their own boxes and are right on 9.1. [`mend_encoder_delay`]
//! writes the 576 back, in the file sopen has just made and before it is published.

use anyhow::{Context, Result};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::Path;

/// LAME's encoder delay, what a libmp3lame encode starts with.
const LAME_DELAY: u16 = 576;
/// What a decoder adds to the tag's delay (ffmpeg's `528 + 1`).
const DECODER_DELAY: u16 = 529;
/// The tag's CRC covers the frame's first 190 bytes (ffmpeg: `av_crc(…, xing_frame, 190)`).
const CRC_SPAN: usize = 190;

/// Where the tag is and what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    /// The first frame's offset in the file (after an ID3v2 tag), and its length.
    pub frame: usize,
    pub frame_len: usize,
    /// The `Xing` / `Info` identifier's offset in the file.
    pub xing: usize,
    /// MPEG-2 or 2.5: frames of 576 samples.
    pub low_rate: bool,
    pub delay: u16,
    pub padding: u16,
    /// ffmpeg's encoder string (`Lavc…`, `Lavf…`) or another encoder's.
    pub encoder: [u8; 9],
    /// The tag CRC as stored, and as the frame's bytes give it.
    pub crc: u16,
    pub crc_of_frame: u16,
}

impl Tag {
    /// Written by ffmpeg's muxer: its encoder string, and a CRC that is ffmpeg's of the frame.
    pub fn is_ffmpeg_s(&self) -> bool {
        self.encoder.starts_with(b"Lav") && self.crc == self.crc_of_frame
    }
}

/// CRC-16/ARC (ffmpeg's `AV_CRC_16_ANSI_LE`, from 0).
fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |mut crc, byte| {
        crc ^= u16::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
        crc
    })
}

/// The tag CRC of the frame at `head[frame..]` whose tag is at `xing`: over its first 190
/// bytes, zeros past the frame's end, the CRC's own field as zeros (ffmpeg writes it after).
fn tag_crc(head: &[u8], frame: usize, frame_len: usize, xing: usize) -> u16 {
    let mut span = [0u8; CRC_SPAN];
    let have = frame_len.min(CRC_SPAN).min(head.len() - frame);
    span[..have].copy_from_slice(&head[frame..frame + have]);
    let field = xing + 154 - frame;
    if field + 2 <= CRC_SPAN {
        span[field..field + 2].fill(0);
    }
    crc16(&span)
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

/// The length of the ID3v2 tag whose header starts `h` (10 bytes, `ID3`…): those 10, the size
/// in header bytes 6–9 (synchsafe: each byte below 0x80, 7 bits each), and 10 more where a
/// version-4 tag sets its footer flag (bit 4 of byte 5). `None` for another version than 3 or
/// 4, or a size byte of 0x80 or more: an invalid tag.
pub(crate) fn id3_len(h: &[u8]) -> Option<u64> {
    let h = h.get(..10)?;
    if !matches!(h[3], 3 | 4) || h[6..10].iter().any(|b| *b >= 0x80) {
        return None;
    }
    let size = h[6..10].iter().fold(0u64, |n, b| (n << 7) | u64::from(*b));
    let footer = if h[3] == 4 && h[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

/// Of a valid MPEG audio layer III frame header: the side information's length, which is where
/// its Xing / Info tag starts after the 4 header bytes, and the whole frame's length in bytes;
/// `None` for anything else.
pub(crate) fn frame(h: &[u8]) -> Option<(u64, u64)> {
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
    let h = h.get(..4)?;
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

/// The LAME tag in `head` (a file's first bytes: its ID3v2 tags and first frame), or `None`:
/// no layer III frame right after the tags, or none whose `Xing` / `Info` tag is whole in it.
/// Read where ffmpeg reads and writes it (`xing_offtbl`, no adjustment for a CRC).
pub fn tag(head: &[u8]) -> Option<Tag> {
    let mut at = 0u64;
    while head.get(at as usize..at as usize + 3)? == b"ID3" {
        at += id3_len(head.get(at as usize..)?)?;
    }
    let frame_at = usize::try_from(at).ok()?;
    let h = head.get(frame_at..frame_at + 4)?;
    let (side, len) = frame(h)?;
    let frame_len = usize::try_from(len).ok()?;
    let xing = frame_at + 4 + side as usize;
    // The tag is 156 bytes, inside the frame.
    if xing + 156 > frame_at + frame_len {
        return None;
    }
    let t = head.get(xing..xing + 156)?;
    if &t[..4] != b"Xing" && &t[..4] != b"Info" {
        return None;
    }
    let d = &t[141..144];
    Some(Tag {
        frame: frame_at,
        frame_len,
        xing,
        low_rate: (h[1] >> 3) & 3 != 3,
        delay: u16::from(d[0]) << 4 | u16::from(d[1] >> 4),
        padding: u16::from(d[1] & 0x0F) << 8 | u16::from(d[2]),
        encoder: t[120..129].try_into().unwrap(),
        crc: u16::from_be_bytes([t[154], t[155]]),
        crc_of_frame: tag_crc(head, frame_at, frame_len, xing),
    })
}

/// A file's first bytes, as far as [`tag`] reads: its ID3v2 tags and the frame after them (at
/// most 2881 bytes: MPEG-1 layer III at 320 kbit/s and 32 kHz, padded).
fn head_of(file: &mut std::fs::File) -> Result<Vec<u8>> {
    let mut at = 0u64;
    loop {
        let mut h = [0u8; 10];
        file.seek(SeekFrom::Start(at))?;
        if file.read(&mut h)? < h.len() || &h[..3] != b"ID3" {
            break;
        }
        match id3_len(&h) {
            Some(len) => at += len,
            None => break,
        }
    }
    file.seek(SeekFrom::Start(0))?;
    let mut head = Vec::new();
    file.by_ref().take(at + 4096).read_to_end(&mut head)?;
    Ok(head)
}

/// Mends the encoder delay of an mp3 ffmpeg has just written with libmp3lame: where its tag says
/// ffmpeg 9.1's 47 at an MPEG-2 or 2.5 rate, it is made LAME's 576 again and the tag's CRC
/// written for it. True when it was mended. Anything else — another delay, a tag that is not
/// ffmpeg's or whose CRC does not check, no tag — is left exactly as it is.
pub fn mend_encoder_delay(path: &Path) -> Result<bool> {
    let what = || format!("the LAME tag of {}", path.display());
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(what)?;
    let mut head = head_of(&mut file).with_context(what)?;
    let Some(t) = tag(&head) else {
        return Ok(false);
    };
    if !(t.low_rate && t.is_ffmpeg_s() && t.delay == LAME_DELAY - DECODER_DELAY) {
        return Ok(false);
    }
    let field = (u32::from(LAME_DELAY) << 12 | u32::from(t.padding)).to_be_bytes();
    head[t.xing + 141..t.xing + 144].copy_from_slice(&field[1..]);
    let crc = tag_crc(&head, t.frame, t.frame_len, t.xing).to_be_bytes();
    head[t.xing + 154..t.xing + 156].copy_from_slice(&crc);
    file.write_all_at(&head[t.xing + 141..t.xing + 156], (t.xing + 141) as u64)
        .with_context(what)?;
    file.sync_all().with_context(what)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg::{self, Tools};
    use std::process::Command;

    fn tools_or_skip() -> Option<Tools> {
        match ffmpeg::resolve() {
            Ok(t) => Some(t),
            Err(e) if std::env::var_os("SOPEN_REQUIRE_FFMPEG").is_some() => {
                panic!("SOPEN_REQUIRE_FFMPEG is set but {e}")
            }
            Err(e) => {
                eprintln!("skipping LAME tag test: {e}");
                None
            }
        }
    }

    /// Two seconds of a tone at `rate` on `channels`, by this ffmpeg's libmp3lame (`quality`:
    /// its options).
    fn encode(tools: &Tools, dst: &Path, rate: u32, channels: u32, quality: &[&str]) {
        let out = Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
            .args(["-f", "lavfi", "-i", &format!("sine=f=440:r={rate}")])
            .args(["-af", &format!("atrim=end_sample={}", 2 * rate)])
            .args(["-ac", &channels.to_string(), "-c:a", "libmp3lame"])
            .args(quality)
            .arg(dst)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn decoded(tools: &Tools, file: &Path) -> usize {
        let out = Command::new(&tools.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(file)
            .args(["-ac", "1", "-f", "s16le", "-"])
            .output()
            .unwrap();
        assert!(out.status.success());
        out.stdout.len() / 2
    }

    /// Every layout ffmpeg writes — MPEG-1, 2 and 2.5, mono and stereo, a VBR `Xing` frame
    /// and a CBR `Info` frame, some shorter than the CRC's 190 bytes — is found, and its tag
    /// CRC is ffmpeg's: what the mend writes, a player that checks the tag finds right.
    #[test]
    fn every_layout_s_tag_is_read_and_its_crc_is_ffmpeg_s() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        for rate in [44_100, 22_050, 8_000] {
            for channels in [1, 2] {
                for quality in [["-q:a", "2"], ["-b:a", "64k"]] {
                    let f = tmp
                        .path()
                        .join(format!("{rate}-{channels}-{}.mp3", quality[0]));
                    encode(&tools, &f, rate, channels, &quality);
                    let t = tag(&std::fs::read(&f).unwrap())
                        .unwrap_or_else(|| panic!("{}: no tag", f.display()));
                    assert!(t.is_ffmpeg_s(), "{}: {t:?}", f.display());
                    assert_eq!(t.low_rate, rate < 32_000, "{}", f.display());
                }
            }
        }
    }

    /// At 44.1 kHz no ffmpeg needs the mend: the file is left byte for byte. At 22.05 kHz the
    /// file is mended exactly where its tag says 47 (ffmpeg 9.1) and decodes to its samples;
    /// a tag that says 576 (ffmpeg 9.0 and before) is left byte for byte.
    #[test]
    fn the_mend_changes_only_a_47_and_then_the_file_decodes_to_its_samples() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let high = tmp.path().join("high.mp3");
        encode(&tools, &high, 44_100, 2, &["-q:a", "2"]);
        let before = std::fs::read(&high).unwrap();
        assert!(!mend_encoder_delay(&high).unwrap());
        assert!(
            std::fs::read(&high).unwrap() == before,
            "44.1 kHz: untouched"
        );
        let low = tmp.path().join("low.mp3");
        encode(&tools, &low, 22_050, 1, &["-q:a", "2"]);
        let before = std::fs::read(&low).unwrap();
        let said = tag(&before).unwrap().delay;
        let mended = mend_encoder_delay(&low).unwrap();
        let after = std::fs::read(&low).unwrap();
        assert_eq!(mended, said == 47, "a delay of {said}");
        if !mended {
            assert!(after == before, "22.05 kHz, delay {said}: untouched");
        }
        let t = tag(&after).unwrap();
        assert_eq!(t.delay, 576);
        assert!(t.is_ffmpeg_s(), "the CRC written for the mend: {t:?}");
        assert_eq!(decoded(&tools, &low), 44_100);
        // Only the delay and the CRC: 5 bytes, where they are.
        let changed: Vec<usize> = (0..before.len())
            .filter(|&i| before[i] != after[i])
            .collect();
        assert!(
            changed
                .iter()
                .all(|&i| (t.xing + 141..t.xing + 144).contains(&i)
                    || (t.xing + 154..t.xing + 156).contains(&i)),
            "{changed:?}"
        );
    }

    /// The mend itself on any ffmpeg: a 22.05 kHz file given ffmpeg 9.1's 47 (its CRC made to
    /// match) is mended to 576 and decodes to its samples; the same 47 under a CRC that does
    /// not check is not ffmpeg's tag, and is left alone.
    #[test]
    fn a_47_is_mended_only_under_ffmpeg_s_own_crc() {
        let Some(tools) = tools_or_skip() else { return };
        let tmp = tempfile::tempdir().unwrap();
        let low = tmp.path().join("low.mp3");
        encode(&tools, &low, 22_050, 2, &["-q:a", "2"]);
        let mut bytes = std::fs::read(&low).unwrap();
        let t = tag(&bytes).unwrap();
        let field = (47u32 << 12 | u32::from(t.padding)).to_be_bytes();
        bytes[t.xing + 141..t.xing + 144].copy_from_slice(&field[1..]);
        let crc = tag_crc(&bytes, t.frame, t.frame_len, t.xing).to_be_bytes();
        let mut wrong = bytes.clone();
        bytes[t.xing + 154..t.xing + 156].copy_from_slice(&crc);
        let given = tmp.path().join("given.mp3");
        std::fs::write(&given, &bytes).unwrap();
        assert_eq!(
            decoded(&tools, &given),
            44_100 + 529,
            "47: the priming kept"
        );
        assert!(mend_encoder_delay(&given).unwrap());
        assert_eq!(tag(&std::fs::read(&given).unwrap()).unwrap().delay, 576);
        assert_eq!(decoded(&tools, &given), 44_100);
        wrong[t.xing + 154..t.xing + 156]
            .copy_from_slice(&(u16::from_be_bytes(crc) ^ 1).to_be_bytes());
        let odd = tmp.path().join("odd.mp3");
        std::fs::write(&odd, &wrong).unwrap();
        assert!(!mend_encoder_delay(&odd).unwrap());
        assert!(std::fs::read(&odd).unwrap() == wrong, "untouched");
    }
}
