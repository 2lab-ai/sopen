//! The clipboard edits (spec §1 message 12): what Cut leaves in the rectangle, and how a pasted
//! picture or text is laid into it. Window-free, like [`super::Doc`]: each function changes
//! the pixels of its rectangle only and keeps the image's pixel type (16-bit stays 16-bit).

use crate::gui::fonts::Face;
use ab_glyph::{Font as _, FontRef, GlyphId, PxScale, ScaleFont as _};
use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageBuffer, Rgba, Rgba32FImage, RgbaImage};

/// Straight (not premultiplied) 8-bit RGBA: the toolbar's colours.
pub type Colour = [u8; 4];

/// How a pasted picture fills the rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Scaled to the rectangle's size, aspect not kept.
    #[default]
    Stretch,
    /// Scaled to fit inside it, aspect kept, centred; the rest gets the background colour.
    Fit,
    /// Unscaled at its top-left, cut off at its edges.
    OneToOne,
}

impl Fit {
    /// As the status line says it.
    pub fn name(self) -> &'static str {
        match self {
            Fit::Stretch => "stretch",
            Fit::Fit => "fit",
            Fit::OneToOne => "1:1",
        }
    }
}

fn unit(c: Colour) -> [f32; 4] {
    c.map(|v| f32::from(v) / 255.0)
}

/// Rows of the rectangle in 32-bit float at a time: a paste over the whole of a large photo
/// needs a band's worth of extra memory, not four times the photo.
const BAND: u32 = 256;

/// The part of `rect` inside an image of `size`: `(w, h)` from the same top-left corner.
fn inside((iw, ih): (u32, u32), [x, y, w, h]: [u32; 4]) -> (u32, u32) {
    (w.min(iw.saturating_sub(x)), h.min(ih.saturating_sub(y)))
}

/// Runs `edit` on the pixels of `rect` that are inside the image, a band of rows at a time —
/// straight RGBA in 0..=1; pixel `(rx, ry)` of a band is pixel `(rx, top + ry)` of the
/// rectangle — and writes them back in the image's own pixel type. Nothing outside `rect` is
/// touched, and a pixel `edit` leaves alone comes back bit for bit (8- and 16-bit values
/// survive the trip through 32-bit float).
fn edit_rect(img: &mut DynamicImage, rect: [u32; 4], mut edit: impl FnMut(&mut Rgba32FImage, u32)) {
    let [x, y, ..] = rect;
    let (w, h) = inside((img.width(), img.height()), rect);
    let mut top = 0;
    while top < h {
        let rows = BAND.min(h - top);
        let mut band = img.crop_imm(x, y + top, w, rows).into_rgba32f();
        edit(&mut band, top);
        let band = DynamicImage::ImageRgba32F(band);
        let (bx, by) = (i64::from(x), i64::from(y + top));
        macro_rules! put {
            ($($variant:ident => $to:ident),*) => {
                match &mut *img {
                    $(DynamicImage::$variant(buf) => imageops::replace(buf, &band.$to(), bx, by),)*
                    other => {
                        let mut rgba = other.to_rgba8();
                        imageops::replace(&mut rgba, &band.to_rgba8(), bx, by);
                        *other = DynamicImage::ImageRgba8(rgba);
                    }
                }
            };
        }
        put!(
            ImageLuma8 => to_luma8,
            ImageLumaA8 => to_luma_alpha8,
            ImageRgb8 => to_rgb8,
            ImageRgba8 => to_rgba8,
            ImageLuma16 => to_luma16,
            ImageLumaA16 => to_luma_alpha16,
            ImageRgb16 => to_rgb16,
            ImageRgba16 => to_rgba16,
            ImageRgb32F => to_rgb32f,
            ImageRgba32F => to_rgba32f
        );
        top += rows;
    }
}

/// `src` over `dst`, straight alpha: a transparent `src` leaves `dst` as it is, an opaque one
/// replaces it.
fn over(src: [f32; 4], dst: [f32; 4]) -> [f32; 4] {
    let sa = src[3];
    if sa <= 0.0 {
        return dst;
    }
    if sa >= 1.0 {
        return src;
    }
    let da = dst[3] * (1.0 - sa);
    let a = sa + da;
    let mix = |i: usize| (src[i] * sa + dst[i] * da) / a;
    [mix(0), mix(1), mix(2), a]
}

/// Fills `rect` with `colour` (alpha too, where the image has alpha).
pub fn fill(img: &mut DynamicImage, rect: [u32; 4], colour: Colour) {
    let c = unit(colour);
    edit_rect(img, rect, |band, _| band.pixels_mut().for_each(|p| p.0 = c));
}

/// A pasted picture at the size it is laid at.
enum Laid {
    /// Opaque (a screenshot, a photo): resampled as it is, 8 bits per channel.
    Opaque(RgbaImage),
    /// With transparency: resampled in premultiplied alpha (16 bits per channel), so
    /// transparent pixels do not darken the edges next to them.
    Premultiplied(ImageBuffer<Rgba<u16>, Vec<u16>>),
}

impl Laid {
    /// `picture` at `w`×`h` (bilinear), or as it is when it already has that size.
    fn new(picture: &RgbaImage, w: u32, h: u32) -> Self {
        let same = picture.dimensions() == (w, h);
        if picture.pixels().all(|p| p.0[3] == 255) {
            return Laid::Opaque(if same {
                picture.clone()
            } else {
                imageops::resize(picture, w, h, FilterType::Triangle)
            });
        }
        let pre = ImageBuffer::from_fn(picture.width(), picture.height(), |x, y| {
            let [r, g, b, a] = picture.get_pixel(x, y).0.map(u32::from);
            let times_alpha = |c: u32| ((c * 257 * a + 127) / 255) as u16;
            Rgba([
                times_alpha(r),
                times_alpha(g),
                times_alpha(b),
                (a * 257) as u16,
            ])
        });
        Laid::Premultiplied(if same {
            pre
        } else {
            imageops::resize(&pre, w, h, FilterType::Triangle)
        })
    }

    /// Straight RGBA in 0..=1.
    fn at(&self, x: u32, y: u32) -> [f32; 4] {
        match self {
            Laid::Opaque(p) => unit(p.get_pixel(x, y).0),
            Laid::Premultiplied(p) => {
                let [r, g, b, a] = p.get_pixel(x, y).0.map(|v| f32::from(v) / 65535.0);
                if a > 0.0 {
                    [r / a, g / a, b / a, a].map(|v| v.clamp(0.0, 1.0))
                } else {
                    [0.0; 4]
                }
            }
        }
    }
}

