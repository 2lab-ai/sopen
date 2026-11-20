//! The image window (spec §6): fit/zoom/pan, a rectangle in image pixels, Crop, Pixelize,
//! Cut / Copy / Paste through the system clipboard ([`clip`]), Undo/Redo, Save (backup first,
//! conflict-checked overwrite) and Save copy (a new sibling).
//!
//! [`Doc`] is the edit model (decode, ops, history, encode) and [`ImageFile`] the write side;
//! both are window-free, so the tests and the `SOPEN_AUTOSAVE` hook run the exact code the
//! window's buttons run.
//!
//! As the file browser's preview (02 §4.2, [`embed_preview`]) the window shows the picture fitted,
//! without toolbars, rows or prompt, and takes no input; [`dimensions`] and
//! [`Doc::decode_checked`] are the preview's budget checks (the header before the decode, a stop
//! at every read of it), [`fitted`] the picture it shows, made off the window's thread.

pub mod clip;

use super::fonts::{self, Face};
use super::{Hooks, Map, Op, Origin, Outcome, Selection, Status};
use crate::remote::WriteOutcome;
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, TextureHandle, Vec2};
use image::{
    AnimationDecoder, DynamicImage, ImageBuffer, ImageDecoder, ImageEncoder, ImageFormat,
    ImageReader, Pixel, Primitive,
};
use num_traits::ToPrimitive;
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Default pixelize block, in image pixels.
pub const DEFAULT_BLOCK: u32 = 16;
/// The window's title in its frame, in the manner of PC Shell's `File Edit`.
const TITLE: &str = "Image Edit";
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

/// The width and height an encoded image says it has, from its header alone (nothing is
/// decoded): what a preview checks against its budget before decoding (02 §4.3).
pub fn dimensions(bytes: &[u8]) -> Result<(u32, u32)> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("cannot read the image")?
        .into_dimensions()
        .context("cannot read the image's size")
}

/// The encoded bytes as a decoder reads them, asking `check` before every read: a decode stops
/// at its next read once `check` says to (a cancel, a preview's deadline). Decoders that read as
/// they decode (PNG, GIF, BMP, TIFF) stop in the middle of the image; one that takes its input
/// whole first stops before it decodes.
struct Checked<'a> {
    inner: Cursor<&'a [u8]>,
    check: &'a dyn Fn() -> Result<()>,
}

impl Checked<'_> {
    fn asked(&self) -> std::io::Result<()> {
        (self.check)().map_err(|e| std::io::Error::other(format!("{e:#}")))
    }
}

impl std::io::Read for Checked<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.asked()?;
        self.inner.read(buf)
    }
}

impl std::io::BufRead for Checked<'_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.asked()?;
        self.inner.fill_buf()
    }

    fn consume(&mut self, n: usize) {
        self.inner.consume(n);
    }
}

impl std::io::Seek for Checked<'_> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

