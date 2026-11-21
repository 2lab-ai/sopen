//! What the image window learns of a file before it reads it whole (06 §7.3): the header, found
//! by walking — a JPEG's marker segments up to its SOF, read by sopen's own walk; any other
//! format's through its decoder, on a reader that fetches 64 KiB blocks and stops at 1 MiB —
//! the memory a decode will need, the Exif thumbnail, and a picture of the bytes that have
//! arrived so far (a JPEG's prefix, a PNG's rows). Everything here treats the file as
//! untrusted: every offset and length it names is checked against what was read.

use anyhow::{anyhow, Context as _, Result};
use image::metadata::Orientation;
use image::{DynamicImage, ImageBuffer, ImageDecoder, ImageFormat, ImageReader, RgbaImage};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::rc::Rc;

/// What a header read fetches at a time: the stream's block.
pub const BLOCK: u64 = crate::stream::BLOCK;
/// The most a header read fetches (06 §7.3).
pub const HEADER_CAP: u64 = 1 << 20;
/// Up to this much memory the window goes on without asking (06 §7.3).
pub const BUDGET: u64 = 512 << 20;
/// The most an Exif thumbnail may be held as (RGBA, upright): 1 MiB, 512×512 or the like. One
/// is 160×120 or so — the whole APP1 it comes in is at most 64 KiB — and its own header is as
/// untrusted as the file's.
pub const THUMB_RGBA: u64 = 1 << 20;

/// Reads `buf.len()` bytes at an offset (fewer at the end of the file).
pub type ReadAt<'a> = dyn FnMut(u64, &mut [u8]) -> io::Result<usize> + 'a;

/// A file read a block at a time, each block once, up to a cap: what a header read is given.
/// `Read + Seek`, so a decoder reads its header through it wherever the format put it.
pub struct Head<'a> {
    read: Box<ReadAt<'a>>,
    size: u64,
    cap: u64,
    blocks: BTreeMap<u64, Vec<u8>>,
    pos: u64,
    flags: Rc<Flags>,
}

/// Why a read through a [`Head`] stopped, when it was not the file's content: kept apart from
/// the reader, which a decoder holds while these are looked at.
#[derive(Default)]
struct Flags {
    /// A read wanted a block past the cap.
    over: Cell<bool>,
    /// The read under it failed: why (the stream's own error — not the file's fault).
    broken: RefCell<Option<String>>,
}

/// Why a header read stopped, when it was not the file's content.
const OVER: &str = "the header is not in the first 1 MiB read";

impl<'a> Head<'a> {
    pub fn new(
        size: u64,
        cap: u64,
        read: impl FnMut(u64, &mut [u8]) -> io::Result<usize> + 'a,
    ) -> Self {
        Self {
            read: Box::new(read),
            size,
            cap,
            blocks: BTreeMap::new(),
            pos: 0,
            flags: Rc::default(),
        }
    }

    /// Bytes already in memory, read without a cap.
    pub fn of(bytes: &'a [u8]) -> Self {
        Self::new(bytes.len() as u64, u64::MAX, move |off, buf| {
            let at = (off as usize).min(bytes.len());
            let n = buf.len().min(bytes.len() - at);
            buf[..n].copy_from_slice(&bytes[at..at + n]);
            Ok(n)
        })
    }

    /// The bytes fetched so far.
    pub fn fetched(&self) -> u64 {
        self.blocks.values().map(|b| b.len() as u64).sum()
    }

    fn block(&mut self, index: u64) -> io::Result<&[u8]> {
        if !self.blocks.contains_key(&index) {
            if (self.blocks.len() as u64 + 1) * BLOCK > self.cap {
                self.flags.over.set(true);
                return Err(io::Error::other(OVER));
            }
            let off = index * BLOCK;
            let mut buf = vec![0u8; BLOCK.min(self.size.saturating_sub(off)) as usize];
            let n = match (self.read)(off, &mut buf) {
                Ok(n) => n,
                Err(e) => {
                    *self.flags.broken.borrow_mut() = Some(e.to_string());
                    return Err(e);
                }
            };
            buf.truncate(n);
            self.blocks.insert(index, buf);
        }
        Ok(&self.blocks[&index])
    }

