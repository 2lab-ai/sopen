//! Malformed files never panic or hang the open path: the metadata scanners, `Doc::decode`
//! (decode + metadata + the trial encode), the header read a streamed open starts with and the
//! picture it makes of a file's first bytes (06 §7.3) get every truncation of each fixture and
//! every single-byte flip of its first 256 bytes, and must return — Ok or Err — quickly. A
//! header never promises less memory than the decode then takes.

use super::probe::{self, Head, Probe};
use super::stream_tests::{baseline, exif, png as raw_png, progressive, with_segments};
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
        let decoded = Doc::decode(bytes);
        header_case(bytes, decoded.as_ref().ok())
    }));
    match result {
        Err(_) => return Err("panicked".to_string()),
        Ok(Err(why)) => return Err(why),
        Ok(Ok(())) => {}
    }
    let took = started.elapsed();
    if took > CASE_LIMIT {
        return Err(format!("took {took:?}"));
    }
    Ok(())
}

/// The header read and the picture of the first bytes on one malformed input. Where the header
/// read gives a header and the file does decode, the header told the truth: the size shown,
/// and at least the memory the decode took.
fn header_case(bytes: &[u8], decoded: Option<&Doc>) -> Result<(), String> {
    // Capped as a streamed open caps it, and whole.
    let capped = {
        let mut head = Head::new(bytes.len() as u64, probe::BLOCK, |off, buf: &mut [u8]| {
            let at = (off as usize).min(bytes.len());
            let n = buf.len().min(bytes.len() - at);
            buf[..n].copy_from_slice(&bytes[at..at + n]);
            Ok(n)
        });
        probe::probe(&mut head)
    };
    if capped.is_err() {
        return Err("the header read failed on a read that did not".to_string());
    }
    let Ok(Probe::Header(header)) = probe::probe(&mut Head::of(bytes)) else {
        return Ok(());
    };
    // A flipped size byte makes a frame of 65 000 rows: decoding that costs what its size
    // costs, three times over here, and is not what the limit is for. The pictures are made
    // of frames up to a megabyte.
    let cuts = [bytes.len() / 4, bytes.len() / 2, bytes.len()];
    for cut in cuts.into_iter().filter(|_| header.need <= 1 << 20) {
        if let Some(picture) = probe::partial(Cursor::new(&bytes[..cut]), &header, 64) {
            if picture.width() > 64 || picture.height() > 64 {
                return Err(format!(
                    "a picture of {:?} for a side of 64",
                    picture.dimensions()
                ));
            }
        }
    }
    if let Some(doc) = decoded {
        let took = doc.img.as_bytes().len() as u64;
        if header.upright() != doc.size() || header.need < took {
            return Err(format!(
                "the header says {:?} and {} bytes, the decode gave {:?} and {took}",
                header.upright(),
                header.need,
                doc.size()
            ));
        }
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

/// [`sweep`] for the header read and the picture of the first bytes alone, on fixtures the
/// metadata sweeps do not cover: `flips` bytes from the start are flipped, and the file is cut
/// at every length up to `cuts` and at 64 lengths spread over the rest.
fn header_sweep(name: &'static str, bytes: Vec<u8>, flips: usize, cuts: usize) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut failures = Vec::new();
        let mut case = |what: String, bytes: &[u8]| {
            let started = Instant::now();
            match catch_unwind(AssertUnwindSafe(|| header_case(bytes, None))) {
                Err(_) => failures.push(format!("{what}: panicked")),
                Ok(Err(why)) => failures.push(format!("{what}: {why}")),
                Ok(Ok(())) if started.elapsed() > CASE_LIMIT => {
                    failures.push(format!("{what}: took {:?}", started.elapsed()))
                }
                Ok(Ok(())) => {}
            }
        };
        case("intact".into(), &bytes);
        let spread = (cuts..bytes.len()).step_by((bytes.len() / 64).max(1));
        for n in (0..cuts.min(bytes.len())).chain(spread) {
            case(format!("truncated to {n}"), &bytes[..n]);
        }
        for i in 0..bytes.len().min(flips) {
            let mut flipped = bytes.clone();
            flipped[i] ^= 0xFF;
            case(format!("byte {i} flipped"), &flipped);
        }
        let _ = tx.send(failures);
    });
    let failures = rx
        .recv_timeout(SWEEP_LIMIT)
        .unwrap_or_else(|_| panic!("{name}: sweep did not finish in {SWEEP_LIMIT:?} (a hang)"));
    assert!(
        failures.is_empty(),
        "{name}: {} malformed cases failed, first: {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
}

fn encoded(format: ImageFormat) -> Vec<u8> {
    encode(&small(), format, None, None).unwrap().bytes
}

#[test]
fn malformed_headers_never_panic_the_header_read() {
    header_sweep("gif", encoded(ImageFormat::Gif), 256, usize::MAX);
    header_sweep("bmp", encoded(ImageFormat::Bmp), 256, usize::MAX);
    header_sweep("progressive jpeg", progressive(64, 48, 6), 512, 1024);
    header_sweep("interlaced png", raw_png(40, 30, true), 256, 512);
    // A JPEG whose Exif carries a thumbnail: every byte of the Exif's own structure flipped.
    let thumb = baseline(32, 24);
    let with_thumb = with_segments(&baseline(48, 32), &[(0xE1, exif(&thumb, 6, 2000))]);
    assert!(matches!(
        probe::probe(&mut Head::of(&with_thumb)),
        Ok(Probe::Header(h)) if h.thumb.is_some()
    ));
    header_sweep("jpeg with a thumbnail", with_thumb, 1024, 1024);
}

/// An Exif block is as untrusted as the file: offsets and counts that point anywhere give no
/// thumbnail, and a thumbnail whose own header claims a huge picture is not decoded.
#[test]
fn a_hostile_exif_gives_no_thumbnail_and_costs_nothing() {
    let thumb_of = |exif: Vec<u8>| -> Option<(u32, u32)> {
        let file = with_segments(&baseline(48, 32), &[(0xE1, exif)]);
        let started = Instant::now();
        let found = std::thread::spawn(move || {
            let found = probe::probe(&mut Head::of(&file)).unwrap();
            (found, probe::largest_reserved())
        })
        .join()
        .expect("the header read panicked");
        assert!(started.elapsed() < CASE_LIMIT);
        match found {
            (Probe::Header(header), reserved) => {
                assert_eq!(
                    (header.width, header.height),
                    (48, 32),
                    "the frame stays right"
                );
                assert!(reserved <= 1 << 20, "{reserved} reserved for a thumbnail");
                header.thumb.map(|t| t.dimensions())
            }
            (other, _) => panic!("{other:?}"),
        }
    };
    let good = exif(&baseline(32, 24), 1, 2000);
    assert_eq!(thumb_of(good.clone()), Some((32, 24)));
    // The fields of the block, by where `exif` puts them (after `Exif\0\0`).
    let at = |field: usize| 6 + field;
    let with = |field: usize, value: &[u8]| {
        let mut bad = good.clone();
        bad[at(field)..at(field) + value.len()].copy_from_slice(value);
        bad
    };
    for (what, bad) in [
        ("IFD0 far past the end", with(4, &[0xFF, 0xFF, 0xFF, 0xF0])),
        ("IFD0 with 65535 entries", with(8, &[0xFF, 0xFF])),
        ("IFD1 far past the end", with(22, &[0x7F, 0xFF, 0xFF, 0xFF])),
        ("IFD1 with 65535 entries", with(26, &[0xFF, 0xFF])),
        (
            "the thumbnail far past the end",
            with(36, &[0xFF, 0xFF, 0xFF, 0xFF]),
        ),
        (
            "a thumbnail longer than the block",
            with(48, &[0xFF, 0xFF, 0xFF, 0xFF]),
        ),
        ("a thumbnail of no length", with(48, &[0, 0, 0, 0])),
        ("not a TIFF", with(0, b"XX")),
        ("a thumbnail that is no JPEG", with(56, &[0x89, 0x50])),
    ] {
        assert_eq!(thumb_of(bad), None, "{what}");
    }
    // A thumbnail that says it is 8000×8000 — 192 MB, were it decoded — over a real one's
    // data: its own frame is read first, and a frame that large is no thumbnail.
    let real = baseline(32, 24);
    let frame = real.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    let mut liar = real.clone();
    liar[frame + 5..frame + 9].copy_from_slice(&[0x1F, 0x40, 0x1F, 0x40]);
    assert_eq!(thumb_of(exif(&liar, 1, 2000)), None);
    // An Exif in the thumbnail, with a thumbnail of its own, and so on down: the frame is
    // found and nothing nested is decoded.
    let mut nested = baseline(8, 8);
    for _ in 0..40 {
        let len = nested.len() + 100;
        nested = with_segments(&baseline(8, 8), &[(0xE1, exif(&nested, 1, len))]);
    }
    assert_eq!(thumb_of(exif(&nested, 1, 60_000)), Some((8, 8)));
    // A block cut anywhere.
    for cut in 0..120 {
        thumb_of(good[..cut.min(good.len())].to_vec());
    }
}