#[cfg(test)]
thread_local! {
    static DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Tests: images decoded on this thread so far.
#[cfg(test)]
pub(crate) fn decodes() -> usize {
    DECODES.with(std::cell::Cell::get)
}

/// The longest side of the picture a preview shows ([`fitted`]): a side of the browser on a
/// large screen, at two pixels a point.
pub const PREVIEW_SIDE: u32 = 2048;

/// The picture a preview shows, made where the preview is prepared (off the window's thread):
/// the image scaled down to fit [`PREVIEW_SIDE`] — `None` when it already fits (it is shown as
/// it is). A 64-megapixel image made into a texture on the window's thread took over a second
/// (E3).
pub fn fitted(doc: &Doc) -> Option<image::RgbaImage> {
    let (w, h) = doc.size();
    (w > PREVIEW_SIDE || h > PREVIEW_SIDE)
        .then(|| doc.img.thumbnail(PREVIEW_SIDE, PREVIEW_SIDE).to_rgba8())
}

impl Doc {
    /// Decodes `bytes` (format by content, not by name), applies the Exif orientation and keeps
    /// the ICC profile and Exif for the save.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::decode_checked(bytes, &|| Ok(()))
    }

    /// [`Doc::decode`], asking `check` before every read of the decoder and after the decode:
    /// its error stops the decode and is the error returned (a cancel, a preview's deadline).
    pub fn decode_checked(bytes: &[u8], check: &dyn Fn() -> Result<()>) -> Result<Self> {
        let decoded = Self::decode_from(bytes, check);
        // A decode cut short by `check` fails with an I/O error: `check`'s own is the reason.
        check()?;
        decoded
    }

    fn decode_from(bytes: &[u8], check: &dyn Fn() -> Result<()>) -> Result<Self> {
        #[cfg(test)]
        DECODES.with(|n| n.set(n.get() + 1));
        let checked = Checked {
            inner: Cursor::new(bytes),
            check,
        };
        let reader = ImageReader::new(checked)
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

    /// The pixels of `rect` — the whole image when `None` — as 8-bit RGBA, for the clipboard.
    /// Reads only, so a read-only image can be copied too.
    pub fn copy(&self, rect: Option<[u32; 4]>) -> Result<image::RgbaImage> {
        let [x, y, w, h] = match rect {
            Some(rect) => {
                self.check_rect(rect)?;
                rect
            }
            None => {
                let (w, h) = self.size();
                [0, 0, w, h]
            }
        };
        Ok(self.img.crop_imm(x, y, w, h).to_rgba8())
    }

    /// Cut: the pixels of `rect` (returned, for the clipboard), and `rect` left in
    /// `background`. One undo step.
    pub fn cut(
        &mut self,
        rect: Option<[u32; 4]>,
        background: clip::Colour,
    ) -> Result<(image::RgbaImage, String)> {
        self.editable()?;
        let rect = rect.context("draw a rectangle first")?;
        let cut = self.copy(Some(rect))?;
        let mut next = self.img.clone();
        clip::fill(&mut next, rect, background);
        self.commit(next);
        Ok((cut, format!("cut {}×{}", rect[2], rect[3])))
    }

    /// Paste a picture into `rect` as `fit` says (see [`clip::place`]); without a rectangle,
    /// unscaled at the top-left, cut off at the image's edges. Returns the status line and
    /// the area the picture covers (the selection after a paste without one). One undo step.
    pub fn paste_image(
        &mut self,
        rect: Option<[u32; 4]>,
        picture: &image::RgbaImage,
        fit: clip::Fit,
        background: clip::Colour,
    ) -> Result<(String, [u32; 4])> {
        self.editable()?;
        let (pw, ph) = picture.dimensions();
        if pw == 0 || ph == 0 {
            bail!("the clipboard image is empty");
        }
        let (rect, fit) = match rect {
            Some(rect) => {
                self.check_rect(rect)?;
                (rect, fit)
            }
            None => {
                let (w, h) = self.size();
                ([0, 0, w, h], clip::Fit::OneToOne)
            }
        };
        let mut next = self.img.clone();
        let at = clip::place(&mut next, rect, picture, fit, background);
        self.commit(next);
        let line = format!(
            "pasted image {pw}×{ph} → {}×{} ({})",
            at[2],
            at[3],
            fit.name()
        );
        Ok((line, at))
    }

    /// Paste text into `rect`, wrapped to its width and sized to fill it, in `colour`; without
    /// a rectangle, at the top-left in letters a tenth of the image's height (at least 12 px),
    /// wrapped to the image's width. Drawn in `hangul` (an installed font with Hangul, if there
    /// is one), then egui's built-in font. Returns the status line and the text's block (the
    /// selection after a paste without a rectangle). One undo step.
    pub fn paste_text(
        &mut self,
        rect: Option<[u32; 4]>,
        text: &str,
        colour: clip::Colour,
        hangul: Option<&Face>,
    ) -> Result<(String, [u32; 4])> {
        self.editable()?;
        if text.trim().is_empty() {
            bail!("the clipboard text is blank");
        }
        let (rect, size) = match rect {
            Some(rect) => {
                self.check_rect(rect)?;
                (rect, clip::TextSize::Fill)
            }
            None => {
                let (w, h) = self.size();
                let px = (h as f32 / 10.0).max(12.0);
                ([0, 0, w, h], clip::TextSize::Px(px))
            }
        };
        let faces: Vec<&Face> = hangul.into_iter().chain([fonts::fallback()]).collect();
        let mut next = self.img.clone();
        let drawn = clip::draw_text(&mut next, rect, text, colour, &faces, size);
        self.commit(next);
        // Counted as drawn: composed (a decomposed Korean syllable is one character, not three).
        let mut line = format!("pasted text ({} chars)", clip::composed(text).count());
        match (drawn.missing, hangul.is_some()) {
            (0, _) => {}
            (_, false) => line.push_str(" · no Hangul font found, so Hangul may not render"),
            (n, true) => line.push_str(&format!(
                " · {} without a glyph in any font found",
                plural(n, "character")
            )),
        }
        Ok((line, drawn.area))
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
    /// Encoded edits waiting to be uploaded, and refused ones: `<cache>/.edits/<host>/<abs>`.
    edits: PathBuf,
    /// `<cache>` (backups go to `<cache>/.backup/<host>/<abs>.<stamp>`).
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

/// Closing was asked for while a save runs.
const CLOSES_AFTER_SAVE: &str = "the window closes when the save is done";

/// Why Save cannot replace the file on this server (spec §5): Save copy still works.
pub const CANNOT_REPLACE: &str = "this server cannot replace a file in one step (no \
     posix-rename@openssh.com) — Save copy writes the edit next to the original instead";

/// The one line for a write that failed, keeping the edit. A request that got no reply is
/// "outcome unknown — check <path>", never "not saved". The hex window says it the same way.
pub(super) fn failed(e: &anyhow::Error, staged: &Path) -> anyhow::Error {
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
        let edits = crate::cache::edits_path(cache_root, origin.host(), &origin.abs);
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

    /// The encoded edit in a new owner-only file next to where edits are kept (never over or
    /// through an existing one).
    fn stage(&self, encoded: &[u8], tag: &str) -> Result<PathBuf> {
        let (path, mut file) = super::create_keep(&self.edits, tag)?;
        if let Err(e) = file.write_all(encoded) {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("cannot write {}", path.display()));
        }
        Ok(path)
    }

    /// The local backup of the original, written once per session before the first overwrite.
    fn ensure_backup(&mut self) -> Result<PathBuf> {
        if let Some(path) = &self.backup {
            return Ok(path.clone());
        }
        let first = crate::cache::backup_path(
            &self.cache_root,
            self.origin.host(),
            &self.origin.abs,
            SystemTime::now(),
        );
        // Another session's backup from the same second keeps its name: this one adds `-<random>`.
        let (path, mut file) = crate::cache::create_unique(|attempt| {
            if attempt == 0 {
                return first.clone();
            }
            let mut name = first.file_name().unwrap_or_default().to_os_string();
            name.push(format!("-{}", &crate::remote::random_hex()[..8]));
            first.with_file_name(name)
        })?;
        if let Err(e) = file.write_all(self.original.as_slice()) {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("cannot write the backup {}", path.display()));
        }
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
                    // A name of its own first, so the move cannot replace anyone's file.
                    let (kept, _) = super::create_keep(&self.edits, "conflict")?;
                    if let Err(e) = std::fs::rename(&staged, &kept) {
                        let _ = std::fs::remove_file(&kept);
                        return Err(e).with_context(|| {
                            format!("cannot keep the edited copy at {}", kept.display())
                        });
                    }
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
        let wanted = super::edited_name(&name);
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
/// `notices` (where earlier copies' unsaved edits went) are shown whole in the window's rows.
pub fn open(file: ImageFile, hooks: &Hooks, notices: Vec<String>) -> Result<Outcome> {
    let mut doc = Doc::decode(file.original())?;
    let mut sel = Selection::default();
    let mut block = DEFAULT_BLOCK;
    apply_state(&mut doc, &mut sel, &mut block, hooks)?;
    if hooks.autosave {
        return autosave(file, &doc, hooks);
    }
    let title = file.origin.display();
    let (w, h) = doc.size();
    // Before the owner acts, on the terminal too: what Save will not keep and what Save cannot
    // do here (`notices` are already there: the caller printed them).
    if let Some(line) = doc.not_kept_line() {
        eprintln!("sopen: {line}");
    }
    if !file.can_replace() {
        eprintln!("sopen: {CANNOT_REPLACE}");
    }
    let mut window = ImageWindow::new(file, doc, sel, block, notices);
    window.ask_close = hooks.state.ask_close;
    let outcome = window.outcome.clone();
    let leftover = window.leftover.clone();
    // A window that shows the image near 1:1 where the screen allows (the two toolbar rows,
    // the frame, the status bar and the F-key row around it); eframe clamps to it.
    let size = [
        (w as f32 + 2.0 * super::chrome::CELL.x).clamp(640.0, 1400.0),
        (h as f32 + 121.0).clamp(420.0, 940.0),
    ];
    let run = super::run_window(&title, size, window, hooks);
    // Never exit with a write in flight without its outcome on the terminal.
    if let Some(job) = leftover.lock().unwrap().take() {
        eprintln!("sopen: the window closed during a save; waiting for it to finish");
        let result = job
            .rx
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the save stopped without a result")));
        let _ = report(&result, &mut outcome.lock().unwrap());
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

/// A picture for the clipboard: egui keeps premultiplied alpha, so a pixel that is neither
/// opaque nor fully transparent is rounded on the way (8-bit premultiplied).
fn color_image(picture: &image::RgbaImage) -> egui::ColorImage {
    let (w, h) = picture.dimensions();
    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], picture.as_raw())
}

/// A pasted picture (egui-winit hands it over premultiplied) as straight 8-bit RGBA.
fn rgba_image(pasted: &egui::ColorImage) -> image::RgbaImage {
    let [w, h] = pasted.size;
    let bytes = pasted
        .pixels
        .iter()
        .flat_map(|c| c.to_srgba_unmultiplied())
        .collect();
    image::RgbaImage::from_raw(w as u32, h as u32, bytes).expect("w×h×4 bytes")
}

/// Prints a finished save to the terminal, records it in `outcome` and returns the window's
/// one-line status and what must be shown whole (the disclosure rows): write notes, what was
/// not kept, a refusal and where the edit went. Also used for a save still in flight when the
/// window went away.
fn report(result: &Result<JobDone>, outcome: &mut Outcome) -> (Status, Vec<String>) {
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
            let status = Status::info(format!(
                "saved {remote} · original backed up at {}",
                backup.display()
            ));
            (status, notes.iter().chain(not_kept).cloned().collect())
        }
        Ok(JobDone::Saved(Saved::Refused { message, .. })) => {
            outcome.conflict = true;
            outcome.refused = Some(message.clone());
            eprintln!("sopen: {message}");
            (
                Status::error("not saved: the file changed"),
                vec![message.clone()],
            )
        }
        Ok(JobDone::Copy(path, not_kept)) => {
            let line = super::with_notes(&format!("saved a copy at {path}"), not_kept);
            println!("{line}");
            (
                Status::info(format!("saved a copy at {path}")),
                not_kept.clone(),
            )
        }
        Err(e) => {
            eprintln!("sopen: {e:#}");
            (Status::error("save failed"), vec![format!("{e:#}")])
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
    /// Lines from open that must be read whole and stay (see [`super::disclosures`]).
    opened: Vec<String>,
    /// What the last write reported that must be read whole, and the next such report, held
    /// while the button is down.
    notices: Vec<String>,
    pending_notices: Option<Vec<String>>,
    /// How Cmd/Ctrl+V lays a picture into the rectangle.
    fit: clip::Fit,
    /// What Cut leaves and Fit puts around a pasted picture (opaque; see `transparent`).
    background: Color32,
    /// The background is transparent instead — offered only when the image has alpha.
    transparent: bool,
    /// The image's pixel type has alpha (never changes: the edits keep the pixel type).
    has_alpha: bool,
    /// Pasted text's colour.
    text_colour: Color32,
    /// A cut or paste whose key is still down (see [`Held`]).
    held: Option<Held>,
    /// `SOPEN_STATE` `close=ask`: closing is asked for on the first pass.
    ask_close: bool,
    /// A preview in the file browser (02 §4.2): the picture fitted in a single-lined frame, no
    /// toolbars, status row or disclosures, and no input of its own.
    preview: bool,
    /// The picture a preview shows when the image is larger ([`fitted`], made off this thread);
    /// the hand-over shows the image itself again.
    shown: Option<image::RgbaImage>,
}

/// A Cut or Paste whose key has not been let go yet. egui-winit 0.37 drops winit's key-repeat
/// flag (`repeat: _` in its `on_keyboard_input`), so a Cmd/Ctrl+X or V held a moment too long
/// arrives as the same event again: a second Cut would put the blank the first one left on the
/// clipboard, a second Paste would draw over the first. While one is held, more of its events
/// are that repeat and are ignored; it ends when its key comes up (winit forwards that key-up
/// even with Cmd held on macOS, `platform_impl/macos/app.rs`), when Cmd/Ctrl comes up, or when
/// the window loses focus (where a key-up can go missing). The next press is a new cut or
/// paste, whatever it pastes and wherever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    Cut,
    Paste,
}

impl Held {
    /// The keys that make it: Cmd/Ctrl+X or V, or a keyboard's own Cut or Paste key.
    fn keys(self) -> [Key; 2] {
        match self {
            Held::Cut => [Key::X, Key::Cut],
            Held::Paste => [Key::V, Key::Paste],
        }
    }
}

impl Drop for ImageWindow {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            *self.leftover.lock().unwrap() = Some(job);
        }
    }
}