    /// Up to `buf.len()` bytes at `off`: fewer where the file ends.
    fn read_some(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut done = 0;
        while done < buf.len() {
            let at = off + done as u64;
            if at >= self.size {
                break;
            }
            let block = self.block(at / BLOCK)?;
            let from = (at % BLOCK) as usize;
            if from >= block.len() {
                break;
            }
            let n = (buf.len() - done).min(block.len() - from);
            buf[done..done + n].copy_from_slice(&block[from..from + n]);
            done += n;
        }
        Ok(done)
    }

    /// Exactly `len` bytes at `off`; `None` where the file ends first or the cap is passed.
    fn exact(&mut self, off: u64, len: usize) -> io::Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; len];
        match self.read_some(off, &mut buf) {
            Ok(n) if n == len => Ok(Some(buf)),
            Ok(_) => Ok(None),
            Err(_) if self.flags.over.get() && self.flags.broken.borrow().is_none() => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Read for Head<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.read_some(self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Head<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let next = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(by) => self.size.checked_add_signed(by),
            SeekFrom::Current(by) => self.pos.checked_add_signed(by),
        };
        self.pos = next.ok_or_else(|| io::Error::other("a seek before the start of the file"))?;
        Ok(self.pos)
    }
}

/// What a file's header says, before any pixel data is read.
#[derive(Debug, Clone)]
pub struct Header {
    pub format: ImageFormat,
    /// As stored: the Exif orientation is not applied.
    pub width: u32,
    pub height: u32,
    /// The bytes a decode of it holds: width × height × the bytes per pixel of the colour
    /// type the decoder produces.
    pub need: u64,
    pub orientation: Orientation,
    /// The Exif thumbnail, upright.
    pub thumb: Option<RgbaImage>,
}

