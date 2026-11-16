//! The image window (spec §6): fit/zoom/pan, a rectangle in image pixels, Crop, Pixelize,
//! Undo/Redo, Save (backup first, conflict-checked overwrite) and Save copy (a new sibling).
//!
//! [`Doc`] is the edit model (decode, ops, history, encode) and [`ImageFile`] the write side;
//! both are window-free, so the tests and the `SOPEN_AUTOSAVE` hook run the exact code the
//! window's buttons run.

use super::{Hooks, Map, Op, Origin, Outcome, Selection, Status};
use crate::remote::WriteOutcome;
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, TextureHandle, Vec2};
use image::{
    AnimationDecoder, DynamicImage, ImageBuffer, ImageDecoder, ImageEncoder, ImageFormat,
    ImageReader, Pixel, Primitive,
};
use num_traits::ToPrimitive;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Default pixelize block, in image pixels.
pub const DEFAULT_BLOCK: u32 = 16;
/// Undo history bound: whichever of these is hit first drops the oldest step.
const HISTORY_STEPS: usize = 32;
const HISTORY_BYTES: usize = 1 << 30;

// ---------------------------------------------------------------------------------------------
// The edit model.

pub struct Doc {
    pub img: DynamicImage,
    pub format: ImageFormat,
    icc: Option<Vec<u8>>,
    /// Exif with the orientation reset: the pixels are stored upright (orientation applied on
    /// open), so writing the original tag back would rotate the image a second time.
    exif: Option<Vec<u8>>,
    /// Why the image cannot be edited or saved (animated images), if so.
    pub read_only: Option<String>,
    /// Metadata in the file that Save does not carry over (`XMP`, `2 text chunks`, …).
    pub foreign: Vec<String>,
    /// Metadata that could not be read or would be rejected by the encoder, one sentence each
    /// (`ICC profile not kept: …`).
    pub lost: Vec<String>,
    undo: Vec<DynamicImage>,
    redo: Vec<DynamicImage>,
    /// Bumped by every change; `saved_rev` is the revision in the original file, `copied_rev`
    /// the one last written by Save copy.
    rev: u64,
    saved_rev: u64,
    copied_rev: u64,
}

fn animated(bytes: &[u8], format: ImageFormat) -> bool {
    match format {
        ImageFormat::Gif => image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
            .map(|d| d.into_frames().take(2).count() > 1)
            .unwrap_or(false),
        ImageFormat::Png => image::codecs::png::PngDecoder::new(Cursor::new(bytes))
            .and_then(|d| d.is_apng())
            .unwrap_or(false),
        ImageFormat::WebP => image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))
            .map(|d| d.has_animation())
            .unwrap_or(false),
        _ => false,
    }
}

/// Pages (IFDs) in a TIFF, counted up to `limit`: classic and BigTIFF, either byte order.
/// A chain that runs off the end of the data or loops stops the count there.
pub fn tiff_pages(bytes: &[u8], limit: usize) -> usize {
    let big = match bytes.get(..4) {
        Some(b"II*\0") | Some(b"MM\0*") => false,
        Some(b"II+\0") | Some(b"MM\0+") => true,
        _ => return 0,
    };
    let le = bytes[0] == b'I';
    let uint = |at: usize, len: usize| -> Option<u64> {
        let b = bytes.get(at..at.checked_add(len)?)?;
        Some(b.iter().enumerate().fold(0u64, |acc, (i, &v)| {
            let shift = if le { 8 * i } else { 8 * (len - 1 - i) };
            acc | (u64::from(v) << shift)
        }))
    };
    let (mut ifd, count_len, entry_len, next_len) = if big {
        (uint(8, 8), 8, 20, 8)
    } else {
        (uint(4, 4), 2, 12, 4)
    };
    let mut seen = Vec::new();
    while let Some(at) = ifd
        .filter(|&o| o != 0)
        .and_then(|o| usize::try_from(o).ok())
    {
        if seen.len() >= limit || seen.contains(&at) {
            break;
        }
        let Some(n) = uint(at, count_len).and_then(|n| usize::try_from(n).ok()) else {
            break;
        };
        seen.push(at);
        let next_at = n
            .checked_mul(entry_len)
            .and_then(|e| e.checked_add(at + count_len));
        ifd = next_at.and_then(|i| uint(i, next_len));
    }
    seen.len()
}

fn be16(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn be32(b: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?) as usize)
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// PNG: `XMP` (iTXt `XML:com.adobe.xmp`) and text chunks (tEXt / zTXt / other iTXt).
fn png_foreign(b: &[u8]) -> Vec<String> {
    let (mut xmp, mut text) = (false, 0);
    let mut i = 8;
    while let (Some(len), Some(kind)) = (be32(b, i), b.get(i + 4..i + 8)) {
        let Some(data) = b.get(i + 8..(i + 8).saturating_add(len)) else {
            break;
        };
        match kind {
            b"iTXt" if data.starts_with(b"XML:com.adobe.xmp\0") => xmp = true,
            b"tEXt" | b"zTXt" | b"iTXt" => text += 1,
            b"IEND" => break,
            _ => {}
        }
        i += 12 + len;
    }
    let mut out = Vec::new();
    if xmp {
        out.push("XMP".to_string());
    }
    if text > 0 {
        out.push(plural(text, "text chunk"));
    }
    out
}

/// JPEG segments before the image data: XMP (APP1), IPTC (APP13), comments (COM).
fn jpeg_foreign(b: &[u8]) -> Vec<String> {
    let (mut xmp, mut iptc, mut comment) = (false, false, false);
    let mut i = 2;
    while b.get(i) == Some(&0xFF) {
        let Some(&marker) = b.get(i + 1) else { break };
        match marker {
            0xFF => {
                i += 1;
                continue;
            }
            0x01 | 0xD0..=0xD8 => {
                i += 2;
                continue;
            }
            0xD9 | 0xDA => break,
            _ => {}
        }
        let Some(len) = be16(b, i + 2) else { break };
        let data = b.get(i + 4..i + 2 + len).unwrap_or_default();
        match marker {
            0xE1 if data.starts_with(b"http://ns.adobe.com/xap/1.0/\0")
                || data.starts_with(b"http://ns.adobe.com/xmp/extension/\0") =>
            {
                xmp = true
            }
            0xED => iptc = true,
            0xFE => comment = true,
            _ => {}
        }
        i += 2 + len;
    }
    [(xmp, "XMP"), (iptc, "IPTC"), (comment, "comment")]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// TIFF first-page tags sopen's encoder does not write: XMP (700), IPTC (33723), and the
/// description / artist / copyright text tags.
fn tiff_foreign(b: &[u8]) -> Vec<String> {
    let le = b.starts_with(b"II");
    let uint = |at: usize, len: usize| -> Option<usize> {
        let s = b.get(at..at.checked_add(len)?)?;
        Some(s.iter().enumerate().fold(0usize, |acc, (i, &v)| {
            let shift = if le { 8 * i } else { 8 * (len - 1 - i) };
            acc | (usize::from(v) << shift)
        }))
    };
    let big = matches!(b.get(..4), Some(b"II+\0") | Some(b"MM\0+"));
    let (ifd, count_len, entry_len) = if big {
        (uint(8, 8), 8, 20)
    } else {
        (uint(4, 4), 2, 12)
    };
    let mut tags = Vec::new();
    if let Some(at) = ifd {
        let n = uint(at, count_len).unwrap_or(0).min(4096);
        for k in 0..n {
            match uint(at + count_len + k * entry_len, 2) {
                Some(tag) => tags.push(tag),
                None => break,
            }
        }
    }
    let mut out = Vec::new();
    if tags.contains(&700) {
        out.push("XMP".to_string());
    }
    if tags.contains(&33723) {
        out.push("IPTC".to_string());
    }
    let text = tags
        .iter()
        .filter(|t| matches!(t, 270 | 315 | 33432))
        .count();
    if text > 0 {
        out.push(plural(text, "text tag"));
    }
    out
}

/// WebP RIFF chunks: XMP (`XMP `).
fn webp_foreign(b: &[u8]) -> Vec<String> {
    let mut i = 12;
    while let Some(kind) = b.get(i..i + 4) {
        let Some(len) = b
            .get(i + 4..i + 8)
            .map(|l| u32::from_le_bytes(l.try_into().unwrap()) as usize)
        else {
            break;
        };
        if kind == b"XMP " {
            return vec!["XMP".to_string()];
        }
        i += 8 + len + (len & 1);
    }
    Vec::new()
}

/// Metadata in the file that Save does not carry over. ICC profiles and Exif are carried
/// (where the format holds them); this names the rest, so nothing is lost silently.
pub fn foreign_metadata(bytes: &[u8], format: ImageFormat) -> Vec<String> {
    match format {
        ImageFormat::Png => png_foreign(bytes),
        ImageFormat::Jpeg => jpeg_foreign(bytes),
        ImageFormat::Tiff => tiff_foreign(bytes),
        ImageFormat::WebP => webp_foreign(bytes),
        _ => Vec::new(),
    }
}

fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::WebP => "WebP",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Gif => "GIF",
        _ => "image",
    }
}

