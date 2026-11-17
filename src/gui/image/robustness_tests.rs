//! Malformed files never panic or hang the open path: the metadata scanners and
//! `Doc::decode` (decode + metadata + the trial encode) get every truncation of each fixture
//! and every single-byte flip of its first 256 bytes, and must return — Ok or Err — quickly.

use super::*;
use image::Rgb;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

/// Longest a single malformed case may take.
const CASE_LIMIT: Duration = Duration::from_secs(2);
/// Longest a whole fixture's sweep may take before the test fails as a hang.
const SWEEP_LIMIT: Duration = Duration::from_secs(120);

fn small() -> DynamicImage {
    DynamicImage::ImageRgb8(ImageBuffer::from_fn(12, 8, |x, y| {
        Rgb([(x * 20) as u8, (y * 30) as u8, 90])
    }))
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

const XMP: &[u8] = b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>";
const EXIF: &[u8] = b"MM\x00\x2a\x00\x00\x00\x08\x00\x00\x00\x00\x00\x00";

/// PNG with an ICC profile, Exif, an iTXt XMP chunk and a tEXt chunk.
fn png() -> Vec<u8> {
    let base = encode(
        &small(),
        ImageFormat::Png,
        Some(vec![7; 64]),
        Some(EXIF.to_vec()),
    )
    .unwrap()
    .bytes;
    let iend = base.len() - 12;
    let mut out = base[..iend].to_vec();
    let xmp = [b"XML:com.adobe.xmp\0\0\0\0\0".as_slice(), XMP].concat();
    for (kind, data) in [(b"iTXt", xmp.as_slice()), (b"tEXt", b"Title\0shot")] {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let typed = [kind.as_slice(), data].concat();
        out.extend_from_slice(&typed);
        out.extend_from_slice(&crc32(&typed).to_be_bytes());
    }
    out.extend_from_slice(&base[iend..]);
    out
}

/// JPEG with Exif and ICC (from the encoder) plus APP1 XMP, APP13 IPTC and a comment.
fn jpeg() -> Vec<u8> {
    let base = encode(
        &small(),
        ImageFormat::Jpeg,
        Some(vec![7; 64]),
        Some(EXIF.to_vec()),
    )
    .unwrap()
    .bytes;
    let mut out = base[..2].to_vec();
    let segments: [(u8, Vec<u8>); 3] = [
        (
            0xE1,
            [b"http://ns.adobe.com/xap/1.0/\0".as_slice(), XMP].concat(),
        ),
        (0xED, b"Photoshop 3.0\08BIM\x04\x04\0\0\0\0\0\0".to_vec()),
        (0xFE, b"a comment".to_vec()),
    ];
    for (marker, data) in segments {
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&data);
    }
    out.extend_from_slice(&base[2..]);
    out
}

/// Lossless WebP with an ICC profile, Exif and an `XMP ` chunk (RIFF size fixed up).
fn webp() -> Vec<u8> {
    let mut out = encode(
        &small(),
        ImageFormat::WebP,
        Some(vec![7; 64]),
        Some(EXIF.to_vec()),
    )
    .unwrap()
    .bytes;
    out.extend_from_slice(b"XMP ");
    out.extend_from_slice(&(XMP.len() as u32).to_le_bytes());
    out.extend_from_slice(XMP);
    if XMP.len() % 2 == 1 {
        out.push(0);
    }
    let riff = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    out
}

/// A real TIFF from the encoder, then a header-only TIFF whose first page carries XMP (700),
/// IPTC (33723) and a description (270) — not decodable, scanned all the same.
fn tiffs() -> [Vec<u8>; 2] {
    let real = encode(&small(), ImageFormat::Tiff, None, None)
        .unwrap()
        .bytes;
    let mut tagged = b"II*\0\x08\0\0\0\x03\0".to_vec();
    for tag in [270u16, 700, 33723] {
        tagged.extend_from_slice(&tag.to_le_bytes());
        tagged.extend_from_slice(&[1, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
    }
    tagged.extend_from_slice(&[0, 0, 0, 0]);
    [real, tagged]
}

/// Runs scanner + decode on one malformed input; `Err(reason)` on a panic or a slow case.
fn one(bytes: &[u8], format: ImageFormat) -> Result<(), String> {
    let started = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _ = foreign_metadata(bytes, format);
        let _ = tiff_pages(bytes, 1000);
        let _ = Doc::decode(bytes);
    }));
    if result.is_err() {
        return Err("panicked".to_string());
    }
    let took = started.elapsed();
    if took > CASE_LIMIT {
        return Err(format!("took {took:?}"));
    }
    Ok(())
}

/// The intact fixture, every truncation and every flip of the first 256 bytes — all of it on
/// a thread with a deadline, so a scanner that hangs on the intact file fails too.
fn sweep(name: &'static str, bytes: Vec<u8>, format: ImageFormat) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // The decoders' own panic messages would flood the output; the result says enough.
        let mut failures = Vec::new();
        let scanned = catch_unwind(AssertUnwindSafe(|| foreign_metadata(&bytes, format)));
        let carries = match scanned {
            Ok(found) => !found.is_empty(),
            Err(_) => {
                failures.push("intact: panicked".to_string());
                true
            }
        };
        if let Err(why) = one(&bytes, format) {
            failures.push(format!("intact: {why}"));
        }
        if !carries && format != ImageFormat::Tiff {
            failures.push("the intact fixture carries no metadata to scan".to_string());
        }
        for n in 0..bytes.len() {
            if let Err(why) = one(&bytes[..n], format) {
                failures.push(format!("truncated to {n}: {why}"));
            }
        }
        for i in 0..bytes.len().min(256) {
            let mut flipped = bytes.clone();
            flipped[i] ^= 0xFF;
            if let Err(why) = one(&flipped, format) {
                failures.push(format!("byte {i} flipped: {why}"));
            }
        }
        let _ = tx.send((bytes.len(), failures));
    });
    let (len, failures) = rx
        .recv_timeout(SWEEP_LIMIT)
        .unwrap_or_else(|_| panic!("{name}: sweep did not finish in {SWEEP_LIMIT:?} (a hang)"));
    assert!(
        failures.is_empty(),
        "{name} ({len} bytes): {} malformed cases failed, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

#[test]
fn malformed_png_never_panics() {
    sweep("png", png(), ImageFormat::Png);
}

#[test]
fn malformed_jpeg_never_panics() {
    sweep("jpeg", jpeg(), ImageFormat::Jpeg);
}

#[test]
fn malformed_webp_never_panics() {
    sweep("webp", webp(), ImageFormat::WebP);
}

#[test]
fn malformed_tiff_never_panics() {
    let [real, tagged] = tiffs();
    assert_eq!(
        foreign_metadata(&tagged, ImageFormat::Tiff),
        ["XMP", "IPTC", "1 text tag"]
    );
    sweep("tiff", real, ImageFormat::Tiff);
    sweep("tiff-tags", tagged, ImageFormat::Tiff);
}