impl Header {
    /// The size the picture is shown at: sides swapped where the orientation turns it.
    pub fn upright(&self) -> (u32, u32) {
        if turns(self.orientation) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    /// Whether a picture of the file's first bytes can be made ([`partial`]): the formats
    /// whose partial decode was measured (06 §7.3).
    pub fn has_partials(&self) -> bool {
        matches!(self.format, ImageFormat::Jpeg | ImageFormat::Png)
    }
}

fn turns(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// What the header read found.
#[derive(Debug)]
pub enum Probe {
    Header(Header),
    /// No header within the cap, or one the walk does not follow: not an error — the file is
    /// streamed and the decoder decides.
    Unknown,
    /// The file cannot become a picture, and reading the rest would not change that: why, in
    /// the words a decode of the whole file would use.
    Refused(String),
}

/// Reads the header through `head`. An `Err` is the read's own failure (a cancel, a stream that
/// failed or changed), never the file's content.
pub fn probe(head: &mut Head) -> io::Result<Probe> {
    let found = probe_in(head);
    let broken = head.flags.broken.borrow().clone();
    match broken {
        Some(why) => Err(io::Error::other(why)),
        None => Ok(found),
    }
}

fn probe_in(head: &mut Head) -> Probe {
    match head.exact(0, 2) {
        Ok(Some(start)) if start == [0xFF, 0xD8] => {
            return jpeg(head, true).unwrap_or(Probe::Unknown)
        }
        Err(_) => return Probe::Unknown,
        Ok(_) => {}
    }
    head.pos = 0;
    let flags = head.flags.clone();
    let reader = match ImageReader::new(BufReader::new(&mut *head)).with_guessed_format() {
        Ok(reader) => reader,
        Err(e) => return Probe::Refused(format!("cannot read the image: {e}")),
    };
    let Some(format) = reader.format() else {
        return Probe::Refused("not an image format sopen can read".into());
    };
    if !supported(format) {
        return Probe::Refused(format!("{format:?} images are not supported"));
    }
    let mut decoder = match reader.into_decoder() {
        Ok(decoder) => decoder,
        // Past the cap, or the read failed: nothing is known of the file.
        Err(_) if flags.over.get() || flags.broken.borrow().is_some() => return Probe::Unknown,
        // The file's own fault, where a decode of the whole file fails too.
        Err(e) => return Probe::Refused(format!("cannot decode the image: {e}")),
    };
    let (width, height) = decoder.dimensions();
    let need = decoder.total_bytes();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    Probe::Header(Header {
        format,
        width,
        height,
        need,
        orientation,
        thumb: None,
    })
}

/// The formats the image window opens.
pub fn supported(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Tiff
            | ImageFormat::Gif
    )
}

/// The longest SOF segment: its length, the precision, the two sides, the count and three
/// bytes for each of 255 components.
const SOF_MAX: usize = 8 + 3 * 255;

/// 06 §7.3's walk over a JPEG's marker segments to its SOF. `None`: the walk gives up (the
/// decoder decides); it never searches forward for a marker. `thumb`: whether the Exif's
/// thumbnail is decoded.
fn jpeg(head: &mut Head, thumb: bool) -> Option<Probe> {
    let mut at = 2u64;
    let mut exif: Option<Vec<u8>> = None;
    loop {
        // A marker: FF, any further FF bytes (fill), then the code.
        if head.exact(at, 1).ok()??[0] != 0xFF {
            return None;
        }
        at += 1;
        let mut code = head.exact(at, 1).ok()??[0];
        while code == 0xFF {
            at += 1;
            code = head.exact(at, 1).ok()??[0];
        }
        at += 1;
        // Nothing with no length, no second start, no end and no scan before the frame.
        if matches!(code, 0x00 | 0x01 | 0xD0..=0xD9 | 0xDA) {
            return None;
        }
        let len = head.exact(at, 2).ok()??;
        let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
        if len < 2 {
            return None;
        }
        if matches!(code, 0xC0..=0xCF) && !matches!(code, 0xC4 | 0xC8 | 0xCC) {
            if len > SOF_MAX {
                return None;
            }
            let sof = head.exact(at, len).ok()??;
            return frame(code, &sof, exif.as_deref(), thumb);
        }
        if code == 0xE1 && exif.is_none() && len >= 8 {
            let start = head.exact(at + 2, 6).ok()??;
            if start == *b"Exif\0\0" {
                exif = Some(head.exact(at + 8, len - 8).ok()??);
            }
        }
        at += len as u64;
    }
}

/// The frame a SOF segment (from its length on) declares.
fn frame(code: u8, sof: &[u8], exif: Option<&[u8]>, thumb: bool) -> Option<Probe> {
    let precision = *sof.get(2)?;
    let height = u16::from_be_bytes([*sof.get(3)?, *sof.get(4)?]);
    let width = u16::from_be_bytes([*sof.get(5)?, *sof.get(6)?]);
    let components = usize::from(*sof.get(7)?);
    // A height of 0 is one set later by DNL: not followed.
    if sof.len() != 8 + 3 * components
        || width == 0
        || height == 0
        || !(1..=4).contains(&components)
    {
        return None;
    }
    let (width, height) = (u32::from(width), u32::from(height));
    if !matches!(code, 0xC0..=0xC2) || precision != 8 {
        let kind = match code {
            0xC0..=0xC2 => "a ",
            0xC3 | 0xC7 | 0xCB | 0xCF => "a lossless ",
            0xC9..=0xCE => "an arithmetic-coded ",
            _ => "a hierarchical ",
        };
        return Some(Probe::Refused(format!(
            "cannot decode the image: {kind}{precision}-bit JPEG (SOF{}), {width}×{height}, is \
             not supported",
            code - 0xC0
        )));
    }
    // One component decodes to one byte a pixel; three and four (CMYK becomes RGB) to three.
    let per_pixel = if components == 1 { 1 } else { 3 };
    let orientation = exif
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);
    Some(Probe::Header(Header {
        format: ImageFormat::Jpeg,
        width,
        height,
        need: u64::from(width) * u64::from(height) * per_pixel,
        orientation,
        thumb: exif
            .filter(|_| thumb)
            .and_then(|exif| thumbnail(exif, orientation)),
    }))
}