impl Doc {
    /// Decodes `bytes` (format by content, not by name), applies the Exif orientation and keeps
    /// the ICC profile and Exif for the save.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .context("cannot read the image")?;
        let format = reader
            .format()
            .context("not an image format sopen can read")?;
        if !matches!(
            format,
            ImageFormat::Png
                | ImageFormat::Jpeg
                | ImageFormat::WebP
                | ImageFormat::Bmp
                | ImageFormat::Tiff
                | ImageFormat::Gif
        ) {
            bail!("{format:?} images are not supported");
        }
        let mut decoder = reader.into_decoder().context("cannot decode the image")?;
        let mut lost = Vec::new();
        let icc = decoder.icc_profile().unwrap_or_else(|e| {
            lost.push(format!("ICC profile not kept: cannot read it ({e})"));
            None
        });
        let mut exif = decoder.exif_metadata().unwrap_or_else(|e| {
            lost.push(format!("Exif not kept: cannot read it ({e})"));
            None
        });
        let orientation = decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut img = DynamicImage::from_decoder(decoder).context("cannot decode the image")?;
        img.apply_orientation(orientation);
        if let Some(chunk) = exif.as_mut() {
            let reset = image::metadata::Orientation::remove_from_exif_chunk(chunk);
            if reset.is_none() && orientation != image::metadata::Orientation::NoTransforms {
                // Writing it back would rotate the (already upright) pixels a second time.
                exif = None;
                lost.push("Exif not kept: its orientation tag could not be reset".to_string());
            }
        }
        // What this format's encoder would refuse to carry, learnt on a 1×1 image now — before
        // the owner edits anything.
        let probe_img = DynamicImage::ImageRgba8(image::RgbaImage::new(1, 1));
        if let Ok(trial) = encode(&probe_img, format, icc.clone(), exif.clone()) {
            lost.extend(trial.not_kept);
        }
        let foreign = foreign_metadata(bytes, format);
        let pages = if format == ImageFormat::Tiff {
            tiff_pages(bytes, 1000)
        } else {
            1
        };
        let read_only = if animated(bytes, format) {
            Some(format!(
                "animated {}, first frame shown; saving would drop the animation",
                format_name(format)
            ))
        } else if pages > 1 {
            Some(format!(
                "multi-page TIFF ({pages} pages), first page shown; saving would drop the others"
            ))
        } else {
            None
        };
        Ok(Self {
            img,
            format,
            icc,
            exif,
            read_only,
            foreign,
            lost,
            undo: Vec::new(),
            redo: Vec::new(),
            rev: 0,
            saved_rev: 0,
            copied_rev: 0,
        })
    }

    /// "Save will not keep: XMP, 2 text chunks; ICC profile not kept: …", or `None` when Save
    /// keeps everything sopen read. Shown when the image opens.
    pub fn not_kept_line(&self) -> Option<String> {
        let mut parts = Vec::new();
        if !self.foreign.is_empty() {
            parts.push(format!("Save will not keep: {}", self.foreign.join(", ")));
        }
        parts.extend(self.lost.iter().cloned());
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// An encoder for the current image that runs anywhere (the window runs it on a worker
    /// thread): same format and metadata, and the notes about what the result does not keep.
    pub fn encoder(&self) -> impl FnOnce() -> Result<Encoded> + Send + 'static {
        let (img, format) = (self.img.clone(), self.format);
        let (icc, exif) = (self.icc.clone(), self.exif.clone());
        let mut carried = Vec::new();
        if !self.foreign.is_empty() {
            carried.push(format!("not kept: {}", self.foreign.join(", ")));
        }
        // Read failures; encoder refusals come from the real encode below.
        carried.extend(
            self.lost
                .iter()
                .filter(|l| l.contains("cannot read") || l.contains("could not be reset"))
                .cloned(),
        );
        let read_only = self.read_only.clone();
        move || {
            if let Some(reason) = read_only {
                bail!("read-only — {reason}");
            }
            let mut out = encode(&img, format, icc, exif)?;
            carried.append(&mut out.not_kept);
            out.not_kept = carried;
            Ok(out)
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.img.width(), self.img.height())
    }

    /// The state differs from the original file (what Save would write).
    pub fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    /// The state is written nowhere — neither the original nor a Save copy (the close prompt).
    pub fn unsaved(&self) -> bool {
        self.dirty() && self.rev != self.copied_rev
    }

    pub fn mark_copied(&mut self, rev: u64) {
        self.copied_rev = rev;
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn mark_saved(&mut self, rev: u64) {
        self.saved_rev = rev;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn editable(&self) -> Result<()> {
        match &self.read_only {
            Some(reason) => bail!("read-only — {reason}"),
            None => Ok(()),
        }
    }

    fn check_rect(&self, [x, y, w, h]: [u32; 4]) -> Result<()> {
        let (iw, ih) = self.size();
        if w == 0 || h == 0 || x.saturating_add(w) > iw || y.saturating_add(h) > ih {
            bail!("rectangle {x},{y} {w}×{h} is outside the {iw}×{ih} image");
        }
        Ok(())
    }

    fn commit(&mut self, next: DynamicImage) {
        let prev = std::mem::replace(&mut self.img, next);
        self.undo.push(prev);
        self.redo.clear();
        let mut bytes: usize = self.undo.iter().map(|i| i.as_bytes().len()).sum();
        while self.undo.len() > HISTORY_STEPS || (bytes > HISTORY_BYTES && self.undo.len() > 1) {
            bytes -= self.undo.remove(0).as_bytes().len();
        }
        self.rev += 1;
    }

    pub fn crop(&mut self, rect: [u32; 4]) -> Result<String> {
        self.editable()?;
        self.check_rect(rect)?;
        let [x, y, w, h] = rect;
        let next = self.img.crop_imm(x, y, w, h);
        self.commit(next);
        Ok(format!("cropped to {w}×{h}"))
    }

    pub fn pixelize(&mut self, rect: [u32; 4], block: u32) -> Result<String> {
        self.editable()?;
        self.check_rect(rect)?;
        let mut next = self.img.clone();
        pixelize(&mut next, rect, block);
        self.commit(next);
        Ok(format!(
            "pixelized {}×{} with {}px blocks",
            rect[2],
            rect[3],
            block.max(1)
        ))
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.img, prev));
        self.rev += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.img, next));
        self.rev += 1;
        true
    }

    /// The current image encoded like the original: same format, PNG/WebP/BMP/TIFF lossless,
    /// JPEG quality 95, ICC profile and Exif kept where the format carries them.
    pub fn encode(&self) -> Result<Encoded> {
        self.editable()?;
        self.encoder()()
    }
}

/// The pixels converted to what `format`'s encoder accepts, keeping bit depth and alpha where
/// the format can hold them.
fn encodable(img: &DynamicImage, format: ImageFormat) -> DynamicImage {
    use image::ColorType as C;
    let color = img.color();
    let gray = matches!(color, C::L8 | C::L16 | C::La8 | C::La16);
    let alpha = color.has_alpha();
    match format {
        ImageFormat::Jpeg if gray => DynamicImage::ImageLuma8(img.to_luma8()),
        ImageFormat::Jpeg => DynamicImage::ImageRgb8(img.to_rgb8()),
        ImageFormat::Png => match color {
            C::Rgb32F => DynamicImage::ImageRgb16(img.to_rgb16()),
            C::Rgba32F => DynamicImage::ImageRgba16(img.to_rgba16()),
            _ => img.clone(),
        },
        ImageFormat::Tiff => match color {
            C::La8 => DynamicImage::ImageRgba8(img.to_rgba8()),
            C::La16 => DynamicImage::ImageRgba16(img.to_rgba16()),
            _ => img.clone(),
        },
        ImageFormat::Bmp => match (gray, alpha) {
            (true, false) => DynamicImage::ImageLuma8(img.to_luma8()),
            (true, true) => DynamicImage::ImageLumaA8(img.to_luma_alpha8()),
            (false, false) => DynamicImage::ImageRgb8(img.to_rgb8()),
            (false, true) => DynamicImage::ImageRgba8(img.to_rgba8()),
        },
        ImageFormat::WebP if alpha => DynamicImage::ImageRgba8(img.to_rgba8()),
        ImageFormat::WebP => DynamicImage::ImageRgb8(img.to_rgb8()),
        _ => DynamicImage::ImageRgba8(img.to_rgba8()),
    }
}

/// Encoded bytes, and what the encoder did not carry (one sentence each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub not_kept: Vec<String>,
}

impl Encoded {
    /// Bytes with nothing to report.
    pub fn plain(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            not_kept: Vec::new(),
        }
    }
}

pub fn encode(
    img: &DynamicImage,
    format: ImageFormat,
    icc: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
) -> Result<Encoded> {
    use image::codecs::{bmp, gif, jpeg, png, tiff, webp};
    fn with_meta(
        mut enc: impl ImageEncoder,
        img: &DynamicImage,
        icc: Option<Vec<u8>>,
        exif: Option<Vec<u8>>,
        not_kept: &mut Vec<String>,
    ) -> image::ImageResult<()> {
        // The save goes on without what the encoder refuses — and says so.
        if let Some(icc) = icc {
            if let Err(e) = enc.set_icc_profile(icc) {
                not_kept.push(format!("ICC profile not kept: {e}"));
            }
        }
        if let Some(exif) = exif {
            if let Err(e) = enc.set_exif_metadata(exif) {
                not_kept.push(format!("Exif not kept: {e}"));
            }
        }
        img.write_with_encoder(enc)
    }
    let img = encodable(img, format);
    let mut out = Vec::new();
    let mut not_kept = Vec::new();
    let nk = &mut not_kept;
    let written = match format {
        ImageFormat::Png => with_meta(png::PngEncoder::new(&mut out), &img, icc, exif, nk),
        ImageFormat::Jpeg => with_meta(
            jpeg::JpegEncoder::new_with_quality(&mut out, 95),
            &img,
            icc,
            exif,
            nk,
        ),
        ImageFormat::WebP => with_meta(
            webp::WebPEncoder::new_lossless(&mut out),
            &img,
            icc,
            exif,
            nk,
        ),
        ImageFormat::Bmp => with_meta(bmp::BmpEncoder::new(&mut out), &img, icc, exif, nk),
        ImageFormat::Tiff => {
            let mut cursor = Cursor::new(Vec::new());
            with_meta(tiff::TiffEncoder::new(&mut cursor), &img, icc, exif, nk)
                .map(|()| out = cursor.into_inner())
        }
        // GIF holds 256 colours per frame: pixelized areas are quantised by the encoder.
        ImageFormat::Gif => {
            if icc.is_some() {
                nk.push("ICC profile not kept: the GIF encoder writes none".to_string());
            }
            if exif.is_some() {
                nk.push("Exif not kept: the GIF encoder writes none".to_string());
            }
            gif::GifEncoder::new(&mut out).encode_frame(image::Frame::new(img.to_rgba8()))
        }
        other => return Err(anyhow!("cannot save {other:?} images")),
    };
    written.with_context(|| format!("cannot encode the {}", format_name(format)))?;
    Ok(Encoded {
        bytes: out,
        not_kept,
    })
}

