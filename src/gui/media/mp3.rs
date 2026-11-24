//! Whether a remote mp3 may seek fast (06 §7.4): its first frame carries a LAME `Info` tag, so it
//! is constant-bitrate and ffmpeg's `-fflags +fastseek` lands within one frame (26 ms at
//! 44.1 kHz). Every other mp3 seeks exactly, which reads the file up to the target: the media
//! window places cuts at the play position, so a play position seconds off would put the cuts in
//! the wrong place. sopen reads the tag itself, where ffmpeg's mp3 demuxer reads it
//! (`xing_offtbl` in libavformat/mp3dec.c), and anything it does not read as `Info` means an
//! exact seek: a misreading costs bytes, never precision.

use crate::ffmpeg::mp3::{frame, id3_len};
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
        let Some(len) = id3_len(&head) else {
            return Ok(false);
        };
        off += len;
    };
    let Some((side, len)) = frame(&head[..4]) else {
        return Ok(false);
    };
    // The tag is believed only in a whole frame: a file that ends inside it is cut short.
    Ok(read(off + len - 1, 1)?.is_some()
        && read(off + 4 + side, 4)?.is_some_and(|tag| tag == b"Info"))
}

#[cfg(test)]
mod tests;