/// The thumbnail an Exif block carries (IFD1's JPEGInterchangeFormat and its length), decoded
/// and turned upright. `None` when there is none, or it is not a small JPEG.
fn thumbnail(exif: &[u8], orientation: Orientation) -> Option<RgbaImage> {
    let little = match exif.get(..4)? {
        [b'I', b'I', 0x2A, 0x00] => true,
        [b'M', b'M', 0x00, 0x2A] => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<usize> {
        let b = exif.get(at..at.checked_add(2)?)?;
        Some(usize::from(if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }))
    };
    let u32_at = |at: usize| -> Option<usize> {
        let b = exif.get(at..at.checked_add(4)?)?;
        let v = if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        };
        usize::try_from(v).ok()
    };
    let ifd0 = u32_at(4)?;
    let ifd1 = u32_at(
        ifd0.checked_add(2)?
            .checked_add(u16_at(ifd0)?.checked_mul(12)?)?,
    )?;
    if ifd1 == 0 {
        return None;
    }
    let (mut offset, mut length) = (None, None);
    for i in 0..u16_at(ifd1)? {
        let entry = ifd1.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        // Both are one LONG (type 4), its value in the entry.
        match (u16_at(entry)?, u16_at(entry + 2)?) {
            (0x0201, 4) => offset = u32_at(entry + 8),
            (0x0202, 4) => length = u32_at(entry + 8),
            _ => {}
        }
    }
    let offset = offset?;
    let bytes = exif.get(offset..offset.checked_add(length?)?)?;
    // Its own header first: a thumbnail whose frame is larger than a thumbnail is not decoded.
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    // (And not its own thumbnail: an Exif nested in an Exif is not followed.)
    match jpeg(&mut Head::of(bytes), false)? {
        Probe::Header(own) if u64::from(own.width) * u64::from(own.height) * 4 <= THUMB_RGBA => {}
        _ => return None,
    }
    let mut thumb = image::load_from_memory_with_format(bytes, ImageFormat::Jpeg).ok()?;
    thumb.apply_orientation(orientation);
    Some(thumb.to_rgba8())
}

/// This machine's physical memory, in bytes.
pub fn physical_memory() -> Option<u64> {
    // SAFETY: `sysconf` reads two of the system's constants and touches no memory of ours.
    let (pages, page) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    (pages > 0 && page > 0).then(|| (pages as u64).saturating_mul(page as u64))
}

/// `37.3 GB`, `16 GB`, `600 MB`: a memory size, in binary units under the names everyone
/// reads them by.
pub fn memory_text(bytes: u64) -> String {
    let (value, unit) = if bytes >= 1 << 30 {
        (bytes as f64 / (1u64 << 30) as f64, "GB")
    } else {
        (bytes as f64 / (1u64 << 20) as f64, "MB")
    };
    let text = format!("{value:.1}");
    format!("{} {unit}", text.strip_suffix(".0").unwrap_or(&text))
}

/// Why the window asks before it goes on (06 §7.3): what going on would hold in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// The decoded pixels, of an image this large.
    Decode { need: u64, size: (u32, u32) },
    /// The file's own bytes: a file this large is read into memory whole before its header
    /// can promise anything about the picture in it.
    Read { bytes: u64 },
    /// Both over the budget: the file's bytes and the decoded pixels are held at once.
    Both {
        file: u64,
        need: u64,
        size: (u32, u32),
    },
}

impl Ask {
    pub fn need(self) -> u64 {
        match self {
            Ask::Decode { need, .. } => need,
            Ask::Read { bytes } => bytes,
            Ask::Both { file, need, .. } => file.saturating_add(need),
        }
    }

    /// Whether the machine does not have that much memory at all: said in the error colour.
    pub fn exceeds(self, physical: Option<u64>) -> bool {
        physical.is_some_and(|p| self.need() > p)
    }

    /// The memory line, without its keys.
    pub fn line(self, physical: Option<u64>) -> String {
        let what = match self {
            Ask::Decode { need, size: (w, h) } => format!(
                "decoding this image needs {} of memory ({w}×{h})",
                memory_text(need)
            ),
            Ask::Read { bytes } => format!(
                "reading this image needs {} of memory (the file itself)",
                memory_text(bytes)
            ),
            Ask::Both {
                file, size: (w, h), ..
            } => format!(
                "reading and decoding this image needs {} of memory ({w}×{h}; the file itself {})",
                memory_text(self.need()),
                memory_text(file)
            ),
        };
        match physical {
            Some(p) => format!("{what}; this machine has {}", memory_text(p)),
            None => what,
        }
    }
}

/// What must be asked before `need` bytes of pixels are decoded, if anything.
pub fn ask_decode(need: u64, size: (u32, u32), budget: u64) -> Option<Ask> {
    (need > budget).then_some(Ask::Decode { need, size })
}