/// Replaces every `block`×`block` cell of `rect` with its mean colour (cells at the right and
/// bottom edges are clipped to the rectangle). Works on the image's own pixel type, so 16-bit
/// and float images keep their precision.
pub fn pixelize(img: &mut DynamicImage, rect: [u32; 4], block: u32) {
    macro_rules! each {
        ($($variant:ident),*) => {
            match img {
                $(DynamicImage::$variant(buf) => pixelize_buf(buf, rect, block),)*
                other => {
                    let mut rgba = other.to_rgba8();
                    pixelize_buf(&mut rgba, rect, block);
                    *other = DynamicImage::ImageRgba8(rgba);
                }
            }
        };
    }
    each!(
        ImageLuma8,
        ImageLumaA8,
        ImageRgb8,
        ImageRgba8,
        ImageLuma16,
        ImageLumaA16,
        ImageRgb16,
        ImageRgba16,
        ImageRgb32F,
        ImageRgba32F
    );
}

fn pixelize_buf<P: Pixel>(buf: &mut ImageBuffer<P, Vec<P::Subpixel>>, rect: [u32; 4], block: u32) {
    let [x0, y0, w, h] = rect;
    let (x1, y1) = (
        x0.saturating_add(w).min(buf.width()),
        y0.saturating_add(h).min(buf.height()),
    );
    let b = block.max(1);
    let channels = usize::from(P::CHANNEL_COUNT);
    let integer = P::Subpixel::DEFAULT_MAX_VALUE.to_f64().unwrap_or(1.0) > 1.0;
    let mut sum = vec![0f64; channels];
    let mut by = y0;
    while by < y1 {
        let bh = b.min(y1 - by);
        let mut bx = x0;
        while bx < x1 {
            let bw = b.min(x1 - bx);
            sum.iter_mut().for_each(|s| *s = 0.0);
            for y in by..by + bh {
                for x in bx..bx + bw {
                    for (s, c) in sum.iter_mut().zip(buf.get_pixel(x, y).channels()) {
                        *s += c.to_f64().unwrap_or(0.0);
                    }
                }
            }
            let n = f64::from(bw * bh);
            let mean: Vec<P::Subpixel> = sum
                .iter()
                .map(|s| {
                    let v = if integer { (s / n).round() } else { s / n };
                    num_traits::NumCast::from(v).unwrap_or(P::Subpixel::DEFAULT_MIN_VALUE)
                })
                .collect();
            let cell = *P::from_slice(&mean);
            for y in by..by + bh {
                for x in bx..bx + bw {
                    buf.put_pixel(x, y, cell);
                }
            }
            bx += bw;
        }
        by += bh;
    }
}

// ---------------------------------------------------------------------------------------------
// The write side.

/// The opened image file: where it lives, its bytes as opened (for the backup) and the local
/// places sopen writes to.
pub struct ImageFile {
    pub origin: Origin,
    /// The bytes as opened — what the backup holds. Kept in memory: a later fetch of the same
    /// file can replace the cached copy while this window is open.
    original: Arc<Vec<u8>>,
    /// Encoded edits waiting to be uploaded, and refused ones: `<cache>/edits/<host>/<abs>`.
    edits: PathBuf,
    /// `<cache>` (backups go to `<cache>/backup/<host>/<abs>.<stamp>`).
    cache_root: PathBuf,
    /// Set by the first successful backup; later saves of the same session keep it.
    backup: Option<PathBuf>,
}

/// Result of one Save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    /// `notes`: what the owner must still hear (e.g. the remote changed right after the save).
    Written {
        remote: String,
        backup: PathBuf,
        notes: Vec<String>,
        /// What the saved file does not carry (metadata), shown with the result.
        not_kept: Vec<String>,
    },
    /// The remote changed since it was fetched; nothing was written.
    Refused { message: String, kept: PathBuf },
}

/// Why Save cannot replace the file on this server (spec §5): Save copy still works.
pub const CANNOT_REPLACE: &str = "this server cannot replace a file in one step (no \
     posix-rename@openssh.com) — Save copy writes the edit next to the original instead";

/// The one line for a write that failed, keeping the edit. A request that got no reply is
/// "outcome unknown — check <path>", never "not saved".
fn failed(e: &anyhow::Error, staged: &Path) -> anyhow::Error {
    match crate::remote::outcome_unknown(e) {
        Some(unknown) => anyhow!(
            "{unknown}; your edit is at {} (cause: {})",
            staged.display(),
            e.root_cause()
        ),
        None => anyhow!("{e:#}; not saved — your edit is at {}", staged.display()),
    }
}

impl ImageFile {
    pub fn new(origin: Origin, original: Vec<u8>, cache_root: &Path) -> Self {
        let edits = crate::cache::cache_path(&cache_root.join("edits"), origin.host(), &origin.abs);
        Self {
            origin,
            original: Arc::new(original),
            edits,
            cache_root: cache_root.to_path_buf(),
            backup: None,
        }
    }

    pub fn original(&self) -> &[u8] {
        &self.original
    }

    /// Whether Save can replace the original here (Save copy always can).
    pub fn can_replace(&self) -> bool {
        self.origin.can_replace()
    }

    fn stage(&self, encoded: &[u8], tag: &str) -> Result<PathBuf> {
        let path = super::keep_path(&self.edits, tag, SystemTime::now());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        std::fs::write(&path, encoded)
            .with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }

    /// The local backup of the original, written once per session before the first overwrite.
    fn ensure_backup(&mut self) -> Result<PathBuf> {
        if let Some(path) = &self.backup {
            return Ok(path.clone());
        }
        let path = crate::cache::backup_path(
            &self.cache_root,
            self.origin.host(),
            &self.origin.abs,
            SystemTime::now(),
        );
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        std::fs::write(&path, self.original.as_slice())
            .with_context(|| format!("cannot write the backup {}", path.display()))?;
        println!("backup of {} at {}", self.origin.display(), path.display());
        self.backup = Some(path.clone());
        Ok(path)
    }

    /// Save (spec §5), in this order: can this server replace a file in one step (refused
    /// before anything is written if not) → the local backup of the original (printed) →
    /// `encode` → stage the edit locally → conflict-checked overwrite. An encode failure leaves
    /// the backup made; a refusal or a transport error keeps the edit in a local file whose
    /// path the message names.
    pub fn save(&mut self, encode: impl FnOnce() -> Result<Encoded>) -> Result<Saved> {
        if !self.can_replace() {
            bail!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.origin.display()
            );
        }
        let backup = self.ensure_backup()?;
        let encoded = encode().with_context(|| {
            format!(
                "{} was not touched (the original is backed up at {})",
                self.origin.display(),
                backup.display()
            )
        })?;
        let not_kept = encoded.not_kept;
        let staged = self.stage(&encoded.bytes, "edited")?;
        let result = (|| -> Result<Saved> {
            let at = self.origin.at;
            let abs = self.origin.abs.clone();
            match self
                .origin
                .fs()?
                .write_atomic(&staged, &abs, at.fingerprint())?
            {
                WriteOutcome::Written { now, notes } => {
                    self.origin.at = now;
                    let _ = std::fs::remove_file(&staged);
                    Ok(Saved::Written {
                        remote: self.origin.display(),
                        backup,
                        notes,
                        not_kept,
                    })
                }
                WriteOutcome::Conflict { reason, notes, .. } => {
                    let kept = super::keep_path(&self.edits, "conflict", SystemTime::now());
                    std::fs::rename(&staged, &kept).with_context(|| {
                        format!("cannot keep the edited copy at {}", kept.display())
                    })?;
                    let mut message = format!(
                        "{} changed since it was opened ({reason}); not saved — your edit is at {}",
                        self.origin.display(),
                        kept.display()
                    );
                    for note in notes {
                        message.push_str(&format!("; {note}"));
                    }
                    Ok(Saved::Refused { message, kept })
                }
            }
        })();
        result.map_err(|e| failed(&e, &staged))
    }

    /// Save copy: `<stem>.edited.<ext>` next to the original (`-N` when taken). Returns the
    /// printed path of the new file.
    pub fn save_copy(&mut self, encoded: &[u8]) -> Result<String> {
        let staged = self.stage(encoded, "copy")?;
        let (dir, name) = self.origin.parent();
        let wanted = match name.rfind('.') {
            Some(i) if i > 0 => format!("{}.edited{}", &name[..i], &name[i..]),
            _ => format!("{name}.edited"),
        };
        let result = self
            .origin
            .fs()
            .and_then(|fs| fs.write_new_sibling(&staged, &dir, &wanted));
        match result {
            Ok(final_name) => {
                let _ = std::fs::remove_file(&staged);
                Ok(self.origin.show(&crate::remote::join(&dir, &final_name)))
            }
            Err(e) => Err(failed(&e, &staged)),
        }
    }
}