/// Lays `picture` into `rect` as `fit` says, over what is there (its alpha blends); with
/// [`Fit::Fit`] the part of `rect` it does not cover gets `background`. Returns where the
/// picture went, `[x, y, w, h]` in image pixels — no width and height, and nothing changed,
/// when `rect` or `picture` has none.
pub fn place(
    img: &mut DynamicImage,
    rect: [u32; 4],
    picture: &RgbaImage,
    fit: Fit,
    background: Colour,
) -> [u32; 4] {
    let [x, y, w, h] = rect;
    let (pw, ph) = picture.dimensions();
    if w == 0 || h == 0 || pw == 0 || ph == 0 {
        // Nowhere to lay it, or nothing to lay: nothing changes (the window asks for neither).
        return [x, y, 0, 0];
    }
    let (tw, th) = match fit {
        Fit::Stretch => (w, h),
        Fit::Fit => {
            let s = (f64::from(w) / f64::from(pw)).min(f64::from(h) / f64::from(ph));
            let side =
                |n: u32, room: u32| (f64::from(n) * s).round().clamp(1.0, f64::from(room)) as u32;
            (side(pw, w), side(ph, h))
        }
        Fit::OneToOne => (pw.min(w), ph.min(h)),
    };
    let (dx, dy) = ((w - tw) / 2, (h - th) / 2);
    let at = match fit {
        Fit::Fit => [x + dx, y + dy, tw, th],
        _ => [x, y, tw, th],
    };
    let laid = match fit {
        Fit::OneToOne => Laid::new(
            &imageops::crop_imm(picture, 0, 0, tw, th).to_image(),
            tw,
            th,
        ),
        _ => Laid::new(picture, tw, th),
    };
    match fit {
        Fit::Fit => {
            let bg = unit(background);
            edit_rect(img, rect, |band, top| {
                for (rx, ry, p) in band.enumerate_pixels_mut() {
                    let ry = top + ry;
                    p.0 = if (dx..dx + tw).contains(&rx) && (dy..dy + th).contains(&ry) {
                        over(laid.at(rx - dx, ry - dy), p.0)
                    } else {
                        bg
                    };
                }
            });
        }
        _ => edit_rect(img, at, |band, top| {
            for (rx, ry, p) in band.enumerate_pixels_mut() {
                p.0 = over(laid.at(rx, top + ry), p.0);
            }
        }),
    }
    at
}

#[cfg(test)]
thread_local! {
    /// Characters of pasted text read — by the layout, through [`composed`] — for the tests
    /// that bound its work.
    static READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts one character read by the layout (tests only).
fn read_one() {
    #[cfg(test)]
    READ.with(|r| r.set(r.get() + 1));
}

/// How big pasted text is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextSize {
    /// The largest size at which the text, wrapped to the rectangle's width, fits in it.
    Fill,
    /// This many pixels from the top of the tallest letter to the bottom of the lowest one.
    Px(f32),
}

/// What [`draw_text`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drawn {
    /// The text's block — its lines' extent — inside the rectangle.
    pub area: [u32; 4],
    /// Characters in the lines that show none of the faces has (drawn as the first face's
    /// missing-glyph box; spaces and invisible characters not counted).
    pub missing: usize,
}

/// The faces at one size: which one draws each character, how far it advances.
struct Shaper<'a> {
    fonts: Vec<FontRef<'a>>,
    scale: PxScale,
}

type Glyph = (usize, GlyphId);

/// How one character is drawn.
#[derive(Debug, Clone, Copy)]
enum Look {
    /// A glyph of the first face that has it.
    Glyph(Glyph),
    /// No face has it: the first face's missing-glyph box (`.notdef`). Counted as missing.
    Missing(Glyph),
    /// A space no face has: the missing glyph's room, nothing drawn.
    Blank(Glyph),
    /// A control or default-ignorable character (zero-width space, joiner, soft hyphen,
    /// variation selector, …): nothing drawn, no room taken.
    Invisible,
}

/// Unicode's Default_Ignorable_Code_Point (DerivedCoreProperties.txt): characters a renderer
/// shows nothing for when it cannot shape them.
fn ignorable(c: char) -> bool {
    matches!(
        u32::from(c),
        0xAD | 0x34F
            | 0x61C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// `text` composed (Unicode NFC) as it is read, without copying it: Korean copied from a macOS
/// file name comes decomposed into conjoining jamo (한 = ᄒ ᅡ ᆫ), which the faces draw as loose
/// letters, not the syllable — composed, it is the syllable.
pub fn composed(text: &str) -> impl Iterator<Item = char> + '_ {
    let read = text.chars().inspect(|_| read_one());
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc().normalize_iter(stream_safe(read))
}

/// Unicode's Stream-Safe Text Format (UAX #15 §13), so that the normalizer reads only as far
/// as the layout does: it holds every combining mark that follows a letter until the next
/// letter (to put them in canonical order), so a megabyte of marks would be read whole before
/// the first one came out. After 30 characters in a row that begin with a combining mark (no
/// real text has more), U+034F COMBINING GRAPHEME JOINER — a letter to the normalizer, drawn
/// as nothing — ends the run.
fn stream_safe(chars: impl Iterator<Item = char>) -> impl Iterator<Item = char> {
    use icu_normalizer::properties::{
        CanonicalCombiningClassMapBorrowed, CanonicalDecompositionBorrowed, Decomposed,
    };
    let (class, parts) = (
        CanonicalCombiningClassMapBorrowed::new(),
        CanonicalDecompositionBorrowed::new(),
    );
    let mut run = 0;
    chars.flat_map(move |c| {
        // The first character of its full canonical decomposition (a few levels at most).
        let mut first = c;
        for _ in 0..4 {
            match parts.decompose(first) {
                Decomposed::Default => break,
                Decomposed::Singleton(x) | Decomposed::Expansion(x, _) => first = x,
            }
        }
        let mut joiner = None;
        if class.get_u8(first) == 0 {
            run = 0;
        } else if run == 30 {
            (run, joiner) = (1, Some('\u{34F}'));
        } else {
            run += 1;
        }
        joiner.into_iter().chain(std::iter::once(c))
    })
}

/// [`composed`] `text` with each tab as four spaces.
fn expanded(text: &str) -> impl Iterator<Item = char> + '_ {
    composed(text).flat_map(|c| {
        let (c, n) = if c == '\t' { (' ', 4) } else { (c, 1) };
        std::iter::repeat_n(c, n)
    })
}