impl ImageWindow {
    /// The window as `open` makes it. `notices` (where earlier copies' unsaved edits went)
    /// and what Save cannot do here are read whole, in the always-visible rows, for the whole
    /// session; the one-line status is for short news.
    fn new(file: ImageFile, doc: Doc, sel: Selection, block: u32, notices: Vec<String>) -> Self {
        let can_replace = file.can_replace();
        // Looked up only when text is pasted, but enumerating the fonts takes a moment.
        fonts::prefetch();
        let has_alpha = doc.img.color().has_alpha();
        let opened = notices
            .into_iter()
            .chain((!can_replace).then(|| CANNOT_REPLACE.to_string()))
            .collect();
        Self {
            title: file.origin.display(),
            can_replace,
            doc,
            file: Arc::new(Mutex::new(file)),
            tex: None,
            tex_rev: None,
            sel,
            block,
            view: View::default(),
            status: Status::default(),
            job: None,
            outcome: Arc::default(),
            prompt: false,
            close_ok: false,
            close_when_done: false,
            leftover: Arc::default(),
            painted: false,
            zoom: 1.0,
            map: None,
            opened,
            notices: Vec::new(),
            pending_notices: None,
            fit: clip::Fit::default(),
            background: Color32::WHITE,
            transparent: false,
            has_alpha,
            text_colour: Color32::BLACK,
            held: None,
            ask_close: false,
            preview: false,
            shown: None,
        }
    }