/// What must be asked before a file of `bytes` is read into memory, if anything.
pub fn ask_read(bytes: u64, budget: u64) -> Option<Ask> {
    (bytes > budget).then_some(Ask::Read { bytes })
}

/// What must be asked before a file of `file` bytes is read and decoded (`decode`: the bytes
/// its header says the pixels need, and its size; `None` when no header was found): the
/// decode, the file itself, or — both over the budget — the two together, which is what is
/// then held at once.
pub fn ask_open(file: u64, decode: Option<(u64, (u32, u32))>, budget: u64) -> Option<Ask> {
    let decode = decode.and_then(|(need, size)| ask_decode(need, size, budget));
    match (decode, ask_read(file, budget)) {
        (Some(Ask::Decode { need, size }), Some(_)) => Some(Ask::Both { file, need, size }),
        (decode, read) => decode.or(read),
    }
}

/// A file's size as the loading line says it: `24.0 GB`, `16.6 MB`, `74 bytes`.
pub fn size_text(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} bytes"),
        1_000..1_000_000 => format!("{:.1} kB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

#[cfg(test)]
thread_local! {
    static RESERVED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Tests: the most this thread reserved in one piece so far.
#[cfg(test)]
pub(crate) fn largest_reserved() -> u64 {
    RESERVED.with(std::cell::Cell::get)
}

mod sealed {
    /// Only this module names it: nothing outside can add a [`super::Sample`].
    pub trait Sealed {}
    impl Sealed for u8 {}
    impl Sealed for u16 {}
    impl Sealed for f32 {}
}

/// The numbers a decoder writes pixels as: `u8`, `u16` and `f32`, and — sealed — nothing
/// else. The unsafe code below relies on exactly these: no padding, any bit pattern valid,
/// all-zero bits a value.
pub trait Sample: Copy + sealed::Sealed {}
impl Sample for u8 {}
impl Sample for u16 {}
impl Sample for f32 {}

/// `len` zeroed samples, asked of the allocator with a check (06 §7.3): an allocation the
/// system refuses is an error here, not an abort.
pub fn reserved<T: Sample>(len: usize) -> Result<Vec<T>> {
    let bytes = len as u64 * size_of::<T>() as u64;
    #[cfg(test)]
    RESERVED.with(|most| most.set(most.get().max(bytes)));
    let refused = || {
        anyhow!(
            "not enough memory: the system refused {} for this image",
            memory_text(bytes)
        )
    };
    let layout = std::alloc::Layout::array::<T>(len).map_err(|_| refused())?;
    if layout.size() == 0 {
        return Ok(Vec::new());
    }
    // Zeroed by the allocator, not written here: a buffer larger than the machine's memory is
    // not touched before the decoder fills it, and one the decoder never fills (a file that
    // fails at its first row) costs nothing.
    // SAFETY: the layout is `len` samples and not empty; a null return is the refusal handled
    // below; all-zero bits are a valid `u8`, `u16` and `f32` (the only `Sample`s); the `Vec`
    // takes the allocation with the layout it was made with (capacity = length = `len`).
    unsafe {
        let at = std::alloc::alloc_zeroed(layout).cast::<T>();
        if at.is_null() {
            return Err(refused());
        }
        Ok(Vec::from_raw_parts(at, len, len))
    }
}

/// The samples as the bytes a decoder fills.
fn bytes_of<T: Sample>(samples: &mut [T]) -> &mut [u8] {
    // SAFETY: `T` is `u8`, `u16` or `f32` (the trait is implemented for nothing else here):
    // no padding, every bit pattern valid, and `u8` has no alignment to break. The slice
    // covers exactly the same memory, borrowed as long.
    unsafe {
        std::slice::from_raw_parts_mut(samples.as_mut_ptr().cast::<u8>(), size_of_val(samples))
    }
}

/// `DynamicImage::from_decoder`, with the buffer reserved first instead of allocated
/// unchecked (06 §7.3).
pub fn decode_reserved(decoder: impl ImageDecoder) -> Result<DynamicImage> {
    use image::ColorType as C;
    let (w, h) = decoder.dimensions();
    let total = usize::try_from(decoder.total_bytes())
        .ok()
        .filter(|n| *n <= isize::MAX as usize)
        .context("the image is larger than this machine can address")?;
    fn read<T: Sample>(decoder: impl ImageDecoder, total: usize) -> Result<Vec<T>> {
        let mut buf = reserved::<T>(total / size_of::<T>())?;
        decoder
            .read_image(bytes_of(&mut buf))
            .context("cannot decode the image")?;
        Ok(buf)
    }
    let image = match decoder.color_type() {
        C::L8 => ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageLuma8),
        C::La8 => ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageLumaA8),
        C::Rgb8 => ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageRgb8),
        C::Rgba8 => {
            ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageRgba8)
        }
        C::L16 => ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageLuma16),
        C::La16 => {
            ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageLumaA16)
        }
        C::Rgb16 => {
            ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageRgb16)
        }
        C::Rgba16 => {
            ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageRgba16)
        }
        C::Rgb32F => {
            ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageRgb32F)
        }
        C::Rgba32F => {
            ImageBuffer::from_raw(w, h, read(decoder, total)?).map(DynamicImage::ImageRgba32F)
        }
        other => anyhow::bail!("cannot decode the image: {other:?} pixels are not supported"),
    };
    let mut image = image.context("cannot decode the image: its size and its pixels disagree")?;
    // As `from_decoder` presumes.
    image.set_rgb_primaries(image::metadata::Cicp::SRGB.primaries);
    image.set_transfer_function(image::metadata::Cicp::SRGB.transfer);
    Ok(image)
}