impl Shaper<'_> {
    fn look(&self, c: char) -> Look {
        if c.is_control() || ignorable(c) {
            return Look::Invisible;
        }
        let found = self.fonts.iter().enumerate().find_map(|(i, f)| {
            let id = f.glyph_id(c);
            (id.0 != 0).then_some((i, id))
        });
        match found {
            Some(g) => Look::Glyph(g),
            None if c.is_whitespace() => Look::Blank((0, GlyphId(0))),
            None => Look::Missing((0, GlyphId(0))),
        }
    }

    fn advance(&self, look: Look) -> f32 {
        match look {
            Look::Glyph((i, id)) | Look::Missing((i, id)) | Look::Blank((i, id)) => {
                self.fonts[i].as_scaled(self.scale).h_advance(id)
            }
            Look::Invisible => 0.0,
        }
    }

    fn kern(&self, a: Look, b: Look) -> f32 {
        match (a, b) {
            (Look::Glyph((i, a)), Look::Glyph((j, b))) if i == j => {
                self.fonts[i].as_scaled(self.scale).kern(a, b)
            }
            _ => 0.0,
        }
    }

    /// The least room a character takes when lines are filled — a quarter of the letter size —
    /// so that one that advances by nothing (a combining mark, an invisible character, a
    /// font's empty missing-glyph box) still fills a line: no line holds more than four per
    /// pixel of letter size across, however long the text.
    fn floor(&self) -> f32 {
        self.scale.y / 4.0
    }

    /// The width `line` is drawn at.
    fn width(&self, line: &[char]) -> f32 {
        let mut x = 0.0;
        let mut prev = None;
        for look in line.iter().map(|&c| self.look(c)) {
            if let Some(p) = prev {
                x += self.kern(p, look);
            }
            x += self.advance(look);
            prev = Some(look);
        }
        x
    }

    /// `text` broken into lines no wider than `room`: at newlines, after the last space that
    /// fits, and between letters inside a word longer than a line (Korean or Chinese without
    /// spaces, a long URL). Read one character at a time, each taking at least
    /// [`Self::floor`], and stopped after `most + 1` lines — enough to know a block taller than
    /// `most` lines does not fit — so a huge paste costs no more than the rectangle can show.
    fn wrap(&self, text: &str, room: f32, most: usize) -> Vec<Vec<char>> {
        let floor = self.floor();
        let step = |c: char| self.advance(self.look(c)).max(floor);
        let trimmed = |mut line: Vec<char>| {
            while line.last() == Some(&' ') {
                line.pop();
            }
            line
        };
        let mut lines: Vec<Vec<char>> = Vec::new();
        let mut line: Vec<char> = Vec::new();
        let mut used = 0.0;
        // Where the line may break: just after its last space.
        let mut after_space = None;
        for c in expanded(text) {
            if c == '\n' {
                lines.push(trimmed(std::mem::take(&mut line)));
                (used, after_space) = (0.0, None);
                if lines.len() > most {
                    return lines;
                }
                continue;
            }
            let wide = step(c);
            if used + wide > room && !line.is_empty() {
                if c == ' ' {
                    // The line ends at this space, which goes nowhere.
                    lines.push(trimmed(std::mem::take(&mut line)));
                    (used, after_space) = (0.0, None);
                    if lines.len() > most {
                        return lines;
                    }
                    continue;
                }
                // The word begun after the line's last space goes on to the next line.
                let carried = match after_space {
                    Some(at) if at < line.len() => line.split_off(at),
                    _ => Vec::new(),
                };
                lines.push(trimmed(std::mem::take(&mut line)));
                if lines.len() > most {
                    return lines;
                }
                used = carried.iter().map(|&c| step(c)).sum();
                (line, after_space) = (carried, None);
                if used + wide > room && !line.is_empty() {
                    // Too long for a line of its own: broken between letters.
                    lines.push(std::mem::take(&mut line));
                    used = 0.0;
                    if lines.len() > most {
                        return lines;
                    }
                }
            }
            line.push(c);
            used += wide;
            if c == ' ' {
                after_space = Some(line.len());
            }
        }
        lines.push(trimmed(line));
        lines
    }

    /// `(ascent, line height, gap between lines)` of the first face.
    fn metrics(&self) -> (f32, f32, f32) {
        let s = self.fonts[0].as_scaled(self.scale);
        (
            s.ascent(),
            s.ascent() - s.descent() + s.line_gap(),
            s.line_gap(),
        )
    }

    /// The lines of `text` wrapped to `w` — those that can show in `h`, and one more if there
    /// are — and the block's `(width, height)`.
    fn block(&self, text: &str, w: u32, h: u32) -> (Vec<Vec<char>>, f32, f32) {
        let (_, line_height, gap) = self.metrics();
        let most = ((h as f32 + gap) / line_height.max(1e-3)) as usize;
        let lines = self.wrap(text, w as f32, most);
        let width = lines.iter().map(|l| self.width(l)).fold(0.0, f32::max);
        let height = lines.len() as f32 * line_height - gap;
        (lines, width, height)
    }
}