fn apply_state(doc: &mut Doc, sel: &mut Selection, block: &mut u32, hooks: &Hooks) -> Result<()> {
    let state = &hooks.state;
    if let Some(b) = state.block {
        *block = b.max(1);
    }
    if let Some(r) = state.rect {
        sel.set(r);
    }
    for op in &state.ops {
        let (w, h) = doc.size();
        let rect = sel
            .pixels(Vec2::new(w as f32, h as f32))
            .context("SOPEN_STATE op needs a rect inside the image")?;
        match op {
            Op::Crop => {
                doc.crop(rect)?;
                sel.clear();
            }
            Op::Pixelize => {
                doc.pixelize(rect, *block)?;
            }
        }
    }
    Ok(())
}

/// Opens the image: the window, or the windowless `SOPEN_AUTOSAVE` run.
/// `notice` is shown in the status line on open (an earlier copy with unsaved edits was kept).
pub fn open(file: ImageFile, hooks: &Hooks, notice: Option<String>) -> Result<Outcome> {
    let mut doc = Doc::decode(file.original())?;
    let mut sel = Selection::default();
    let mut block = DEFAULT_BLOCK;
    apply_state(&mut doc, &mut sel, &mut block, hooks)?;
    if hooks.autosave {
        return autosave(file, &doc, hooks);
    }
    let title = file.origin.display();
    let outcome = Arc::new(Mutex::new(Outcome::default()));
    let (w, h) = doc.size();
    let can_replace = file.can_replace();
    let leftover: Arc<Mutex<Option<Job>>> = Arc::default();
    // Before the owner acts: what Save cannot do (status line) and what it will not keep
    // (a label that stays in the status bar). Both also go to the terminal.
    if let Some(line) = doc.not_kept_line() {
        eprintln!("sopen: {line}");
    }
    let warnings: Vec<String> = notice
        .into_iter()
        .chain((!can_replace).then(|| CANNOT_REPLACE.to_string()))
        .collect();
    let status = if warnings.is_empty() {
        Status::default()
    } else {
        for line in &warnings {
            eprintln!("sopen: {line}");
        }
        Status::error(warnings.join(" · "))
    };
    let window = ImageWindow {
        title: title.clone(),
        can_replace,
        doc,
        file: Arc::new(Mutex::new(file)),
        tex: None,
        tex_rev: None,
        sel,
        block,
        view: View::default(),
        status,
        job: None,
        outcome: outcome.clone(),
        prompt: false,
        close_ok: false,
        close_when_done: false,
        leftover: leftover.clone(),
        painted: false,
        zoom: 1.0,
        map: None,
    };
    // A window that shows the image near 1:1 where the screen allows; eframe clamps to it.
    let size = [
        (w as f32).clamp(640.0, 1400.0),
        (h as f32 + 90.0).clamp(420.0, 940.0),
    ];
    let run = super::run_window(&title, size, window, hooks);
    // Never exit with a write in flight without its outcome on the terminal.
    if let Some(job) = leftover.lock().unwrap().take() {
        eprintln!("sopen: the window closed during a save; waiting for it to finish");
        let result = job
            .rx
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the save stopped without a result")));
        report(&result, &mut outcome.lock().unwrap());
    }
    run?;
    let outcome = outcome.lock().unwrap().clone();
    Ok(outcome)
}

fn autosave(mut file: ImageFile, doc: &Doc, hooks: &Hooks) -> Result<Outcome> {
    if let Some(delay) = hooks.save_delay {
        eprintln!("waiting {} ms before saving", delay.as_millis());
        std::thread::sleep(delay);
    }
    let mut outcome = Outcome::default();
    if let Some(line) = doc.not_kept_line() {
        eprintln!("sopen: {line}");
    }
    match file.save(doc.encoder())? {
        Saved::Written {
            remote,
            notes,
            not_kept,
            ..
        } => {
            outcome.saved = true;
            let (w, h) = doc.size();
            let line = format!("saved {remote} ({w}×{h} {})", format_name(doc.format));
            println!("{}", super::with_notes(&line, &not_kept));
            for note in notes {
                eprintln!("sopen: {note}");
            }
        }
        Saved::Refused { message, .. } => {
            outcome.conflict = true;
            outcome.refused = Some(message);
        }
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------------------------
// The window.

/// Prints a finished save to the terminal, records it in `outcome` and returns the window's
/// status line. Also used for a save still in flight when the window went away.
fn report(result: &Result<JobDone>, outcome: &mut Outcome) -> Status {
    match result {
        Ok(JobDone::Saved(Saved::Written {
            remote,
            backup,
            notes,
            not_kept,
        })) => {
            outcome.saved = true;
            outcome.refused = None;
            println!(
                "{}",
                super::with_notes(&format!("saved {remote}"), not_kept)
            );
            for note in notes {
                eprintln!("sopen: {note}");
            }
            let all: Vec<String> = notes.iter().chain(not_kept).cloned().collect();
            if all.is_empty() {
                Status::info(format!(
                    "saved {remote} · original backed up at {}",
                    backup.display()
                ))
            } else {
                // The owner has to see these (the remote changed right after; metadata).
                Status::error(super::with_notes(&format!("saved {remote}"), &all))
            }
        }
        Ok(JobDone::Saved(Saved::Refused { message, .. })) => {
            outcome.conflict = true;
            outcome.refused = Some(message.clone());
            eprintln!("sopen: {message}");
            Status::error(message.clone())
        }
        Ok(JobDone::Copy(path, not_kept)) => {
            let line = super::with_notes(&format!("saved a copy at {path}"), not_kept);
            println!("{line}");
            if not_kept.is_empty() {
                Status::info(line)
            } else {
                Status::error(line)
            }
        }
        Err(e) => {
            eprintln!("sopen: {e:#}");
            Status::error(format!("{e:#}"))
        }
    }
}

/// `scale` = screen points per image pixel (`None` = fit); `center` = the image point at the
/// centre of the view.
#[derive(Debug, Clone, Copy, Default)]
struct View {
    scale: Option<f32>,
    center: Option<Pos2>,
}

enum JobKind {
    Save { close_after: bool },
    SaveCopy { close_after: bool },
}

struct Job {
    kind: JobKind,
    rev: u64,
    rx: Receiver<Result<JobDone>>,
}

enum JobDone {
    Saved(Saved),
    /// The copy's printed path and what it does not carry.
    Copy(String, Vec<String>),
}

struct ImageWindow {
    /// `host:/abs` (the file is locked by a running save; the title is not).
    title: String,
    /// False on a server without `posix-rename@openssh.com`: Save is off, Save copy works.
    can_replace: bool,
    doc: Doc,
    file: Arc<Mutex<ImageFile>>,
    tex: Option<TextureHandle>,
    tex_rev: Option<u64>,
    sel: Selection,
    block: u32,
    view: View,
    status: Status,
    job: Option<Job>,
    outcome: Arc<Mutex<Outcome>>,
    prompt: bool,
    close_ok: bool,
    /// Close was asked for while a save ran: close when it is done.
    close_when_done: bool,
    /// Where a save still in flight goes if the window is dropped anyway; `open` waits for it.
    leftover: Arc<Mutex<Option<Job>>>,
    painted: bool,
    /// Device pixels per image pixel, for the status bar.
    zoom: f32,
    /// Where the last pass drew the image (screen ↔ image pixels).
    map: Option<Map>,
}

impl Drop for ImageWindow {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            *self.leftover.lock().unwrap() = Some(job);
        }
    }
}

impl ImageWindow {
    fn bounds(&self) -> Vec2 {
        let (w, h) = self.doc.size();
        Vec2::new(w as f32, h as f32)
    }

    fn rect(&self) -> Option<[u32; 4]> {
        self.sel.pixels(self.bounds())
    }

    fn report(&mut self, result: Result<String>) {
        self.status = match result {
            Ok(text) => Status::info(text),
            Err(e) => Status::error(format!("{e:#}")),
        };
    }

    fn crop(&mut self) {
        let Some(rect) = self.rect() else {
            return self.status = Status::error("draw a rectangle first");
        };
        let done = self.doc.crop(rect);
        if done.is_ok() {
            self.sel.clear();
            self.view = View::default();
        }
        self.report(done);
    }

    fn pixelize(&mut self) {
        let Some(rect) = self.rect() else {
            return self.status = Status::error("draw a rectangle first");
        };
        let done = self.doc.pixelize(rect, self.block);
        self.report(done);
    }

    fn undo(&mut self) {
        if self.doc.undo() {
            self.status = Status::info("undone");
        }
    }

    fn redo(&mut self) {
        if self.doc.redo() {
            self.status = Status::info("redone");
        }
    }