/// The size a `w`×`h` picture is shown at inside a square of `side`.
fn fit(w: u32, h: u32, side: u32) -> (u32, u32) {
    if w <= side && h <= side {
        return (w.max(1), h.max(1));
    }
    let scale = f64::from(side) / f64::from(w.max(h));
    (
        ((f64::from(w) * scale) as u32).clamp(1, side),
        ((f64::from(h) * scale) as u32).clamp(1, side),
    )
}

/// A picture of the file's first bytes (06 §7.3), upright, no side over `side`: a JPEG's
/// prefix decoded (a progressive one gives the whole frame, coarse; a baseline one its top
/// rows), a plain PNG's rows so far, an interlaced PNG's last complete pass. `None` when the
/// bytes give no picture yet, or the format's partial decode is not one sopen shows.
pub fn partial<R: Read + Seek>(prefix: R, header: &Header, side: u32) -> Option<RgbaImage> {
    let prefix = BufReader::new(prefix);
    let picture = match header.format {
        ImageFormat::Jpeg => {
            let decoder = image::codecs::jpeg::JpegDecoder::new(prefix).ok()?;
            // Never more memory than the header said (what the memory line was about): a
            // decoder that reads another frame than the walk did gets no buffer.
            if decoder.total_bytes() > header.need {
                return None;
            }
            let whole = decode_reserved(decoder).ok()?;
            let (w, h) = fit(whole.width(), whole.height(), side);
            whole.thumbnail_exact(w, h).to_rgba8()
        }
        ImageFormat::Png => png_rows(prefix, side)?,
        _ => return None,
    };
    let mut picture = DynamicImage::ImageRgba8(picture);
    picture.apply_orientation(header.orientation);
    Some(picture.to_rgba8())
}

/// Adam7's passes: where each starts and how far apart its samples are, across and down, and
/// the block a sample stands for until later passes refine it (its footprint).
const ADAM7: [Pass; 7] = [
    Pass::new(0, 8, 0, 8, 8, 8),
    Pass::new(4, 8, 0, 8, 4, 8),
    Pass::new(0, 4, 4, 8, 4, 4),
    Pass::new(2, 4, 0, 4, 2, 4),
    Pass::new(0, 2, 2, 4, 2, 2),
    Pass::new(1, 2, 0, 2, 1, 2),
    Pass::new(0, 1, 1, 2, 1, 1),
];

/// One pass over an image: every `dx`-th column from `x0` of every `dy`-th row from `y0`.
#[derive(Clone, Copy)]
struct Pass {
    x0: u32,
    dx: u32,
    y0: u32,
    dy: u32,
    /// The footprint of a sample.
    fw: u32,
    fh: u32,
}