    /// The window as a preview ([`embed_preview`]).
    fn previewing(file: ImageFile, doc: Doc, shown: Option<image::RgbaImage>) -> Self {
        let mut window = Self::new(file, doc, Selection::default(), DEFAULT_BLOCK, Vec::new());
        window.preview = true;
        window.shown = shown;
        window
    }

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

    /// The background as the edits take it: transparent only where the image has alpha.
    fn background(&self) -> clip::Colour {
        let [r, g, b, _] = self.background.to_srgba_unmultiplied();
        let a = if self.transparent && self.has_alpha {
            0
        } else {
            255
        };
        [r, g, b, a]
    }

    /// Cmd/Ctrl+C: the rectangle, or the whole image, to the system clipboard (eframe hands the
    /// picture to egui-winit, which writes it with arboard).
    fn copy(&mut self, ctx: &egui::Context) {
        match self.doc.copy(self.rect()) {
            Ok(picture) => {
                let (w, h) = picture.dimensions();
                ctx.copy_image(color_image(&picture));
                self.status = Status::info(format!("copied {w}×{h}"));
            }
            Err(e) => self.report(Err(e)),
        }
    }

    /// Cmd/Ctrl+X: copy, then the rectangle in the background colour.
    fn cut(&mut self, ctx: &egui::Context) {
        match self.doc.cut(self.rect(), self.background()) {
            Ok((picture, line)) => {
                ctx.copy_image(color_image(&picture));
                self.report(Ok(line));
            }
            Err(e) => self.report(Err(e)),
        }
    }

    /// A paste's result. One made without a rectangle selects what it covers; one made into a
    /// rectangle keeps it.
    fn pasted(&mut self, done: Result<(String, [u32; 4])>) {
        let had_rect = self.rect().is_some();
        if let Ok((_, area)) = &done {
            if !had_rect && area[2] > 0 && area[3] > 0 {
                self.sel.set(*area);
            }
        }
        self.report(done.map(|(line, _)| line));
    }