/// Draws `text` in `colour` inside `rect`, wrapped to its width, left and top aligned, in the
/// first of `faces` that has each character (the first face sets the line spacing, and draws
/// its missing-glyph box for a character none has). Tabs are four spaces. Nothing outside
/// `rect` is touched; what does not fit — in the rectangle or the image — is cut off. An empty
/// rectangle draws nothing.
pub fn draw_text(
    img: &mut DynamicImage,
    rect: [u32; 4],
    text: &str,
    colour: Colour,
    faces: &[&Face],
    size: TextSize,
) -> Drawn {
    let [x, y, w, h] = rect;
    let empty = Drawn {
        area: [x, y, 0, 0],
        missing: 0,
    };
    let fonts: Vec<FontRef> = faces
        .iter()
        .filter_map(|f| FontRef::try_from_slice_and_index(f.bytes(), f.index()).ok())
        .collect();
    if w == 0 || h == 0 || fonts.is_empty() {
        return empty;
    }
    let at = |px: f32| Shaper {
        fonts: fonts.clone(),
        scale: PxScale::from(px),
    };
    let px = match size {
        TextSize::Px(px) => px.max(1.0),
        TextSize::Fill => {
            // The largest whole pixel size whose wrapped block fits; the smallest, cut off,
            // when none does.
            let fits = |px: u32| {
                let shaper = at(px as f32);
                let (_, width, height) = shaper.block(text, w, h);
                width <= w as f32 && height <= h as f32
            };
            let (mut lo, mut hi) = (1u32, h);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if fits(mid) {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            lo as f32
        }
    };
    let shaper = at(px);
    let (lines, width, height) = shaper.block(text, w, h);
    let (ascent, line_height, _) = shaper.metrics();
    // Coverage, 0..=255, of the rectangle's pixels inside the image (the text is laid out for
    // the whole rectangle, and cut off at the image's edge).
    let (vw, vh) = inside((img.width(), img.height()), rect);
    let mut mask = vec![0u8; vw as usize * vh as usize];
    let mut missing = 0;
    for (row, line) in lines.iter().enumerate() {
        let baseline = ascent + row as f32 * line_height;
        if baseline - ascent >= h as f32 {
            break;
        }
        let mut pen = 0.0;
        let mut prev = None;
        for &c in line {
            let look = shaper.look(c);
            if let Some(p) = prev {
                pen += shaper.kern(p, look);
            }
            let drawn = match look {
                Look::Glyph(g) => Some(g),
                Look::Missing(g) => {
                    missing += 1;
                    Some(g)
                }
                Look::Blank(_) | Look::Invisible => None,
            };
            if let Some((i, id)) = drawn {
                let glyph =
                    id.with_scale_and_position(shaper.scale, ab_glyph::point(pen, baseline));
                if let Some(outline) = shaper.fonts[i].outline_glyph(glyph) {
                    let b = outline.px_bounds();
                    outline.draw(|gx, gy, coverage| {
                        let (px, py) = (
                            b.min.x as i64 + i64::from(gx),
                            b.min.y as i64 + i64::from(gy),
                        );
                        if (0..i64::from(vw)).contains(&px) && (0..i64::from(vh)).contains(&py) {
                            let m = &mut mask[py as usize * vw as usize + px as usize];
                            *m = (*m).max((coverage.clamp(0.0, 1.0) * 255.0).round() as u8);
                        }
                    });
                }
            }
            pen += shaper.advance(look);
            prev = Some(look);
        }
    }
    let c = unit(colour);
    edit_rect(img, rect, |band, top| {
        for (rx, ry, p) in band.enumerate_pixels_mut() {
            let m = mask[(top + ry) as usize * vw as usize + rx as usize];
            if m > 0 {
                p.0 = over([c[0], c[1], c[2], c[3] * f32::from(m) / 255.0], p.0);
            }
        }
    });
    let side = |v: f32, room: u32| (v.ceil().max(0.0) as u32).min(room);
    Drawn {
        area: [x, y, side(width, w), side(height, h)],
        missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::fonts;
    use image::{GenericImageView, ImageBuffer, Rgb, Rgba};

    const WHITE: Colour = [255, 255, 255, 255];
    const BLACK: Colour = [0, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];

    /// Every pixel differs from its neighbours, in all 16 bits.
    fn rgb16(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb16(ImageBuffer::from_fn(w, h, |x, y| {
            let mix = |v: u32, k: u16| (v as u16).wrapping_mul(k).wrapping_add(7);
            Rgb([mix(x, 2213), mix(y, 3001), mix(x * y, 17)])
        }))
    }

    fn coordinates(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([x as u8, y as u8, 99])
        }))
    }

    /// Blue on the left half, red on the right.
    fn halves(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, _| Rgba(if x < w / 2 { BLUE } else { RED }))
    }

    fn untouched_outside(img: &DynamicImage, before: &DynamicImage, [rx, ry, rw, rh]: [u32; 4]) {
        assert_eq!(img.dimensions(), before.dimensions());
        assert_eq!(img.color(), before.color(), "pixel type kept");
        let (a, b) = (img.to_rgba16(), before.to_rgba16());
        for (x, y, p) in a.enumerate_pixels() {
            if !((rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y)) {
                assert_eq!(
                    p,
                    b.get_pixel(x, y),
                    "({x},{y}) outside {rx},{ry} {rw}×{rh}"
                );
            }
        }
    }

    #[test]
    fn fill_replaces_the_rectangle_only_in_the_image_s_own_pixel_type() {
        let before = rgb16(20, 12);
        let mut img = before.clone();
        fill(&mut img, [2, 3, 4, 5], WHITE);
        untouched_outside(&img, &before, [2, 3, 4, 5]);
        let DynamicImage::ImageRgb16(buf) = &img else {
            panic!("variant changed: {:?}", img.color())
        };
        assert_eq!(buf.get_pixel(2, 3).0, [65535; 3]);
        assert_eq!(buf.get_pixel(5, 7).0, [65535; 3]);
    }

    #[test]
    fn a_transparent_fill_clears_alpha() {
        let before = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([9, 8, 7, 255])));
        let mut img = before.clone();
        fill(&mut img, [1, 1, 3, 3], [255, 255, 255, 0]);
        untouched_outside(&img, &before, [1, 1, 3, 3]);
        assert_eq!(img.get_pixel(1, 1).0[3], 0);
        assert_eq!(img.get_pixel(3, 3).0[3], 0);
    }

    #[test]
    fn stretch_scales_the_picture_to_the_rectangle() {
        let before = coordinates(40, 30);
        let mut img = before.clone();
        let at = place(&mut img, [4, 5, 16, 8], &halves(4, 2), Fit::Stretch, WHITE);
        assert_eq!(at, [4, 5, 16, 8]);
        untouched_outside(&img, &before, at);
        assert_eq!(img.get_pixel(4, 5).0, BLUE);
        assert_eq!(img.get_pixel(9, 12).0, BLUE);
        assert_eq!(img.get_pixel(14, 5).0, RED);
        assert_eq!(img.get_pixel(19, 12).0, RED);
    }

    /// A picture the rectangle's size goes in pixel for pixel (copy, then paste over the same
    /// rectangle, changes nothing).
    #[test]
    fn a_picture_of_the_rectangle_s_size_goes_in_exactly() {
        let src = coordinates(40, 30);
        let picture = src.crop_imm(3, 4, 7, 5).to_rgba8();
        for fit in [Fit::Stretch, Fit::Fit, Fit::OneToOne] {
            let mut img = coordinates(40, 30);
            img.as_mut_rgb8().unwrap().put_pixel(20, 20, Rgb([1, 2, 3]));
            let at = place(&mut img, [20, 20, 7, 5], &picture, fit, WHITE);
            assert_eq!(at, [20, 20, 7, 5], "{fit:?}");
            assert_eq!(
                img.crop_imm(20, 20, 7, 5).to_rgba8(),
                picture,
                "{fit:?}: pixel for pixel"
            );
        }
    }

    #[test]
    fn fit_keeps_the_aspect_centres_and_fills_the_rest_with_the_background() {
        let before = coordinates(40, 30);
        // 4×2 into 8×8: 8×4 at rows 2..6.
        let mut img = before.clone();
        let at = place(&mut img, [10, 10, 8, 8], &halves(4, 2), Fit::Fit, WHITE);
        assert_eq!(at, [10, 12, 8, 4]);
        untouched_outside(&img, &before, [10, 10, 8, 8]);
        for (x, y, want) in [
            (10, 10, WHITE),
            (17, 11, WHITE),
            (10, 12, BLUE),
            (17, 15, RED),
            (10, 16, WHITE),
            (17, 17, WHITE),
        ] {
            assert_eq!(img.get_pixel(x, y).0, want, "({x},{y})");
        }
        // 2×4 into 8×4: 2×4 at columns 3..5 — centred across.
        let mut img = before.clone();
        let tall = RgbaImage::from_pixel(2, 4, Rgba(BLUE));
        let at = place(&mut img, [0, 0, 8, 4], &tall, Fit::Fit, BLACK);
        assert_eq!(at, [3, 0, 2, 4]);
        assert_eq!(img.get_pixel(2, 0).0, BLACK);
        assert_eq!(img.get_pixel(3, 0).0, BLUE);
        assert_eq!(img.get_pixel(4, 3).0, BLUE);
        assert_eq!(img.get_pixel(5, 3).0, BLACK);
    }

    #[test]
    fn one_to_one_places_unscaled_at_the_top_left_and_clips() {
        let before = coordinates(40, 30);
        // Bigger than the rectangle: cut off at its right and bottom edges.
        let mut img = before.clone();
        let at = place(&mut img, [5, 6, 4, 3], &halves(6, 6), Fit::OneToOne, WHITE);
        assert_eq!(at, [5, 6, 4, 3]);
        untouched_outside(&img, &before, at);
        assert_eq!(img.get_pixel(7, 8).0, BLUE);
        assert_eq!(img.get_pixel(8, 8).0, RED);
        // Smaller: the rest of the rectangle keeps its pixels.
        let mut img = before.clone();
        let at = place(
            &mut img,
            [5, 6, 10, 10],
            &halves(2, 2),
            Fit::OneToOne,
            WHITE,
        );
        assert_eq!(at, [5, 6, 2, 2]);
        untouched_outside(&img, &before, at);
        assert_eq!(img.get_pixel(5, 7).0, BLUE);
        assert_eq!(img.get_pixel(6, 7).0, RED);
    }

    /// A picture's alpha blends over the pixels there: transparent leaves them exactly, half
    /// transparent mixes; an image with alpha under a transparent background keeps it.
    #[test]
    fn a_picture_with_alpha_is_laid_over_what_is_there() {
        let before = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 1, Rgb([0, 0, 200])));
        let picture = RgbaImage::from_vec(
            4,
            1,
            [
                [255, 0, 0, 0],
                [255, 0, 0, 128],
                [255, 0, 0, 255],
                [9, 9, 9, 0],
            ]
            .concat(),
        )
        .unwrap();
        let mut img = before.clone();
        place(&mut img, [0, 0, 4, 1], &picture, Fit::Stretch, WHITE);
        assert_eq!(
            img.get_pixel(0, 0).0,
            [0, 0, 200, 255],
            "transparent: as before"
        );
        let half = img.get_pixel(1, 0).0;
        assert!(
            half[0].abs_diff(128) <= 1 && half[2].abs_diff(100) <= 1,
            "half over: {half:?}"
        );
        assert_eq!(img.get_pixel(2, 0).0, [255, 0, 0, 255], "opaque: replaced");
        assert_eq!(img.color(), image::ColorType::Rgb8);

        let clear = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 1, Rgba([0, 0, 0, 0])));
        let mut img = clear.clone();
        place(
            &mut img,
            [0, 0, 4, 1],
            &picture,
            Fit::Stretch,
            [255, 255, 255, 0],
        );
        assert_eq!(
            img.get_pixel(1, 0).0,
            [255, 0, 0, 128],
            "its own alpha kept"
        );
        assert_eq!(img.get_pixel(0, 0).0[3], 0);
    }

    /// The rectangle's pixels go through 32-bit float and back: those nothing covers come
    /// back bit for bit, also in a 16-bit image.
    #[test]
    fn sixteen_bit_pixels_under_nothing_come_back_exactly() {
        let before = rgb16(30, 20);
        let mut img = before.clone();
        let clear = RgbaImage::from_pixel(5, 5, Rgba([255, 0, 0, 0]));
        place(&mut img, [3, 2, 20, 15], &clear, Fit::Stretch, WHITE);
        assert_eq!(img, before);
        let mut gray = DynamicImage::ImageLuma8(before.to_luma8());
        let gray_before = gray.clone();
        place(&mut gray, [3, 2, 20, 15], &clear, Fit::Stretch, WHITE);
        assert_eq!(gray, gray_before);
    }

    /// A rectangle taller than one band of rows is edited band by band: a picture laid across
    /// the boundary, a fill and text all land where they would in one piece.
    #[test]
    fn rectangles_taller_than_a_band_are_edited_whole() {
        let before = rgb16(12, 3 * BAND);
        let tall = [2, 5, 8, 2 * BAND + 20];
        let mut img = before.clone();
        fill(&mut img, tall, WHITE);
        untouched_outside(&img, &before, tall);
        let DynamicImage::ImageRgb16(buf) = &img else {
            panic!("variant changed")
        };
        assert!((5..5 + 2 * BAND + 20).all(|y| buf.get_pixel(2, y).0 == [65535; 3]));

        // 8×10, blue on top and red below, fitted into 8×520 at rows 255..265 of the rectangle:
        // across the first band's last row.
        let picture = RgbaImage::from_fn(8, 10, |_, y| Rgba(if y < 5 { BLUE } else { RED }));
        let mut img = coordinates(8, 600);
        let at = place(&mut img, [0, 0, 8, 520], &picture, Fit::Fit, BLACK);
        assert_eq!(at, [0, 255, 8, 10]);
        for (y, want) in [
            (254, BLACK),
            (255, BLUE),
            (256, BLUE),
            (259, BLUE),
            (260, RED),
            (264, RED),
            (265, BLACK),
        ] {
            assert_eq!(img.get_pixel(3, y).0, want, "row {y}");
        }

        // Text in lines down a 600 px tall rectangle has ink in the second and third bands.
        let before = white(60, 600);
        let mut img = before.clone();
        let words = "a b c d e f g h i j k l m n o p q r s t u v w x y z";
        draw_text(
            &mut img,
            [0, 0, 60, 600],
            words,
            BLACK,
            &[fonts::fallback()],
            TextSize::Px(48.0),
        );
        let ink = changed(&img, &before);
        assert!(
            ink.iter().any(|p| p.1 > BAND && p.1 < 2 * BAND),
            "second band"
        );
        assert!(ink.iter().any(|p| p.1 > 2 * BAND), "third band");
        assert!(ink.iter().all(|p| p.0 < 60), "inside");
        // Each band shows its own lines, not the first band's again.
        let band = |from: u32| -> Vec<(u32, u32)> {
            ink.iter()
                .filter(|p| (from..from + BAND).contains(&p.1))
                .map(|p| (p.0, p.1 - from))
                .collect()
        };
        assert_ne!(band(0), band(BAND));
    }

    /// Panics, naming the first pixel that differs, unless `a` and `b` are the same image.
    #[track_caller]
    fn assert_same(a: &DynamicImage, b: &DynamicImage, what: &str) {
        assert_eq!(a.dimensions(), b.dimensions(), "{what}: size");
        assert_eq!(a.color(), b.color(), "{what}: pixel type");
        let (pa, pb) = (a.to_rgba16(), b.to_rgba16());
        if let Some((x, y, p)) = pa
            .enumerate_pixels()
            .find(|(x, y, p)| *p != pb.get_pixel(*x, *y))
        {
            panic!(
                "{what}: ({x},{y}) is {:?}, wants {:?}",
                p.0,
                pb.get_pixel(x, y).0
            );
        }
    }

    /// Pixels of `img` that differ from `before`, as `(x, y)`.
    fn changed(img: &DynamicImage, before: &DynamicImage) -> Vec<(u32, u32)> {
        img.pixels()
            .filter(|(x, y, p)| *p != before.get_pixel(*x, *y))
            .map(|(x, y, _)| (x, y))
            .collect()
    }

    fn bounds(points: &[(u32, u32)]) -> [u32; 4] {
        let x0 = points.iter().map(|p| p.0).min().unwrap();
        let y0 = points.iter().map(|p| p.1).min().unwrap();
        let x1 = points.iter().map(|p| p.0).max().unwrap();
        let y1 = points.iter().map(|p| p.1).max().unwrap();
        [x0, y0, x1 - x0 + 1, y1 - y0 + 1]
    }

    fn white(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, Rgb([255, 255, 255])))
    }

    /// Text is drawn in its colour inside the rectangle and only there, sized to fill it.
    #[test]
    fn text_is_drawn_inside_the_rectangle_only_and_fills_it() {
        let before = white(200, 100);
        let mut img = before.clone();
        let rect = [20, 10, 120, 60];
        let drawn = draw_text(
            &mut img,
            rect,
            "sopen",
            BLACK,
            &[fonts::fallback()],
            TextSize::Fill,
        );
        assert_eq!(drawn.missing, 0);
        let ink = changed(&img, &before);
        assert!(ink.len() > 200, "{} pixels drawn", ink.len());
        let [x, y, w, h] = bounds(&ink);
        assert!(
            x >= 20 && y >= 10 && x + w <= 140 && y + h <= 70,
            "{:?}",
            [x, y, w, h]
        );
        // One word, sized up until it meets the rectangle's width or height.
        assert!(w >= 90 || h >= 40, "fills the rectangle: {w}×{h}");
        assert!(
            img.pixels().any(|(_, _, p)| p.0 == [0, 0, 0, 255]),
            "full coverage is the text colour"
        );
        let [ax, ay, aw, ah] = drawn.area;
        assert!(
            ax == 20 && ay == 10 && aw <= 120 && ah <= 60,
            "{:?}",
            drawn.area
        );
    }

    /// Wrapped to the rectangle's width: a narrow rectangle gets the words on several lines.
    #[test]
    fn text_wraps_to_the_rectangle_width() {
        let before = white(400, 400);
        let words = "one two three four five six";
        let mut wide = before.clone();
        draw_text(
            &mut wide,
            [0, 0, 400, 40],
            words,
            BLACK,
            &[fonts::fallback()],
            TextSize::Fill,
        );
        let mut narrow = before.clone();
        let drawn = draw_text(
            &mut narrow,
            [0, 0, 80, 400],
            words,
            BLACK,
            &[fonts::fallback()],
            TextSize::Px(16.0),
        );
        let ink = changed(&narrow, &before);
        let [_, _, w, _] = bounds(&ink);
        assert!(w <= 80, "{w}");
        assert!(lines_of_ink(&ink) >= 3, "{} lines", lines_of_ink(&ink));
        assert!(drawn.area[3] > 2 * 16, "{:?}", drawn.area);
        assert_eq!(
            lines_of_ink(&changed(&wide, &before)),
            1,
            "one line when it fits"
        );
    }

    /// Runs of pixel rows with ink in them, top to bottom: the lines of text.
    fn lines_of_ink(ink: &[(u32, u32)]) -> usize {
        let mut rows: Vec<u32> = ink.iter().map(|p| p.1).collect();
        rows.sort_unstable();
        rows.dedup();
        1 + rows.windows(2).filter(|r| r[1] > r[0] + 1).count()
    }

    /// Korean is drawn with an installed Hangul font when there is one; with only the
    /// built-in font its letters are counted as missing (the rest is still drawn).
    #[test]
    fn hangul_is_drawn_with_an_installed_font_and_counted_missing_without_one() {
        let before = white(200, 80);
        let mut img = before.clone();
        let drawn = draw_text(
            &mut img,
            [0, 0, 200, 80],
            "한글 ok",
            BLACK,
            &[fonts::fallback()],
            TextSize::Fill,
        );
        assert_eq!(drawn.missing, 2, "한 and 글");
        assert!(!changed(&img, &before).is_empty(), "\"ok\" is still drawn");

        let Some(hangul) = fonts::hangul() else {
            eprintln!("skipping the Hangul half: no installed font with Hangul");
            return;
        };
        let mut img = before.clone();
        let drawn = draw_text(
            &mut img,
            [0, 0, 200, 80],
            "한글",
            BLACK,
            &[hangul, fonts::fallback()],
            TextSize::Fill,
        );
        assert_eq!(drawn.missing, 0);
        let ink = changed(&img, &before);
        assert!(ink.len() > 300, "{} pixels of Hangul", ink.len());
        // Real glyphs, not one box for every letter: 한 and 글 differ, and neither is what the
        // font without Hangul draws for it.
        let one = |c: &str, faces: &[&Face]| {
            let mut img = white(60, 60);
            draw_text(
                &mut img,
                [0, 0, 60, 60],
                c,
                BLACK,
                faces,
                TextSize::Px(40.0),
            );
            img
        };
        let (han, geul) = (one("한", &[hangul]), one("글", &[hangul]));
        assert_ne!(han, geul);
        assert_ne!(han, one("한", &[fonts::fallback()]));
    }

    /// Composing keeps what is already composed, composes what is not, and ends a run of more
    /// than 30 combining marks with a grapheme joiner (stream-safe) — never sooner.
    #[test]
    fn composing_is_nfc_with_runs_of_marks_cut_at_thirty() {
        let nfc = |t: &str| composed(t).collect::<String>();
        assert_eq!(nfc("한글 e\u{0301} ok"), "한글 é ok");
        assert_eq!(nfc("\u{1112}\u{1161}\u{11AB}"), "한");
        let marks = |n: usize| "\u{0316}".repeat(n);
        assert_eq!(nfc(&format!("a{}", marks(30))), format!("a{}", marks(30)));
        assert_eq!(
            nfc(&format!("a{}", marks(61))),
            format!("a{}\u{34F}{}\u{34F}{}", marks(30), marks(30), marks(1))
        );
    }

    /// Korean copied from a macOS file name comes decomposed (NFD: 한 = ㅎ ㅏ ㄴ as conjoining
    /// jamo); it is drawn as the syllables it spells, like the same text composed (NFC).
    #[test]
    fn decomposed_korean_is_drawn_as_its_syllables() {
        let Some(hangul) = fonts::hangul() else {
            eprintln!("skipping: no installed font with Hangul");
            return;
        };
        let nfd = "\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}";
        let draw = |text: &str| {
            let mut img = white(160, 80);
            let drawn = draw_text(
                &mut img,
                [10, 10, 140, 60],
                text,
                BLACK,
                &[hangul, fonts::fallback()],
                TextSize::Fill,
            );
            (img, drawn)
        };
        let (composed, c) = draw("한글");
        let (decomposed, d) = draw(nfd);
        assert_eq!(c, d);
        assert_same(&decomposed, &composed, "NFD 한글 against NFC 한글");
    }

    #[test]
    fn text_is_drawn_in_its_colour_over_alpha() {
        let before = DynamicImage::ImageRgba8(RgbaImage::from_pixel(120, 60, Rgba([0, 0, 0, 0])));
        let mut img = before.clone();
        draw_text(
            &mut img,
            [0, 0, 120, 60],
            "Hi",
            [200, 10, 20, 255],
            &[fonts::fallback()],
            TextSize::Fill,
        );
        assert!(img.pixels().any(|(_, _, p)| p.0 == [200, 10, 20, 255]));
        assert_eq!(img.color(), image::ColorType::Rgba8);
    }

    /// Characters the layout reads to draw `text` filling a 120×60 rectangle.
    fn read_by_layout(text: &str) -> usize {
        READ.with(|r| r.set(0));
        let mut img = white(120, 60);
        let rect = [0, 0, 120, 60];
        draw_text(
            &mut img,
            rect,
            text,
            BLACK,
            &[fonts::fallback()],
            TextSize::Fill,
        );
        READ.with(|r| r.get())
    }

    /// A paste far longer than the rectangle can show costs only what it shows: the layout
    /// reads as much of 2 MB as of 1 MB, a few thousand characters — for words, one endless
    /// word, a character no font has, a zero-width mark and an invisible character alike.
    #[test]
    fn layout_reads_only_what_the_rectangle_can_show() {
        for (what, piece) in [
            ("words", "lorem ipsum dolor sit amet "),
            ("one endless word", "a"),
            ("a character no font has", "\u{10FFFD}"),
            ("a zero-width mark", "\u{0301}"),
            ("a letter whose decomposition is marks", "\u{0F73}"),
            (
                "a letter, then marks",
                &format!("e{}", "\u{0301}".repeat(4096)),
            ),
            ("an invisible character", "\u{200B}"),
        ] {
            let mb = |n: usize| piece.repeat((n << 20) / piece.len());
            let one = read_by_layout(&mb(1));
            let two = read_by_layout(&mb(2));
            assert_eq!(one, two, "{what}: reads more of 2 MB than of 1 MB");
            assert!(one < 100_000, "{what}: read {one} characters");
        }
    }

    /// A character no font has takes room (the first face's missing-glyph box), so it wraps
    /// like a letter and is counted.
    #[test]
    fn a_character_no_font_has_takes_room_and_is_counted() {
        let before = white(200, 200);
        let mut img = before.clone();
        let drawn = draw_text(
            &mut img,
            [0, 0, 200, 200],
            &"\u{10FFFD}".repeat(40),
            BLACK,
            &[fonts::fallback()],
            TextSize::Px(20.0),
        );
        assert!(
            drawn.area[3] > 20,
            "wrapped onto several lines: {:?}",
            drawn.area
        );
        assert!(
            drawn.missing > 0 && drawn.missing <= 40,
            "{}",
            drawn.missing
        );
    }

    /// A rectangle that runs past the image's right or bottom edge: what lands is what a larger
    /// canvas shows, cut to the image — text is laid out for the whole rectangle and each row of
    /// it lands on its own row.
    #[test]
    fn edits_past_the_image_edge_match_a_larger_canvas_cut_down() {
        let words = "one two three four five six seven";
        for rect in [[80, 10, 100, 40], [10, 30, 60, 80], [90, 40, 80, 60]] {
            let (mut small, mut big) = (white(120, 60), white(240, 160));
            for img in [&mut small, &mut big] {
                draw_text(
                    img,
                    rect,
                    words,
                    BLACK,
                    &[fonts::fallback()],
                    TextSize::Fill,
                );
            }
            assert_same(
                &small,
                &big.crop_imm(0, 0, 120, 60),
                &format!("text in {rect:?}"),
            );
            for fit in [Fit::Stretch, Fit::Fit, Fit::OneToOne] {
                let (mut small, mut big) = (coordinates(120, 60), coordinates(240, 160));
                let picture = halves(70, 50);
                let a = place(&mut small, rect, &picture, fit, WHITE);
                let b = place(&mut big, rect, &picture, fit, WHITE);
                assert_eq!(a, b, "{fit:?} in {rect:?}");
                let what = format!("{fit:?} in {rect:?}");
                assert_same(&small, &big.crop_imm(0, 0, 120, 60), &what);
            }
            let (mut small, mut big) = (coordinates(120, 60), coordinates(240, 160));
            fill(&mut small, rect, BLACK);
            fill(&mut big, rect, BLACK);
            assert_same(
                &small,
                &big.crop_imm(0, 0, 120, 60),
                &format!("fill {rect:?}"),
            );
        }
    }

    /// A rectangle with no width or no height, or a picture with none, changes nothing (the
    /// window never asks for one: see `Doc`'s tests).
    #[test]
    fn an_empty_rectangle_or_picture_changes_nothing() {
        let before = coordinates(20, 10);
        let fits = [Fit::Fit, Fit::Stretch, Fit::OneToOne];
        for rect in [[3, 2, 0, 5], [3, 2, 5, 0]] {
            for fit in fits {
                let mut img = before.clone();
                let at = place(&mut img, rect, &halves(4, 2), fit, BLACK);
                assert_eq!(at, [3, 2, 0, 0], "{fit:?} into {rect:?}");
                assert_same(&img, &before, &format!("{fit:?} into {rect:?}"));
            }
            let mut img = before.clone();
            fill(&mut img, rect, BLACK);
            assert_same(&img, &before, &format!("fill {rect:?}"));
            let drawn = draw_text(
                &mut img,
                rect,
                "x",
                BLACK,
                &[fonts::fallback()],
                TextSize::Fill,
            );
            assert_eq!(
                drawn,
                Drawn {
                    area: [3, 2, 0, 0],
                    missing: 0
                },
                "text {rect:?}"
            );
            assert_same(&img, &before, &format!("text {rect:?}"));
        }
        for picture in [RgbaImage::new(0, 3), RgbaImage::new(3, 0)] {
            for fit in fits {
                let mut img = before.clone();
                let at = place(&mut img, [1, 1, 6, 6], &picture, fit, BLACK);
                assert_eq!(at, [1, 1, 0, 0], "{fit:?}: {:?}", picture.dimensions());
                let what = format!("{fit:?}: {:?}", picture.dimensions());
                assert_same(&img, &before, &what);
            }
        }
    }

    /// Resampling a picture with transparency blends in premultiplied alpha: between opaque
    /// red and fully transparent, a pixel is red with partial alpha — never darkened towards
    /// the transparent pixels' black, as blending straight RGBA would. Laid over opaque white,
    /// it then mixes by that alpha.
    #[test]
    fn a_resized_translucent_picture_keeps_its_colour_at_the_edge() {
        let picture = RgbaImage::from_vec(2, 1, vec![255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
        let mut clear = DynamicImage::ImageRgba8(RgbaImage::new(8, 1));
        place(&mut clear, [0, 0, 8, 1], &picture, Fit::Stretch, WHITE);
        let edge = (0..8)
            .map(|x| clear.get_pixel(x, 0).0)
            .find(|p| p[3] > 10 && p[3] < 245)
            .expect("a partly transparent pixel between the two");
        assert!(
            edge[0] >= 254 && edge[1] <= 1 && edge[2] <= 1,
            "red, not darkened: {edge:?}"
        );
        let mut white_img = white(8, 1);
        place(&mut white_img, [0, 0, 8, 1], &picture, Fit::Stretch, WHITE);
        for x in 0..8 {
            let a = f64::from(clear.get_pixel(x, 0).0[3]) / 255.0;
            let want = (255.0 * (1.0 - a)).round();
            let got = f64::from(white_img.get_pixel(x, 0).0[1]);
            assert!(
                (got - want).abs() <= 1.0,
                "x={x}: green {got}, alpha {a} wants {want}"
            );
        }
    }

    /// Half-transparent paint over a 16-bit image mixes at 16-bit precision.
    #[test]
    fn half_transparent_paint_mixes_into_sixteen_bits() {
        let dst = [1000u16, 30000, 65535];
        let mut img = DynamicImage::ImageRgb16(ImageBuffer::from_pixel(3, 1, Rgb(dst)));
        let paint = RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 128]));
        place(&mut img, [1, 0, 1, 1], &paint, Fit::Stretch, WHITE);
        let DynamicImage::ImageRgb16(buf) = &img else {
            panic!("variant changed")
        };
        let a = 128.0 / 255.0;
        let want = [65535.0, 0.0, 0.0].map(|s: f64| s * a);
        for (i, (got, d)) in buf.get_pixel(1, 0).0.iter().zip(dst).enumerate() {
            let want = want[i] + f64::from(d) * (1.0 - a);
            assert!(
                (f64::from(*got) - want).abs() <= 2.0,
                "channel {i}: {got} vs {want}"
            );
        }
        assert_eq!(buf.get_pixel(0, 0).0, dst, "beside it: untouched");
    }

    #[test]
    fn fit_names() {
        assert_eq!(Fit::default(), Fit::Stretch);
        assert_eq!(
            [Fit::Stretch, Fit::Fit, Fit::OneToOne].map(Fit::name),
            ["stretch", "fit", "1:1"]
        );
    }
}