    fn start(&mut self, ctx: &egui::Context, kind: JobKind) {
        if self.job.is_some() {
            return;
        }
        if let Some(reason) = &self.doc.read_only {
            return self.status = Status::error(format!("cannot save: {reason}"));
        }
        if matches!(kind, JobKind::Save { .. }) {
            if !self.can_replace {
                return self.status = Status::error(CANNOT_REPLACE);
            }
            if !self.doc.dirty() {
                return self.status = Status::info("no changes to save");
            }
        }
        let encoder = self.doc.encoder();
        let file = self.file.clone();
        let copy = matches!(kind, JobKind::SaveCopy { .. });
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut file = file.lock().unwrap();
            let result = if copy {
                encoder().and_then(|enc| {
                    file.save_copy(&enc.bytes)
                        .map(|path| JobDone::Copy(path, enc.not_kept))
                })
            } else {
                file.save(encoder).map(JobDone::Saved)
            };
            drop(file);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.status = Status::info(if copy {
            "saving a copy…"
        } else {
            "saving…"
        });
        self.job = Some(Job {
            kind,
            rev: self.doc.rev(),
            rx,
        });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        let Ok(result) = job.rx.try_recv() else {
            return;
        };
        let job = self.job.take().expect("checked above");
        self.status = report(&result, &mut self.outcome.lock().unwrap());
        let close_after = match &result {
            Ok(JobDone::Saved(Saved::Written { notes, .. })) => {
                self.doc.mark_saved(job.rev);
                matches!(job.kind, JobKind::Save { close_after: true }) && notes.is_empty()
            }
            Ok(JobDone::Copy(..)) => {
                self.doc.mark_copied(job.rev);
                matches!(job.kind, JobKind::SaveCopy { close_after: true })
            }
            _ => false,
        };
        let waited = std::mem::take(&mut self.close_when_done);
        if close_after || (waited && !self.doc.unsaved()) {
            self.close_ok = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if waited {
            // Closing was asked for during a save that did not land: ask what to do.
            self.prompt = true;
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        use super::{command_key, plain_key};
        if self.prompt {
            return;
        }
        if command_key(ctx, Key::Z, true) {
            self.redo();
        } else if command_key(ctx, Key::Z, false) {
            self.undo();
        }
        if command_key(ctx, Key::S, true) {
            self.start(ctx, JobKind::SaveCopy { close_after: false });
        } else if command_key(ctx, Key::S, false) {
            self.start(ctx, JobKind::Save { close_after: false });
        }
        if plain_key(ctx, Key::C) {
            self.crop();
        }
        if plain_key(ctx, Key::P) {
            self.pixelize();
        }
        if plain_key(ctx, Key::Escape) {
            self.sel.clear();
        }
        if plain_key(ctx, Key::F) {
            self.view = View::default();
        }
        if plain_key(ctx, Key::Num1) {
            self.view.scale = Some(1.0 / ctx.pixels_per_point());
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let editable = self.doc.read_only.is_none();
        let has_rect = self.rect().is_some();
        let busy = self.job.is_some();
        ui.horizontal(|ui| {
            let why = "draw a rectangle on the image first";
            if ui
                .add_enabled(editable && has_rect, egui::Button::new("Crop  C"))
                .on_disabled_hover_text(why)
                .clicked()
            {
                self.crop();
            }
            if ui
                .add_enabled(editable && has_rect, egui::Button::new("Pixelize  P"))
                .on_disabled_hover_text(why)
                .clicked()
            {
                self.pixelize();
            }
            ui.add_enabled(
                editable,
                egui::Slider::new(&mut self.block, 2..=128)
                    .text("block px")
                    .logarithmic(true),
            );
            ui.separator();
            if ui
                .add_enabled(self.doc.can_undo(), egui::Button::new("Undo"))
                .on_hover_text("⌘/Ctrl+Z")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(self.doc.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("⇧⌘/Shift+Ctrl+Z")
                .clicked()
            {
                self.redo();
            }
            ui.separator();
            let reason = self.doc.read_only.clone().unwrap_or_default();
            let save = ui
                .add_enabled(
                    editable && !busy && self.doc.dirty() && self.can_replace,
                    egui::Button::new("Save"),
                )
                .on_hover_text("⌘/Ctrl+S — overwrites the original after a local backup")
                .on_disabled_hover_text(if !reason.is_empty() {
                    &reason
                } else if !self.can_replace {
                    CANNOT_REPLACE
                } else {
                    "no changes to save"
                });
            if save.clicked() {
                self.start(&ctx, JobKind::Save { close_after: false });
            }
            let copy = ui
                .add_enabled(editable && !busy, egui::Button::new("Save copy"))
                .on_hover_text("⇧⌘/Shift+Ctrl+S — a new <name>.edited file next to the original")
                .on_disabled_hover_text(&reason);
            if copy.clicked() {
                self.start(&ctx, JobKind::SaveCopy { close_after: false });
            }
            if busy {
                ui.spinner();
            }
        });
    }

    /// `zoom` is the previous pass's (the image is laid out after the bar).
    /// One line whatever it says (see [`super::one_line`]): the image above must not move.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let zoom = self.zoom;
        let (w, h) = self.doc.size();
        ui.horizontal(|ui| {
            ui.monospace(format!(
                "{w}×{h} {} · {:.0}%",
                format_name(self.doc.format),
                zoom * 100.0
            ));
            if let Some([x, y, rw, rh]) = self.rect() {
                ui.separator();
                ui.monospace(format!("rect {x},{y} {rw}×{rh}"));
            }
            if self.doc.unsaved() {
                ui.separator();
                ui.label("unsaved changes");
            }
            let warn = match &self.doc.read_only {
                Some(reason) => Some(format!("read-only: {reason}")),
                None => self.doc.not_kept_line(),
            };
            ui.separator();
            if let Some(warn) = warn {
                // The warning and the status share the rest of the line: the status keeps up
                // to half of it, the warning gets everything else.
                let font = egui::TextStyle::Body.resolve(ui.style());
                let width = |text: &str| {
                    ui.painter()
                        .layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE)
                        .size()
                        .x
                };
                let room = ui.available_width();
                let status = if self.status.text.is_empty() {
                    0.0
                } else {
                    (width(&self.status.text) + 16.0).min(room / 2.0)
                };
                let size = Vec2::new(room - status, ui.available_height());
                let color = ui.visuals().warn_fg_color;
                ui.allocate_ui(size, |ui| super::one_line(ui, &warn, color));
                if !self.status.text.is_empty() {
                    ui.separator();
                }
            }
            self.status.show(ui);
        });
    }

    fn texture(&mut self, ctx: &egui::Context) -> egui::TextureId {
        if self.tex_rev != Some(self.doc.rev()) || self.tex.is_none() {
            let max = ctx.input(|i| i.max_texture_side) as u32;
            let (w, h) = self.doc.size();
            // A picture bigger than the GPU's texture limit is shown downscaled; edits still
            // address the full-resolution pixels.
            let shown = if w > max || h > max {
                self.doc.img.thumbnail(max, max).to_rgba8()
            } else {
                self.doc.img.to_rgba8()
            };
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [shown.width() as usize, shown.height() as usize],
                shown.as_raw(),
            );
            let options = egui::TextureOptions {
                magnification: egui::TextureFilter::Nearest,
                ..egui::TextureOptions::LINEAR
            };
            match &mut self.tex {
                Some(tex) => tex.set(ci, options),
                None => self.tex = Some(ctx.load_texture("image", ci, options)),
            }
            self.tex_rev = Some(self.doc.rev());
        }
        self.tex.as_ref().expect("just set").id()
    }

    /// Wheel (mouse) and pinch zoom about the pointer; trackpad scroll and secondary/middle
    /// drag pan.
    fn navigate(&mut self, ui: &egui::Ui, resp: &egui::Response, panel: Rect, fit_scale: f32) {
        let src = self.bounds();
        let mut scale = self.view.scale.unwrap_or(fit_scale);
        let mut center = self.view.center.unwrap_or((src / 2.0).to_pos2());
        let (mut zoom, mut pan) = (1.0f32, Vec2::ZERO);
        if resp.hovered() {
            ui.input(|i| {
                zoom *= i.zoom_delta();
                for event in &i.events {
                    if let egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } = event
                    {
                        if modifiers.command || modifiers.ctrl {
                            continue; // already in zoom_delta
                        }
                        match unit {
                            egui::MouseWheelUnit::Point => pan += *delta,
                            _ => zoom *= 1.15f32.powf(delta.y),
                        }
                    }
                }
            });
        }
        if resp.dragged_by(egui::PointerButton::Secondary)
            || resp.dragged_by(egui::PointerButton::Middle)
        {
            pan += resp.drag_delta();
        }
        if zoom != 1.0 {
            let next = (scale * zoom).clamp(fit_scale.min(0.05), 64.0);
            if let Some(p) = resp.hover_pos() {
                // Keep the image point under the pointer where it is.
                let under = center + (p - panel.center()) / scale;
                center = under - (p - panel.center()) / next;
            }
            scale = next;
            self.view.scale = Some(scale);
            self.view.center = Some(center);
        }
        if pan != Vec2::ZERO && self.view.scale.is_some() {
            self.view.center = Some(center - pan / scale);
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) -> f32 {
        let ctx = ui.ctx().clone();
        let panel = ui.max_rect();
        let src = self.bounds();
        let fit_scale = (panel.width() / src.x).min(panel.height() / src.y);
        let resp = ui.allocate_rect(panel, Sense::click_and_drag());
        self.navigate(ui, &resp, panel, fit_scale);
        let scale = self.view.scale.unwrap_or(fit_scale);
        let center = self.view.center.unwrap_or((src / 2.0).to_pos2());
        let disp = Rect::from_min_size(panel.center() - center.to_vec2() * scale, src * scale);
        let map = Map { disp, src };
        let tex = self.texture(&ctx);
        let painter = ui.painter_at(panel);
        painter.rect_filled(panel, 0.0, Color32::from_gray(28));
        painter.image(
            tex,
            disp,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if self.doc.read_only.is_none() && !self.prompt {
            self.sel.handle(ui, &resp, map);
        }
        self.sel.draw(&painter, map);
        self.map = Some(map);
        self.painted = true;
        scale * ctx.pixels_per_point()
    }

    fn close_prompt(&mut self, ctx: &egui::Context) {
        if !self.prompt {
            return;
        }
        let modal = egui::Modal::new(egui::Id::unique("unsaved")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("Unsaved changes");
            ui.label(format!(
                "Save the changes to {} before closing?",
                self.title
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if self.can_replace && ui.button("Save").clicked() {
                    self.prompt = false;
                    self.start(ctx, JobKind::Save { close_after: true });
                }
                if !self.can_replace && ui.button("Save copy").clicked() {
                    self.prompt = false;
                    self.start(ctx, JobKind::SaveCopy { close_after: true });
                }
                if ui.button("Discard").clicked() {
                    self.prompt = false;
                    self.close_ok = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    self.prompt = false;
                }
            });
        });
        if modal.should_close() {
            self.prompt = false;
        }
    }
}

impl super::Window for ImageWindow {
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);
        self.keys(&ctx);
        egui::Panel::top("tools").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            ui.add_space(1.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.zoom = self.central(ui));
        self.close_prompt(&ctx);
    }

    fn ready(&self) -> bool {
        self.painted && self.job.is_none()
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok {
            return true;
        }
        if self.job.is_some() {
            // Never close mid-write: the window closes once the save has reported.
            self.close_when_done = true;
            self.status = Status::info("saving… the window closes when the save is done");
            return false;
        }
        if !self.doc.unsaved() {
            return true;
        }
        self.prompt = true;
        false
    }
}