impl Pass {
    const fn new(x0: u32, dx: u32, y0: u32, dy: u32, fw: u32, fh: u32) -> Self {
        Self {
            x0,
            dx,
            y0,
            dy,
            fw,
            fh,
        }
    }

    /// A plain image: every pixel, once.
    const WHOLE: Pass = Pass::new(0, 1, 0, 1, 1, 1);

    /// The pass's sample whose footprint covers column `x`, if one does.
    fn sample(self, x: u32, width: u32) -> Option<u32> {
        let from = x.checked_sub(self.x0)?;
        (from % self.dx < self.fw)
            .then_some(from / self.dx)
            .filter(|i| *i < width)
    }
}

/// A PNG's rows so far through the `png` crate's row reader, sampled down as they come (no
/// full-size buffer). A plain one's rows from the top, the rest left clear. An interlaced
/// one's passes laid over one another as a progressive display lays them — each sample over
/// its footprint, so a later pass refines what the earlier ones drew — as of the last pass
/// that is complete: the whole frame, finer with each pass.
fn png_rows<R: io::BufRead + Seek>(prefix: R, side: u32) -> Option<RgbaImage> {
    let mut decoder = png::Decoder::new(prefix);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let (w, h, interlaced) = {
        let info = reader.info();
        (info.width, info.height, info.interlaced)
    };
    let channels = match reader.output_color_type() {
        (png::ColorType::Grayscale, png::BitDepth::Eight) => 1,
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => 2,
        (png::ColorType::Rgb, png::BitDepth::Eight) => 3,
        (png::ColorType::Rgba, png::BitDepth::Eight) => 4,
        _ => return None,
    };
    if w == 0 || h == 0 {
        return None;
    }
    let (out_w, out_h) = fit(w, h, side);
    // The pixel of the image each pixel of the picture shows.
    let source_x = |x: u32| (u64::from(x) * u64::from(w) / u64::from(out_w)) as u32;
    let source_y = |y: u32| (u64::from(y) * u64::from(h) / u64::from(out_h)) as u32;
    let passes: &[Pass] = if interlaced { &ADAM7 } else { &[Pass::WHOLE] };
    // What is being drawn, and — interlaced — the picture as of the last complete pass.
    let mut work = RgbaImage::new(out_w, out_h);
    let mut shown: Option<RgbaImage> = None;
    let mut drawn = false;
    for pass in passes {
        let pass_w = w.saturating_sub(pass.x0).div_ceil(pass.dx);
        let pass_h = h.saturating_sub(pass.y0).div_ceil(pass.dy);
        if pass_w == 0 || pass_h == 0 {
            // A pass with no samples in an image this small: the stream has none of it.
            continue;
        }
        for line in 0..pass_h {
            let row = match reader.next_row() {
                Ok(Some(row)) => row.data(),
                // The bytes end here (or are wrong from here): what came before is the picture.
                Ok(None) | Err(_) => {
                    return if interlaced {
                        shown
                    } else {
                        drawn.then_some(work)
                    };
                }
            };
            if row.len() != pass_w as usize * channels {
                return shown;
            }
            // The rows of the image this row stands for, and the picture's rows that show them.
            let top = pass.y0 + line * pass.dy;
            let below = top.saturating_add(pass.fh);
            let first = (u64::from(top) * u64::from(out_h)).div_ceil(u64::from(h)) as u32;
            for y in (first..out_h).take_while(|y| source_y(*y) < below) {
                for x in 0..out_w {
                    let Some(from) = pass.sample(source_x(x), pass_w) else {
                        continue;
                    };
                    let from = from as usize;
                    let p = &row[from * channels..(from + 1) * channels];
                    let rgba = match channels {
                        1 => [p[0], p[0], p[0], 255],
                        2 => [p[0], p[0], p[0], p[1]],
                        3 => [p[0], p[1], p[2], 255],
                        _ => [p[0], p[1], p[2], p[3]],
                    };
                    work.put_pixel(x, y, image::Rgba(rgba));
                }
                drawn = true;
            }
        }
        if interlaced {
            match &mut shown {
                Some(shown) => shown.copy_from_slice(&work),
                None => shown = Some(work.clone()),
            }
        }
    }
    if interlaced {
        shown
    } else {
        Some(work)
    }
}