    /// Cmd/Ctrl+C, X and V. egui-winit turns these shortcuts into these events — never into
    /// key presses — and for V reads the system clipboard: its text when it holds text, else
    /// its image. Not while a text field has the keyboard (its own copy and paste) or the close
    /// prompt is up. The events are taken in order with the key-ups, focus and modifier changes
    /// that end a held one ([`Held`]) — also while the shortcuts themselves are off, so that a
    /// key let go meanwhile is not missed.
    fn clipboard(&mut self, ctx: &egui::Context) {
        let off = self.prompt || ctx.text_edit_focused();
        let events: Vec<egui::Event> = ctx.input(|i| {
            i.events
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        egui::Event::Copy
                            | egui::Event::Cut
                            | egui::Event::Paste(_)
                            | egui::Event::PasteImage(_)
                            | egui::Event::Key { pressed: false, .. }
                            | egui::Event::WindowFocused(false)
                            | egui::Event::ModifiersChanged(_)
                    )
                })
                .cloned()
                .collect()
        });
        for event in events {
            let action = match &event {
                egui::Event::Key { key, .. } => {
                    if self.held.is_some_and(|h| h.keys().contains(key)) {
                        self.held = None;
                    }
                    continue;
                }
                egui::Event::WindowFocused(_) => {
                    self.held = None;
                    continue;
                }
                egui::Event::ModifiersChanged(mods) => {
                    if !mods.command {
                        self.held = None;
                    }
                    continue;
                }
                egui::Event::Cut => Some(Held::Cut),
                egui::Event::Paste(_) | egui::Event::PasteImage(_) => Some(Held::Paste),
                _ => None,
            };
            if off || (action.is_some() && self.held == action) {
                continue;
            }
            match &event {
                egui::Event::Copy => self.copy(ctx),
                egui::Event::Cut => self.cut(ctx),
                egui::Event::Paste(text) => {
                    let [r, g, b, a] = self.text_colour.to_srgba_unmultiplied();
                    let done =
                        self.doc
                            .paste_text(self.rect(), text, [r, g, b, a], fonts::hangul());
                    self.pasted(done);
                }
                egui::Event::PasteImage(image) => {
                    let picture = rgba_image(image);
                    let done =
                        self.doc
                            .paste_image(self.rect(), &picture, self.fit, self.background());
                    self.pasted(done);
                }
                _ => {}
            }
            if action.is_some() {
                self.held = action;
            }
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
        let (status, shown) = report(&result, &mut self.outcome.lock().unwrap());
        self.status = status;
        self.pending_notices = Some(shown);
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
        // An edit made while the save ran is not in what it wrote.
        let newer = self.doc.rev() != job.rev;
        let waited = std::mem::take(&mut self.close_when_done);
        if (close_after && !newer) || (waited && !self.doc.unsaved()) {
            self.close_ok = true;
            super::request_close(ctx);
        } else if close_after || waited {
            // Closing was asked for, but the save did not land or the picture changed since it
            // started: say so first (the status is cut to one line) and ask what to do.
            if close_after && newer {
                self.status =
                    Status::info(format!("newer edits are not saved · {}", self.status.text));
            }
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
        // F5 is PC Shell's Save, as Cmd/Ctrl+S — a plain key: not while a field (the block
        // size) has the typing.
        if !ctx.egui_wants_keyboard_input() && super::hex::bare_key(ctx, Key::F5) {
            self.start(ctx, JobKind::Save { close_after: false });
        }
        if plain_key(ctx, Key::C) {
            self.crop();
        }
        if plain_key(ctx, Key::P) {
            self.pixelize();
        }
        if self.sel.is_set() && plain_key(ctx, Key::Escape) {
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
        // Wraps on a narrow window so Save stays reachable; its height depends only on the
        // window width (button labels never change, nothing in it comes and goes — the busy
        // spinner is in the status bar), so it cannot move the image mid-drag.
        ui.horizontal_wrapped(|ui| {
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
        });
    }

    /// The second toolbar row: how Cmd/Ctrl+V lays a picture into the rectangle, and the
    /// background and text colours. One line from the smallest window up, and the same for the
    /// whole session (the transparent choice depends only on the image's pixel type), so it
    /// never moves the image.
    fn paste_row(&mut self, ui: &mut egui::Ui) {
        use egui::color_picker::{color_edit_button_srgba, Alpha};
        let editable = self.doc.read_only.is_none();
        ui.add_enabled_ui(editable, |ui| {
            ui.horizontal(|ui| {
                // Narrow swatches and no separators: the row, transparent choice included,
                // fits the smallest window (480 px).
                ui.spacing_mut().interact_size.x = 24.0;
                ui.label("Paste")
                    .on_hover_text("⌘/Ctrl+V: how a picture fills the rectangle");
                for (fit, label, hint) in [
                    (clip::Fit::Stretch, "Stretch", "scaled to the rectangle"),
                    (
                        clip::Fit::Fit,
                        "Fit",
                        "scaled to fit inside it, aspect kept; the rest gets the background",
                    ),
                    (
                        clip::Fit::OneToOne,
                        "1:1",
                        "unscaled at its top-left, cut off at its edges",
                    ),
                ] {
                    if super::chrome::choice(ui, self.fit == fit, label)
                        .on_hover_text(hint)
                        .clicked()
                    {
                        self.fit = fit;
                    }
                }
                ui.label("Background");
                color_edit_button_srgba(ui, &mut self.background, Alpha::Opaque)
                    .on_hover_text("what ⌘/Ctrl+X leaves, and what Fit puts around a picture");
                if self.has_alpha {
                    ui.checkbox(&mut self.transparent, "transparent");
                }
                ui.label("Text");
                color_edit_button_srgba(ui, &mut self.text_colour, Alpha::Opaque)
                    .on_hover_text("the colour of pasted text");
            });
        });
    }

    /// `zoom` is the previous pass's (the image is laid out after the bar).
    /// One line whatever it says (see [`super::one_line`]): the image above must not move. The
    /// busy spinner sits here, before "saving…", rather than in the wrapping toolbar: kept out
    /// of sight there, its slot wrapped alone onto an empty second row at some widths.
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
            ui.separator();
            if self.job.is_some() {
                // No taller than the status text beside it, so the bar keeps its height.
                let size = ui.text_style_height(&egui::TextStyle::Body);
                ui.add(egui::Spinner::new().size(size));
            }
            self.status.show(ui);
        });
    }

    fn texture(&mut self, ctx: &egui::Context) -> egui::TextureId {
        if self.tex_rev != Some(self.doc.rev()) || self.tex.is_none() {
            let max = ctx.input(|i| i.max_texture_side) as u32;
            let (w, h) = self.doc.size();
            // A picture bigger than the GPU's texture limit is shown downscaled; edits still
            // address the full-resolution pixels. A preview shows the picture made for it.
            let fitted = self.shown.as_ref().filter(|_| self.preview);
            let shown = if let Some(fitted) = fitted {
                fitted.clone()
            } else if w > max || h > max {
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
        // The window's frame; the picture on black inside it.
        let outer = ui.max_rect();
        // Single-lined in a preview: it does not have the keyboard.
        let panel = super::chrome::frame(&ui.painter_at(outer), outer, TITLE, !self.preview);
        let src = self.bounds();
        let fit_scale = (panel.width() / src.x).min(panel.height() / src.y);
        // A preview takes no click, drag or wheel: it is only looked at.
        let sense = if self.preview {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        };
        let resp = ui.allocate_rect(panel, sense);
        if !self.preview {
            self.navigate(ui, &resp, panel, fit_scale);
        }
        let scale = self.view.scale.unwrap_or(fit_scale);
        let center = self.view.center.unwrap_or((src / 2.0).to_pos2());
        let disp = Rect::from_min_size(panel.center() - center.to_vec2() * scale, src * scale);
        let map = Map { disp, src };
        let tex = self.texture(&ctx);
        let painter = ui.painter_at(panel);
        painter.rect_filled(panel, 0.0, super::chrome::PALETTE.picture_back);
        painter.image(
            tex,
            disp,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if self.doc.read_only.is_none() && !self.prompt && !self.preview {
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
        let id = egui::Id::unique("unsaved");
        let modal = super::chrome::dialog(ctx, id, "Unsaved changes", |ui| {
            ui.set_width(360.0);
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
                    super::request_close(ctx);
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
        if self.preview {
            // The picture alone, fitted (02 §4.2): no keys, clipboard, toolbars, rows or prompt.
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.zoom = self.central(ui));
            return;
        }
        if std::mem::take(&mut self.ask_close) {
            // `SOPEN_STATE` `close=ask`: as if closing were asked for (the prompt's receipt).
            self.close_requested();
        }
        self.poll_job(&ctx);
        self.keys(&ctx);
        self.clipboard(&ctx);
        egui::Panel::top("tools").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            self.paste_row(ui);
            ui.add_space(1.0);
        });
        // The F-key row, the window's last (on its own: the browser around it has the row).
        super::own_keys(ui, super::chrome::IMAGE_KEYS);
        // A report that lands while the button is down (a drag, or a press that may become one)
        // waits until it is let go: its row would move the image under the pointer.
        let held = self.sel.dragging() || ctx.input(|i| i.pointer.primary_down());
        if !held {
            if let Some(next) = self.pending_notices.take() {
                self.notices = next;
            }
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        let warn = ui.visuals().warn_fg_color;
        let error = ui.visuals().error_fg_color;
        let lines: Vec<(String, Color32)> = match &self.doc.read_only {
            Some(reason) => Some(format!("read-only: {reason}")),
            None => self.doc.not_kept_line(),
        }
        .map(|l| (l, warn))
        .into_iter()
        .chain(
            self.opened
                .iter()
                .chain(&self.notices)
                .map(|n| (n.clone(), error)),
        )
        .collect();
        if !lines.is_empty() {
            egui::Panel::bottom("disclosures").show(ui, |ui| {
                ui.add_space(2.0);
                super::disclosures(ui, &lines);
                ui.add_space(2.0);
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.zoom = self.central(ui));
        self.close_prompt(&ctx);
    }

    fn ready(&self) -> bool {
        self.painted && self.job.is_none()
    }

    fn keys(&self) -> &'static [super::chrome::FKey] {
        super::chrome::IMAGE_KEYS
    }

    /// A preview (02 §4.2) shows the picture fitted, without bars, and takes no input; `false`
    /// (the hand-over) makes it the image window again, on the picture it already decoded (the
    /// texture is made again at full size).
    fn set_preview(&mut self, on: bool) {
        if self.preview && !on {
            self.tex_rev = None;
            self.shown = None;
        }
        self.preview = on;
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok || self.preview {
            return true;
        }
        if self.job.is_some() {
            // Never close mid-write: the window closes once the save has reported. Said in the
            // rows (read whole on a narrow window), after any report still held.
            self.close_when_done = true;
            self.status = Status::info("saving…");
            let mut next = self
                .pending_notices
                .take()
                .unwrap_or_else(|| self.notices.clone());
            if !next.iter().any(|n| n == CLOSES_AFTER_SAVE) {
                next.push(CLOSES_AFTER_SAVE.to_string());
            }
            self.pending_notices = Some(next);
            return false;
        }
        if !self.doc.unsaved() {
            return true;
        }
        self.prompt = true;
        false
    }
}

/// The image window inside another window (the file browser): the window [`open`] shows,
/// without the receipt hooks. `notices` are shown whole in its rows, as `open` shows them.
pub fn embed(file: ImageFile, doc: Doc, notices: Vec<String>) -> super::Embedded {
    let window = ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, notices);
    let (outcome, leftover) = (window.outcome.clone(), window.leftover.clone());
    super::Embedded {
        window: Box::new(window),
        settle: Box::new(move |wait| settle(&leftover, &outcome, wait)),
    }
}

/// The image window as a preview in the file browser (02 §4.2): told it is one before its first
/// pass, showing `shown` ([`fitted`], made where the preview was prepared) when the image is
/// larger. Its hand-over is [`super::Window::set_preview`] `false`.
pub fn embed_preview(
    file: ImageFile,
    doc: Doc,
    shown: Option<image::RgbaImage>,
) -> super::Embedded {
    let window = ImageWindow::previewing(file, doc, shown);
    let (outcome, leftover) = (window.outcome.clone(), window.leftover.clone());
    super::Embedded {
        window: Box::new(window),
        settle: Box::new(move |wait| settle(&leftover, &outcome, wait)),
    }
}

/// A save the window left running when it went away, as [`open`] waits for it: with `wait`
/// waited for, else taken only once it has finished; printed and counted either way. The
/// window's outcome once none runs.
fn settle(leftover: &Mutex<Option<Job>>, outcome: &Mutex<Outcome>, wait: bool) -> Option<Outcome> {
    let mut slot = leftover.lock().unwrap();
    if let Some(job) = slot.as_ref() {
        let stopped = || Err(anyhow!("the save stopped without a result"));
        let result = if wait {
            eprintln!("sopen: the window closed during a save; waiting for it to finish");
            job.rx.recv().unwrap_or_else(|_| stopped())
        } else {
            match job.rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => stopped(),
            }
        };
        *slot = None;
        let _ = report(&result, &mut outcome.lock().unwrap());
    }
    Some(outcome.lock().unwrap().clone())
}

#[cfg(test)]
mod input_tests;

#[cfg(test)]
mod robustness_tests;

#[cfg(test)]
mod preview_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::{LocalFs, RemoteFs};
    use crate::testutil::NoReplace;
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
        assert!(
            backup.starts_with(crate::cache::backup_root(&tmp.path().join("cache")).join("local"))
        );
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
        let edits = crate::cache::edits_root(&tmp.path().join("cache"));
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
        let backups = crate::cache::backup_root(&tmp.path().join("cache"));
        let edits = crate::cache::edits_root(&tmp.path().join("cache"));
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
        let backups = walk(&crate::cache::backup_root(&tmp.path().join("cache")));
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
        assert_eq!(std::fs::read(tmp.path().join("a.png")).unwrap(), original);
        assert!(walk(&crate::cache::edits_root(&tmp.path().join("cache"))).is_empty());
    }

    fn window_for(file: ImageFile, leftover: Arc<Mutex<Option<Job>>>) -> ImageWindow {
        let doc = Doc::decode(file.original()).unwrap();
        let mut w = ImageWindow::new(file, doc, Selection::default(), DEFAULT_BLOCK, Vec::new());
        w.leftover = leftover;
        w
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
        assert!(!super::super::Window::close_requested(&mut w), "still not");
        // Said once, in the rows, however often closing is asked for.
        assert_eq!(w.pending_notices, Some(vec![CLOSES_AFTER_SAVE.to_string()]));
        drop(w);
        assert!(
            leftover.lock().unwrap().is_some(),
            "in-flight save handed over"
        );
    }

    /// Save (or Save copy) chosen in the close prompt closes the window only when what it wrote
    /// is the picture on screen: an edit made while it ran keeps the window open, says so, and
    /// asks again.
    #[test]
    fn close_after_save_waits_for_edits_made_during_the_save() {
        for copy in [false, true] {
            for newer in [false, true] {
                let case = format!("copy={copy} newer={newer}");
                let tmp = tempfile::tempdir().unwrap();
                let file = local_file(tmp.path(), "a.png", &png_bytes(&gradient(12, 12)));
                let mut w = window_for(file, Arc::default());
                w.doc.pixelize([0, 0, 4, 4], 2).unwrap();
                let (tx, rx) = mpsc::channel();
                w.job = Some(Job {
                    kind: if copy {
                        JobKind::SaveCopy { close_after: true }
                    } else {
                        JobKind::Save { close_after: true }
                    },
                    rev: w.doc.rev(),
                    rx,
                });
                if newer {
                    w.doc.pixelize([4, 4, 8, 8], 2).unwrap();
                }
                let done = if copy {
                    JobDone::Copy("box:/a.edited.png".to_string(), Vec::new())
                } else {
                    JobDone::Saved(Saved::Written {
                        remote: "box:/a.png".to_string(),
                        backup: PathBuf::from("/backup/a.png"),
                        notes: Vec::new(),
                        not_kept: Vec::new(),
                    })
                };
                tx.send(Ok(done)).unwrap();
                w.poll_job(&egui::Context::default());
                assert_eq!(w.close_ok, !newer, "{case}");
                assert_eq!(w.prompt, newer, "{case}: asks again");
                assert_eq!(
                    w.doc.unsaved(),
                    newer,
                    "{case}: the newer edit is still unsaved"
                );
                if newer {
                    assert!(
                        w.status.text.starts_with("newer edits are not saved"),
                        "{case}: {}",
                        w.status.text
                    );
                    // Cancel, then close again: still asked, never closed with the edit unsaved.
                    w.prompt = false;
                    assert!(!super::super::Window::close_requested(&mut w), "{case}");
                    assert!(
                        w.prompt && !w.close_ok,
                        "{case}: asks again on a second close"
                    );
                }
            }
        }
    }

    /// A close asked for during a plain Save / Save copy (not one chosen in the prompt) waits
    /// for it, then closes — unless an edit was made meanwhile: then it asks.
    #[test]
    fn a_close_during_a_plain_save_closes_after_it_unless_there_are_newer_edits() {
        for copy in [false, true] {
            for newer in [false, true] {
                let case = format!("copy={copy} newer={newer}");
                let tmp = tempfile::tempdir().unwrap();
                let file = local_file(tmp.path(), "a.png", &png_bytes(&gradient(12, 12)));
                let mut w = window_for(file, Arc::default());
                w.doc.pixelize([0, 0, 4, 4], 2).unwrap();
                let (tx, rx) = mpsc::channel();
                w.job = Some(Job {
                    kind: if copy {
                        JobKind::SaveCopy { close_after: false }
                    } else {
                        JobKind::Save { close_after: false }
                    },
                    rev: w.doc.rev(),
                    rx,
                });
                assert!(!super::super::Window::close_requested(&mut w), "{case}");
                assert!(w.close_when_done, "{case}");
                if newer {
                    w.doc.pixelize([4, 4, 8, 8], 2).unwrap();
                }
                let done = if copy {
                    JobDone::Copy("box:/a.edited.png".to_string(), Vec::new())
                } else {
                    JobDone::Saved(Saved::Written {
                        remote: "box:/a.png".to_string(),
                        backup: PathBuf::from("/backup/a.png"),
                        notes: Vec::new(),
                        not_kept: Vec::new(),
                    })
                };
                tx.send(Ok(done)).unwrap();
                w.poll_job(&egui::Context::default());
                assert_eq!(w.close_ok, !newer, "{case}");
                assert_eq!(w.prompt, newer, "{case}: asks");
                assert_eq!(w.doc.unsaved(), newer, "{case}");
            }
        }
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
            walk(&crate::cache::backup_root(&tmp.path().join("cache"))).is_empty(),
            "no backup"
        );
        let copy = file.save_copy(&encoded).unwrap();
        assert!(copy.ends_with("/a.edited.png"), "{copy}");
    }

    /// Staging an edit and keeping a refused one never write over (or through) a file that
    /// already has the name: here, files planted under every name a second-precision stamp
    /// could give in the next three seconds.
    #[test]
    fn save_never_writes_over_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(30, 20));
        let mut file = local_file(tmp.path(), "a.png", &original);
        std::fs::write(tmp.path().join("a.png"), b"someone else's bytes").unwrap();
        let now = SystemTime::now();
        let mut planted = Vec::new();
        for k in 0..3 {
            let at = now + std::time::Duration::from_secs(k);
            let stamp = crate::cache::utc_stamp(at);
            let backup = crate::cache::backup_path(
                &file.cache_root,
                file.origin.host(),
                &file.origin.abs,
                at,
            );
            let names = ["edited", "conflict"]
                .map(|tag| file.edits.with_file_name(format!("a.{tag}-{stamp}.png")));
            for (n, path) in names.into_iter().chain([backup]).enumerate() {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let body = format!("someone's file {n} at {k}");
                std::fs::write(&path, &body).unwrap();
                planted.push((path, body));
            }
        }
        let encoded = png_bytes(&gradient(6, 6));
        let Saved::Refused { kept, .. } =
            file.save(|| Ok(Encoded::plain(encoded.clone()))).unwrap()
        else {
            panic!("expected Refused")
        };
        for (path, body) in &planted {
            assert_eq!(&std::fs::read_to_string(path).unwrap(), body, "{path:?}");
        }
        assert_eq!(std::fs::read(&kept).unwrap(), encoded);
        // The backup took the next free name: the stamped one plus `-<random>`.
        let backups: Vec<PathBuf> = walk(&crate::cache::backup_root(&file.cache_root))
            .into_iter()
            .filter(|p| !planted.iter().any(|(q, _)| q == p))
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    }

    /// The backup, the staged edit and a kept (refused) edit are the owner's only: 0600 files in
    /// 0700 directories, whatever the umask lets through. What Save copy publishes is not: it
    /// gets the mode any new file in that directory gets.
    #[test]
    fn local_copies_of_an_image_are_owner_only() {
        if crate::testutil::rerun_under_umask_022(
            "gui::image::tests::local_copies_of_an_image_are_owner_only",
        ) {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let original = png_bytes(&gradient(30, 20));
        let mut file = local_file(tmp.path(), "a.png", &original);
        std::fs::write(tmp.path().join("a.png"), b"someone else's bytes").unwrap();
        let Saved::Refused { kept, .. } = file
            .save(|| Ok(Encoded::plain(png_bytes(&gradient(6, 6)))))
            .unwrap()
        else {
            panic!("expected Refused")
        };
        let cache = tmp.path().join("cache");
        let backups = walk(&crate::cache::backup_root(&cache));
        assert_eq!(backups.len(), 1);
        for f in [&backups[0], &kept] {
            assert_eq!(mode(f), 0o600, "{f:?}");
        }
        for leaf in [backups[0].parent().unwrap(), kept.parent().unwrap()] {
            for dir in leaf.ancestors().take_while(|d| d.starts_with(&cache)) {
                assert_eq!(mode(dir), 0o700, "{dir:?}");
            }
        }
        let published = file.save_copy(&png_bytes(&gradient(6, 6))).unwrap();
        let control = tmp.path().join("control");
        std::fs::File::create(&control).unwrap();
        assert_eq!(mode(Path::new(&published)), mode(&control));
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

    /// Pasted text with no installed Hangul font is still drawn (egui's built-in font), and
    /// the status line says Hangul may not render; blank text is refused.
    #[test]
    fn pasted_text_without_a_hangul_font_says_hangul_may_not_render() {
        let mut doc = Doc::decode(&png_bytes(&gradient(80, 40))).unwrap();
        let before = doc.img.clone();
        let (line, area) = doc
            .paste_text(Some([0, 0, 80, 40]), "한글 ok", [0, 0, 0, 255], None)
            .unwrap();
        assert_eq!(
            line,
            "pasted text (5 chars) · no Hangul font found, so Hangul may not render"
        );
        assert_ne!(doc.img, before, "\"ok\" is drawn");
        assert_eq!(area[..2], [0, 0]);
        let err = doc
            .paste_text(Some([0, 0, 80, 40]), " \n\t", [0, 0, 0, 255], None)
            .unwrap_err();
        assert_eq!(err.to_string(), "the clipboard text is blank");
        assert!(
            doc.undo() && !doc.can_undo(),
            "one step: the blank paste made none"
        );
        assert_eq!(doc.img, before);
    }

    /// The window never asks for an empty rectangle: a drag that covers no whole pixel leaves
    /// none, and the model refuses one for every cut and paste.
    #[test]
    fn the_window_never_asks_for_an_empty_rectangle() {
        let bounds = Vec2::new(20.0, 10.0);
        let mut sel = Selection::default();
        for empty in [[3, 2, 0, 5], [3, 2, 5, 0]] {
            sel.set(empty);
            assert_eq!(sel.pixels(bounds), None, "{empty:?}");
            let mut doc = Doc::decode(&png_bytes(&gradient(20, 10))).unwrap();
            let picture = image::RgbaImage::new(2, 2);
            let black = [0, 0, 0, 255];
            assert!(doc.cut(Some(empty), black).is_err(), "{empty:?}");
            for fit in [clip::Fit::Stretch, clip::Fit::Fit, clip::Fit::OneToOne] {
                assert!(doc.paste_image(Some(empty), &picture, fit, black).is_err());
            }
            assert!(doc.paste_text(Some(empty), "x", black, None).is_err());
            assert!(!doc.can_undo(), "{empty:?}");
        }
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