#[cfg(test)]
mod input_tests;

#[cfg(test)]
mod robustness_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{LocalFs, RemoteFs};
    use image::{GenericImageView, Rgb, Rgba};

    fn gradient(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgba8(ImageBuffer::from_fn(w, h, |x, y| {
            Rgba([(x * 7) as u8, (y * 5) as u8, ((x + y) * 3) as u8, 255])
        }))
    }

    fn png_bytes(img: &DynamicImage) -> Vec<u8> {
        encode(img, ImageFormat::Png, None, None).unwrap().bytes
    }

    #[test]
    fn crop_keeps_the_selected_pixels() {
        let img = gradient(40, 30);
        let mut doc = Doc::decode(&png_bytes(&img)).unwrap();
        assert_eq!(doc.crop([5, 7, 21, 11]).unwrap(), "cropped to 21×11");
        assert_eq!(doc.size(), (21, 11));
        assert_eq!(doc.img.get_pixel(0, 0), img.get_pixel(5, 7));
        assert_eq!(doc.img.get_pixel(20, 10), img.get_pixel(25, 17));
        assert!(
            doc.crop([10, 0, 20, 5]).is_err(),
            "outside the cropped image"
        );
    }

    #[test]
    fn pixelize_averages_cells_and_leaves_the_rest() {
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_fn(8, 4, |x, _| {
            Rgb([if x % 2 == 0 { 0 } else { 200 }, 10, 20])
        }));
        let mut px = img.clone();
        // 5 px wide region with 2 px blocks: cells at x=1..3, 3..5, and a clipped 5..6.
        pixelize(&mut px, [1, 0, 5, 4], 2);
        assert_eq!(px.get_pixel(0, 0), img.get_pixel(0, 0), "outside untouched");
        assert_eq!(px.get_pixel(7, 3), img.get_pixel(7, 3), "outside untouched");
        assert_eq!(px.get_pixel(1, 0).0, [100, 10, 20, 255]);
        assert_eq!(px.get_pixel(2, 3).0, [100, 10, 20, 255]);
        assert_eq!(
            px.get_pixel(5, 2).0,
            [200, 10, 20, 255],
            "clipped edge cell"
        );
        assert_eq!(px.color(), image::ColorType::Rgb8, "pixel type kept");
    }

    #[test]
    fn pixelize_keeps_sixteen_bit_precision() {
        let img = DynamicImage::ImageLuma16(ImageBuffer::from_fn(4, 1, |x, _| {
            image::Luma([[1000u16, 1001, 60000, 60003][x as usize]])
        }));
        let mut px = img.clone();
        pixelize(&mut px, [0, 0, 4, 1], 2);
        let DynamicImage::ImageLuma16(buf) = px else {
            panic!("variant changed")
        };
        assert_eq!(buf.as_raw(), &[1001, 1001, 60002, 60002]);
    }

    #[test]
    fn undo_redo_and_dirty_tracking() {
        let mut doc = Doc::decode(&png_bytes(&gradient(20, 20))).unwrap();
        assert!(!doc.dirty() && !doc.can_undo());
        doc.pixelize([0, 0, 10, 10], 4).unwrap();
        doc.crop([0, 0, 5, 5]).unwrap();
        assert!(doc.dirty());
        let saved = doc.rev();
        doc.mark_saved(saved);
        assert!(!doc.dirty());
        assert!(doc.undo());
        assert_eq!(doc.size(), (20, 20));
        assert!(doc.dirty());
        assert!(doc.redo());
        assert_eq!(doc.size(), (5, 5));
        assert!(!doc.redo());
        doc.undo();
        doc.undo();
        assert!(!doc.can_undo());
        assert_eq!(
            doc.img,
            Doc::decode(&png_bytes(&gradient(20, 20))).unwrap().img
        );
    }

    /// Every format round-trips: same container, same colour type where the format holds it,
    /// and exact pixels for the lossless ones.
    #[test]
    fn encoders_round_trip_per_format() {
        let rgba = gradient(33, 17);
        let rgb = DynamicImage::ImageRgb8(rgba.to_rgb8());
        let rgb16 = DynamicImage::ImageRgb16(ImageBuffer::from_fn(9, 5, |x, y| {
            Rgb([x as u16 * 7001, y as u16 * 13007, 65535])
        }));
        let cases: [(ImageFormat, &DynamicImage, image::ColorType, bool); 8] = [
            (ImageFormat::Png, &rgba, image::ColorType::Rgba8, true),
            (ImageFormat::Png, &rgb16, image::ColorType::Rgb16, true),
            (ImageFormat::WebP, &rgba, image::ColorType::Rgba8, true),
            (ImageFormat::WebP, &rgb, image::ColorType::Rgb8, true),
            (ImageFormat::Bmp, &rgb, image::ColorType::Rgb8, true),
            (ImageFormat::Tiff, &rgba, image::ColorType::Rgba8, true),
            (ImageFormat::Tiff, &rgb16, image::ColorType::Rgb16, true),
            (ImageFormat::Jpeg, &rgb, image::ColorType::Rgb8, false),
        ];
        for (format, img, color, lossless) in cases {
            let bytes = encode(img, format, None, None).unwrap().bytes;
            let back = Doc::decode(&bytes).unwrap();
            assert_eq!(back.format, format, "{format:?}");
            assert_eq!(back.img.color(), color, "{format:?} colour type");
            assert_eq!(back.size(), (img.width(), img.height()), "{format:?}");
            if lossless {
                assert_eq!(back.img.as_bytes(), img.as_bytes(), "{format:?} pixels");
            } else {
                // JPEG q95 on a smooth gradient: every channel within a few levels.
                let worst = back
                    .img
                    .to_rgb8()
                    .as_raw()
                    .iter()
                    .zip(img.to_rgb8().as_raw())
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                assert!(worst <= 12, "{format:?} worst channel error {worst}");
            }
        }
    }

    #[test]
    fn gif_static_saves_and_animated_is_read_only() {
        let still = encode(&gradient(10, 10), ImageFormat::Gif, None, None)
            .unwrap()
            .bytes;
        let doc = Doc::decode(&still).unwrap();
        assert_eq!(doc.format, ImageFormat::Gif);
        assert!(doc.read_only.is_none());
        assert!(doc.encode().is_ok());

        let mut anim = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut anim);
            for shade in [10u8, 200] {
                let frame = image::RgbaImage::from_pixel(6, 6, Rgba([shade, 0, 0, 255]));
                enc.encode_frame(image::Frame::new(frame)).unwrap();
            }
        }
        let mut doc = Doc::decode(&anim).unwrap();
        let reason = doc.read_only.clone().expect("animated GIF is read-only");
        assert!(reason.contains("animated GIF"), "{reason}");
        assert_eq!(doc.img.get_pixel(0, 0).0[0], 10, "frame 1 shown");
        assert!(doc.crop([0, 0, 2, 2]).is_err());
        assert!(doc.encode().unwrap_err().to_string().contains("read-only"));
    }

    /// A copy of a one-page little-endian TIFF with its first page linked in again as page 2
    /// (both pages share the strip data — a valid two-page file).
    fn two_page_tiff(one: &[u8]) -> Vec<u8> {
        assert_eq!(&one[..4], b"II*\0");
        let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as usize;
        let u32_at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let ifd0 = u32_at(one, 4) as usize;
        let n = u16_at(one, ifd0);
        let next_at = ifd0 + 2 + 12 * n;
        assert_eq!(u32_at(one, next_at), 0, "one page");
        let mut out = one.to_vec();
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let ifd1 = out.len();
        out.extend_from_slice(&one[ifd0..next_at]);
        out.extend_from_slice(&0u32.to_le_bytes());
        out[next_at..next_at + 4].copy_from_slice(&(ifd1 as u32).to_le_bytes());
        out
    }

    #[test]
    fn tiff_page_count_reads_both_byte_orders_and_stops_on_garbage() {
        let one = encode(&gradient(4, 4), ImageFormat::Tiff, None, None)
            .unwrap()
            .bytes;
        assert_eq!(tiff_pages(&one, 10), 1);
        assert_eq!(tiff_pages(&two_page_tiff(&one), 10), 2);
        assert_eq!(tiff_pages(&two_page_tiff(&one), 1), 1, "limit");
        // Big-endian header, one IFD with no entries at offset 8.
        let mm = [b"MM\0*".as_slice(), &[0, 0, 0, 8], &[0, 0], &[0, 0, 0, 0]].concat();
        assert_eq!(tiff_pages(&mm, 10), 1);
        // An IFD whose "next" points at itself counts once.
        let mut lp = mm.clone();
        lp[10..14].copy_from_slice(&[0, 0, 0, 8]);
        assert_eq!(tiff_pages(&lp, 10), 1);
        assert_eq!(
            tiff_pages(b"II*\0\xff\xff\xff\x7f", 10),
            0,
            "offset past the end"
        );
        assert_eq!(tiff_pages(b"GIF89a", 10), 0);
    }

    #[test]
    fn multi_page_tiff_is_read_only() {
        let one = encode(&gradient(16, 8), ImageFormat::Tiff, None, None)
            .unwrap()
            .bytes;
        let single = Doc::decode(&one).unwrap();
        assert!(single.read_only.is_none(), "one page is editable");
        let two = two_page_tiff(&one);
        let mut doc = Doc::decode(&two).unwrap();
        assert_eq!(doc.size(), (16, 8), "page 1 shown");
        let reason = doc.read_only.clone().expect("two pages: read-only");
        assert!(reason.contains("multi-page TIFF"), "{reason}");
        assert!(doc.crop([0, 0, 4, 4]).is_err());
        assert!(doc.encode().is_err());
    }

    /// Exif with no tags (big-endian TIFF header, empty IFD).
    const EMPTY_EXIF: &[u8] = b"MM\x00\x2a\x00\x00\x00\x08\x00\x00\x00\x00\x00\x00";

    fn fake_icc() -> Vec<u8> {
        (0..=255u8).cycle().take(600).collect()
    }

    #[test]
    fn png_and_jpeg_keep_icc_and_exif() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let src = DynamicImage::ImageRgb8(gradient(24, 16).to_rgb8());
            let made = encode(&src, format, Some(fake_icc()), Some(EMPTY_EXIF.to_vec())).unwrap();
            assert!(made.not_kept.is_empty(), "{format:?}: {:?}", made.not_kept);
            let doc = Doc::decode(&made.bytes).unwrap();
            assert_eq!(
                doc.icc.as_deref(),
                Some(fake_icc().as_slice()),
                "{format:?}"
            );
            assert_eq!(doc.exif.as_deref(), Some(EMPTY_EXIF), "{format:?}");
            assert_eq!(doc.not_kept_line(), None, "{format:?}");
            let saved = doc.encode().unwrap();
            assert!(
                saved.not_kept.is_empty(),
                "{format:?}: {:?}",
                saved.not_kept
            );
            let again = Doc::decode(&saved.bytes).unwrap();
            assert_eq!(again.icc, doc.icc, "{format:?} ICC round trip");
            assert_eq!(again.exif, doc.exif, "{format:?} Exif round trip");
        }
    }

    /// A profile or Exif the encoder refuses is not swallowed: the save goes on and says what
    /// it did not keep.
    #[test]
    fn encoder_refusals_are_reported() {
        let img = gradient(8, 8);
        let bmp = encode(&img, ImageFormat::Bmp, Some(fake_icc()), None).unwrap();
        assert_eq!(bmp.not_kept.len(), 1);
        assert!(
            bmp.not_kept[0].starts_with("ICC profile not kept: "),
            "{:?}",
            bmp.not_kept
        );
        let tiff = encode(&img, ImageFormat::Tiff, None, Some(EMPTY_EXIF.to_vec())).unwrap();
        assert!(
            tiff.not_kept[0].starts_with("Exif not kept: "),
            "{:?}",
            tiff.not_kept
        );
        let gif = encode(&img, ImageFormat::Gif, Some(fake_icc()), None).unwrap();
        assert!(gif.not_kept[0].starts_with("ICC profile not kept: "));
        assert!(
            !bmp.bytes.is_empty() && !tiff.bytes.is_empty(),
            "the save went on"
        );
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

    /// `png` with `chunks` inserted before IEND.
    fn png_with_chunks(png: &[u8], chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let iend = png.len() - 12;
        let mut out = png[..iend].to_vec();
        for (kind, data) in chunks {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let typed: Vec<u8> = [kind.as_slice(), data].concat();
            out.extend_from_slice(&typed);
            out.extend_from_slice(&crc32(&typed).to_be_bytes());
        }
        out.extend_from_slice(&png[iend..]);
        out
    }

    /// XMP and text the encoder cannot write are named when the image opens and again in the
    /// save outcome — PNG (iTXt XMP + tEXt) and JPEG (APP1 XMP).
    #[test]
    fn xmp_and_text_chunks_are_reported_on_open_and_in_the_save_outcome() {
        let xmp = b"XML:com.adobe.xmp\0\0\0\0\0<x:xmpmeta xmlns:x='adobe:ns:meta/'/>";
        let png = png_with_chunks(
            &png_bytes(&gradient(16, 16)),
            &[(b"iTXt", xmp.as_slice()), (b"tEXt", b"Comment\0shot")],
        );
        let doc = Doc::decode(&png).unwrap();
        assert_eq!(
            doc.not_kept_line().as_deref(),
            Some("Save will not keep: XMP, 1 text chunk")
        );

        let jpeg = encode(&gradient(16, 16), ImageFormat::Jpeg, None, None)
            .unwrap()
            .bytes;
        let payload = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x='adobe:ns:meta/'/>";
        let mut with_xmp = jpeg[..2].to_vec();
        with_xmp.extend_from_slice(&[0xFF, 0xE1]);
        with_xmp.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        with_xmp.extend_from_slice(payload);
        with_xmp.extend_from_slice(&jpeg[2..]);
        let jdoc = Doc::decode(&with_xmp).unwrap();
        assert_eq!(
            jdoc.not_kept_line().as_deref(),
            Some("Save will not keep: XMP")
        );

        // The model's save outcome repeats it.
        let tmp = tempfile::tempdir().unwrap();
        let mut file = local_file(tmp.path(), "x.png", &png);
        let mut doc = Doc::decode(file.original()).unwrap();
        doc.pixelize([0, 0, 8, 8], 4).unwrap();
        let Saved::Written { not_kept, .. } = file.save(doc.encoder()).unwrap() else {
            panic!("expected Written")
        };
        assert_eq!(not_kept, ["not kept: XMP, 1 text chunk"]);
    }

    #[test]
    fn tiff_and_webp_xmp_are_found() {
        // Little-endian TIFF, one IFD with tags 270 (description) and 700 (XMP).
        let mut tiff = b"II*\0\x08\0\0\0\x02\0".to_vec();
        for tag in [270u16, 700] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&[2, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        }
        tiff.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            foreign_metadata(&tiff, ImageFormat::Tiff),
            ["XMP", "1 text tag"]
        );
        let mut webp = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
        webp.extend_from_slice(&3u32.to_le_bytes());
        webp.extend_from_slice(&[1, 2, 3, 0]);
        webp.extend_from_slice(b"XMP ");
        webp.extend_from_slice(&2u32.to_le_bytes());
        webp.extend_from_slice(b"<>");
        assert_eq!(foreign_metadata(&webp, ImageFormat::WebP), ["XMP"]);
        assert!(foreign_metadata(b"\x89PNG\r\n\x1a\n", ImageFormat::Png).is_empty());
    }

    #[test]
    fn exif_orientation_is_applied_and_cleared() {
        // A 4×2 JPEG tagged "rotate 90° clockwise" opens as 2×4, and the saved Exif no longer
        // asks viewers to rotate it again.
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 2, Rgb([90, 90, 90])));
        let exif: Vec<u8> = [
            b"MM\x00\x2a\x00\x00\x00\x08".as_slice(),
            &[0x00, 0x01],
            &[
                0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00,
            ],
            &[0x00, 0x00, 0x00, 0x00],
        ]
        .concat();
        let bytes = encode(&img, ImageFormat::Jpeg, None, Some(exif))
            .unwrap()
            .bytes;
        let doc = Doc::decode(&bytes).unwrap();
        assert_eq!(doc.size(), (2, 4));
        let saved = Doc::decode(&doc.encode().unwrap().bytes).unwrap();
        assert_eq!(saved.size(), (2, 4), "not rotated twice");
    }

    fn local_file(dir: &Path, name: &str, bytes: &[u8]) -> ImageFile {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let abs = path.to_str().unwrap().to_string();
        let mut fs = LocalFs::new();
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        ImageFile::new(origin, bytes.to_vec(), &dir.join("cache"))
    }

    #[test]
    fn save_backs_up_then_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(30, 20));
        let mut file = local_file(tmp.path(), "a.png", &original);
        let mut doc = Doc::decode(file.original()).unwrap();
        doc.pixelize([0, 0, 16, 16], 8).unwrap();
        let encoded = doc.encode().unwrap().bytes;
        let Saved::Written { backup, .. } =
            file.save(|| Ok(Encoded::plain(encoded.clone()))).unwrap()
        else {
            panic!("expected Written")
        };
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        assert!(backup.starts_with(tmp.path().join("cache/backup/local")));
        assert_eq!(std::fs::read(tmp.path().join("a.png")).unwrap(), encoded);
        // A second save in the same session goes over our own write and keeps the backup.
        doc.crop([0, 0, 10, 10]).unwrap();
        let second = doc.encode().unwrap().bytes;
        let Saved::Written { backup: again, .. } =
            file.save(|| Ok(Encoded::plain(second.clone()))).unwrap()
        else {
            panic!("expected Written")
        };
        assert_eq!(again, backup);
        assert_eq!(std::fs::read(tmp.path().join("a.png")).unwrap(), second);
        // Nothing left in the edits staging area.
        let edits = tmp.path().join("cache/edits");
        let left: Vec<_> = walk(&edits);
        assert!(left.is_empty(), "{left:?}");
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                } else {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Save order (spec §5): capability → backup (on disk before encoding starts) → encode →
    /// stage → replace.
    #[test]
    fn save_backs_up_before_encoding() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(30, 20));
        let mut file = local_file(tmp.path(), "a.png", &original);
        let backups = tmp.path().join("cache/backup");
        let edits = tmp.path().join("cache/edits");
        let mut seen_at_encode = Vec::new();
        let encoded = png_bytes(&gradient(30, 20).crop_imm(0, 0, 10, 10));
        let saved = file
            .save(|| {
                seen_at_encode = walk(&backups);
                assert!(walk(&edits).is_empty(), "nothing staged before encoding");
                Ok(Encoded::plain(encoded.clone()))
            })
            .unwrap();
        assert!(matches!(saved, Saved::Written { .. }), "{saved:?}");
        assert_eq!(
            seen_at_encode.len(),
            1,
            "backup existed when encoding began"
        );
        assert_eq!(std::fs::read(&seen_at_encode[0]).unwrap(), original);
        assert_eq!(std::fs::read(tmp.path().join("a.png")).unwrap(), encoded);
    }

    #[test]
    fn encode_failure_leaves_the_backup_and_the_original() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(30, 20));
        let mut file = local_file(tmp.path(), "a.png", &original);
        let err = file
            .save(|| Err(anyhow!("injected encoder failure")))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("injected encoder failure"), "{msg}");
        assert!(msg.contains("backed up at"), "{msg}");
        let backups = walk(&tmp.path().join("cache/backup"));
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
        assert_eq!(std::fs::read(tmp.path().join("a.png")).unwrap(), original);
        assert!(walk(&tmp.path().join("cache/edits")).is_empty());
    }

    fn window_for(file: ImageFile, leftover: Arc<Mutex<Option<Job>>>) -> ImageWindow {
        let doc = Doc::decode(file.original()).unwrap();
        ImageWindow {
            title: file.origin.display(),
            can_replace: true,
            doc,
            file: Arc::new(Mutex::new(file)),
            tex: None,
            tex_rev: None,
            sel: Selection::default(),
            block: DEFAULT_BLOCK,
            view: View::default(),
            status: Status::default(),
            job: None,
            outcome: Arc::default(),
            prompt: false,
            close_ok: false,
            close_when_done: false,
            leftover,
            painted: false,
            zoom: 1.0,
            map: None,
        }
    }

    /// Closing while a save runs never closes mid-write: the window stays until the save has
    /// reported. A window dropped anyway hands the in-flight save to `open`, which waits.
    #[test]
    fn close_waits_for_a_running_save() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(12, 12));
        let file = local_file(tmp.path(), "a.png", &original);
        let leftover: Arc<Mutex<Option<Job>>> = Arc::default();
        let mut w = window_for(file, leftover.clone());
        assert!(
            super::super::Window::close_requested(&mut w),
            "idle, clean: closes"
        );
        let (_tx, rx) = mpsc::channel();
        w.job = Some(Job {
            kind: JobKind::Save { close_after: false },
            rev: 0,
            rx,
        });
        assert!(!super::super::Window::close_requested(&mut w));
        assert!(w.close_when_done);
        assert!(w.status.text.contains("closes when the save is done"));
        assert!(!super::super::Window::close_requested(&mut w), "still not");
        drop(w);
        assert!(
            leftover.lock().unwrap().is_some(),
            "in-flight save handed over"
        );
    }

    #[test]
    fn save_refuses_a_changed_file_and_keeps_the_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(30, 20));
        let mut file = local_file(tmp.path(), "a.png", &original);
        // Someone else writes the file after it was opened.
        std::fs::write(tmp.path().join("a.png"), b"someone else's bytes").unwrap();
        let mut doc = Doc::decode(file.original()).unwrap();
        doc.pixelize([0, 0, 8, 8], 4).unwrap();
        let encoded = doc.encode().unwrap().bytes;
        let Saved::Refused { message, kept } =
            file.save(|| Ok(Encoded::plain(encoded.clone()))).unwrap()
        else {
            panic!("expected Refused")
        };
        assert!(message.contains("changed since it was opened"), "{message}");
        assert!(message.contains(kept.to_str().unwrap()), "{message}");
        assert_eq!(std::fs::read(&kept).unwrap(), encoded);
        assert_eq!(
            std::fs::read(tmp.path().join("a.png")).unwrap(),
            b"someone else's bytes"
        );
        assert_eq!(kept.extension().unwrap(), "png");
    }

    /// The local disk, posing as a server without `posix-rename@openssh.com`.
    struct NoReplace(LocalFs);

    impl RemoteFs for NoReplace {
        fn stat_opt(&mut self, p: &str) -> Result<Option<crate::remote::Stat>> {
            self.0.stat_opt(p)
        }
        fn lstat_opt(&mut self, p: &str) -> Result<Option<crate::remote::Stat>> {
            self.0.lstat_opt(p)
        }
        fn realpath(&mut self, p: &str) -> Result<String> {
            self.0.realpath(p)
        }
        fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
            self.0.read_head(p, max)
        }
        fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
            self.0.list_dir(p)
        }
        fn read_to(
            &mut self,
            p: &str,
            out: &mut dyn std::io::Write,
            progress: &mut dyn FnMut(u64),
        ) -> Result<u64> {
            self.0.read_to(p, out, progress)
        }
        fn create_from(
            &mut self,
            local: &Path,
            p: &str,
            mode: Option<u32>,
        ) -> std::result::Result<crate::remote::Uploaded, crate::remote::CreateError> {
            self.0.create_from(local, p, mode)
        }
        fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
            self.0.set_mode(p, mode)
        }
        fn set_owner(&mut self, p: &str, uid: u32, gid: u32) -> Result<()> {
            self.0.set_owner(p, uid, gid)
        }
        fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
            self.0.rename_over(from, to)
        }
        fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
            self.0.link_new(from, to)
        }
        fn remove(&mut self, p: &str) -> Result<()> {
            self.0.remove(p)
        }
        fn can_replace(&self) -> bool {
            false
        }
    }

    #[test]
    fn save_without_posix_rename_is_refused_before_upload_and_save_copy_works() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(20, 20));
        let path = tmp.path().join("a.png");
        std::fs::write(&path, &original).unwrap();
        let abs = path.to_str().unwrap().to_string();
        let mut fs = NoReplace(LocalFs::new());
        let at = fs.stat(&abs).unwrap();
        let origin = Origin::new(Box::new(fs), None, None, abs, at);
        let mut file = ImageFile::new(origin, original.clone(), &tmp.path().join("cache"));
        assert!(!file.can_replace());
        let mut doc = Doc::decode(file.original()).unwrap();
        doc.pixelize([0, 0, 8, 8], 4).unwrap();
        let encoded = doc.encode().unwrap().bytes;
        let err = file
            .save(|| Ok(Encoded::plain(encoded.clone())))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Save copy"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), original, "untouched");
        assert!(
            walk(&tmp.path().join("cache/backup")).is_empty(),
            "no backup"
        );
        let copy = file.save_copy(&encoded).unwrap();
        assert!(copy.ends_with("/a.edited.png"), "{copy}");
    }

    #[test]
    fn save_copy_writes_an_edited_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(12, 12));
        let mut file = local_file(tmp.path(), "shot.png", &original);
        let encoded = png_bytes(&gradient(6, 6));
        let first = file.save_copy(&encoded).unwrap();
        assert!(first.ends_with("/shot.edited.png"), "{first}");
        let second = file.save_copy(&encoded).unwrap();
        assert!(second.ends_with("/shot.edited-1.png"), "{second}");
        assert_eq!(
            std::fs::read(tmp.path().join("shot.png")).unwrap(),
            original
        );
        assert_eq!(std::fs::read(&first).unwrap(), encoded);
    }

    #[test]
    fn state_hook_applies_ops_in_order() {
        let mut doc = Doc::decode(&png_bytes(&gradient(40, 40))).unwrap();
        let mut sel = Selection::default();
        let mut block = DEFAULT_BLOCK;
        let hooks = Hooks {
            state: super::super::State::parse("rect=10,10,21,15;op=pixelize,crop;block=4").unwrap(),
            ..Hooks::default()
        };
        apply_state(&mut doc, &mut sel, &mut block, &hooks).unwrap();
        assert_eq!(block, 4);
        assert_eq!(doc.size(), (21, 15));
        assert!(doc.can_undo());
        let mut bad = Doc::decode(&png_bytes(&gradient(4, 4))).unwrap();
        let hooks = Hooks {
            state: super::super::State::parse("op=crop").unwrap(),
            ..Hooks::default()
        };
        assert!(apply_state(&mut bad, &mut sel, &mut block, &hooks).is_err());
    }
}
