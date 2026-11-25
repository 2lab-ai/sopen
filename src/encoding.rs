//! Text encodings (`.prd/08-viewer.md` §7): the list a text view decodes by (§7.1), the default
//! picked from a file's first 8 KiB (§7.2), and every rule the text model places lines and
//! characters by over a byte range (§7.3) — one decoder per encoding giving each character and
//! the bytes it takes, the LF and CR units at their alignment, and the four places a character
//! starts (a grid cut, a restart, a jump, a place inside a line not wrapped). Pure functions over
//! byte slices and offsets: no I/O, no screen (D31). A ranged read decodes as the whole file
//! does: every start a rule gives is a character start of the whole-file decode, and decoding
//! from it gives the whole-file decode's characters (`tests.rs` holds every encoding to that).
//!
//! CP949 / EUC-KR is VM-4's (§13): its decoder (encoding_rs's `EUC_KR`), its rules, its phase
//! and the default's step 3 are `cp949.rs`, held to the whole decode by `cp949_tests.rs`. Where
//! nothing proves a character start, its rules give `None` (D28).

use crate::gui::chrome;
use crate::kind::SNIFF_BYTES;

/// A text encoding of §7.1, in the list's order ([`Encoding::ALL`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// One byte, 0x00–0x7F; every byte from 0x80 up is one U+FFFD.
    Ascii,
    /// 1–4 bytes; each maximal bad sequence is one U+FFFD, as before VM-2.
    Utf8,
    /// 2-byte units, low byte first, and surrogate pairs; an unpaired surrogate is one U+FFFD.
    Utf16Le,
    /// 2-byte units, high byte first, and surrogate pairs.
    Utf16Be,
    /// 2-byte units, low byte first, no pairs: every surrogate unit (D800–DFFF) is one U+FFFD.
    Ucs2Le,
    /// 2-byte units, high byte first, no pairs.
    Ucs2Be,
    /// 4-byte units, low byte first (= UCS-4, D16): a value above 10FFFF or a surrogate is one
    /// U+FFFD.
    Utf32Le,
    /// 4-byte units, high byte first.
    Utf32Be,
    /// ISO-8859-1: byte n is U+00nn, so 0x80–0x9F are the C1 controls (drawn by D32).
    Latin1,
    /// 0x00–0x7F as ASCII, 0x80–0xFF by the skin's table ([`chrome::cp437`]).
    Cp437,
    /// Microsoft's Unified Hangul Code, a superset of EUC-KR / KS X 1001 (§7.4): VM-4's.
    Cp949,
}

/// A character as decoded: what it is (U+FFFD for bytes that are not one) and the bytes it takes
/// in the file — the text view's `wrap::Cell` (it is this one since VM-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub len: u8,
}

/// The file bytes a rule reads around a place: `bytes`, from offset `at`. Offsets count from byte
/// 0 of the file, or from any multiple of the unit (a line's start): a rule uses only their
/// alignment, and answers in the same count. `anchored`: `at` is a character start the model
/// knows — a line's or a piece's start (a grid cut), or a CP949 checkpoint's
/// ([`Encoding::phase`]) — so nothing before it is looked for, and a rule asked about `at` itself
/// answers `at` (a stray UTF-8 continuation byte that starts a line or a piece is its own U+FFFD,
/// not skipped: `left` 0 of a span, `Wrapper::start_at`).
#[derive(Debug, Clone, Copy)]
pub struct Around<'a> {
    pub at: u64,
    pub bytes: &'a [u8],
    pub anchored: bool,
}

impl<'a> Around<'a> {
    /// Bytes read from `at`, a place inside a line.
    pub fn new(at: u64, bytes: &'a [u8]) -> Self {
        Self {
            at,
            bytes,
            anchored: false,
        }
    }

    /// Bytes read from `at`, a character start the model knows (a line's start).
    pub fn anchored(at: u64, bytes: &'a [u8]) -> Self {
        Self {
            at,
            bytes,
            anchored: true,
        }
    }

    fn end(&self) -> u64 {
        self.at + self.bytes.len() as u64
    }

    /// What was read of the `n` bytes before `x`. A place past the bytes read has nothing known
    /// before it — the bytes up to it were not read — so it gets none (a caller's slip: a debug
    /// build says so), never the last bytes read as if they ended at `x`.
    fn before(&self, x: u64, n: usize) -> &'a [u8] {
        debug_assert!(x <= self.end(), "the bytes before {x} were not read");
        let lo = x.saturating_sub(n as u64).max(self.at);
        if x > self.end() || x <= lo {
            return &[];
        }
        &self.bytes[(lo - self.at) as usize..(x - self.at) as usize]
    }

    /// What was read of the `n` bytes from `x`.
    fn after(&self, x: u64, n: usize) -> &'a [u8] {
        let hi = x.saturating_add(n as u64).min(self.end());
        if x < self.at || hi <= x {
            return &[];
        }
        &self.bytes[(x - self.at) as usize..(hi - self.at) as usize]
    }
}

/// CP949's look-back W (§7.4): its last sync byte is looked for this many bytes before a place.
pub const W: usize = 256;

/// The bytes after a place that rules 2–4 read, in every encoding but CP949, whose restart reads
/// its window on to the first sync byte: UTF-8's up to three continuation bytes, the unit a
/// UTF-16 place rounds up into.
pub const AHEAD: usize = 4;

/// The default (§7.2): the encoding, and whether step 2 or 3 guessed it — the label's
/// `(guessed)` while that encoding is on screen, also after switching back to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pick {
    pub encoding: Encoding,
    pub guessed: bool,
}

impl Pick {
    /// The bar's encoding (§7.5) while `shown` is on screen: its label, then ` (BOM)` while a
    /// BOM is skipped under it, ` (guessed)` while it is the one step 2 or 3 picked. `head`: the
    /// file's first bytes (four, or the whole file when shorter).
    pub fn label(self, shown: Encoding, head: &[u8]) -> String {
        let mut out = shown.label().to_string();
        if shown.bom_len(head) > 0 {
            out.push_str(" (BOM)");
        }
        if self.guessed && self.encoding == shown {
            out.push_str(" (guessed)");
        }
        out
    }
}

/// §7.2's default, from the first [`SNIFF_BYTES`] of `head` (more is ignored, so a file the kind
/// sniff routed to text by its pattern is decoded by it: D27). `truncated`: the file goes on past
/// `head` (step 3's allowance for a character cut by the sample's end).
///
/// 1. A BOM, the longest first, whatever the file's size (D37).
/// 2. Else the NUL pattern: in the order UTF-32LE, UTF-32BE, UTF-16LE, UTF-16BE, the first whose
///    sample holds at least 4 whole units, at least 3/4 of them ASCII units (D17).
/// 3. Else CP949 by D39's (a)–(e) ([`cp949::guess`]), shown `(guessed)`.
/// 4. Else UTF-8, valid or not. ASCII, UCS-2, Latin-1 and CP437 are never defaults.
pub fn pick(head: &[u8], truncated: bool) -> Pick {
    use Encoding::*;
    let truncated = truncated || head.len() > SNIFF_BYTES;
    let sample = &head[..head.len().min(SNIFF_BYTES)];
    let chosen = |encoding, guessed| Pick { encoding, guessed };
    if let Some(e) = [Utf32Le, Utf32Be, Utf8, Utf16Le, Utf16Be]
        .into_iter()
        .find(|e| sample.starts_with(e.bom()))
    {
        return chosen(e, false);
    }
    if let Some(e) = [Utf32Le, Utf32Be, Utf16Le, Utf16Be]
        .into_iter()
        .find(|&e| nul_pattern(e, sample))
    {
        return chosen(e, true);
    }
    if cp949::guess(sample, truncated) {
        return chosen(Cp949, true);
    }
    chosen(Utf8, false)
}

/// Step 2 for one wide encoding: at least 4 whole units in `sample`, at least 3/4 of them ASCII
/// units — 0x09, 0x0A, 0x0D or 0x20–0x7E, the printable byte beside one 0x00 (UTF-16) or three
/// (UTF-32).
fn nul_pattern(e: Encoding, sample: &[u8]) -> bool {
    let n = e.unit();
    let be = e.big_endian();
    let units = sample.len() / n;
    let ascii = sample
        .chunks_exact(n)
        .filter(|u| {
            let v = if n == 2 {
                u32::from(u16_of(u, be))
            } else {
                u32_of(u, be)
            };
            matches!(v, 0x09 | 0x0A | 0x0D | 0x20..=0x7E)
        })
        .count();
    units >= 4 && 4 * ascii >= 3 * units
}

impl Encoding {
    /// §7.1's list, in its order.
    pub const ALL: [Encoding; 11] = [
        Encoding::Ascii,
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Ucs2Le,
        Encoding::Ucs2Be,
        Encoding::Utf32Le,
        Encoding::Utf32Be,
        Encoding::Latin1,
        Encoding::Cp437,
        Encoding::Cp949,
    ];

    /// As the bar shows it (§7.5): `UTF-16LE`, `Latin-1`, `CP949`.
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Ascii => "ASCII",
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16LE",
            Encoding::Utf16Be => "UTF-16BE",
            Encoding::Ucs2Le => "UCS-2LE",
            Encoding::Ucs2Be => "UCS-2BE",
            Encoding::Utf32Le => "UTF-32LE",
            Encoding::Utf32Be => "UTF-32BE",
            Encoding::Latin1 => "Latin-1",
            Encoding::Cp437 => "CP437",
            Encoding::Cp949 => "CP949",
        }
    }

    /// The encoding a label names, in any case, among those decoded today ([`Self::supported`]).
    pub fn from_label(name: &str) -> Option<Encoding> {
        Self::ALL
            .into_iter()
            .find(|e| e.supported() && e.label().eq_ignore_ascii_case(name))
    }

    /// Whether it is decoded: every entry (CP949 since VM-4 filled its slot).
    pub fn supported(self) -> bool {
        self != Encoding::Cp949 || cp949::SUPPORTED
    }

    /// The code unit, in bytes: 1, 2 or 4. Every offset the text model makes is a multiple of it,
    /// counted from byte 0 (§7.3).
    pub fn unit(self) -> usize {
        match self {
            Encoding::Utf16Le | Encoding::Utf16Be | Encoding::Ucs2Le | Encoding::Ucs2Be => 2,
            Encoding::Utf32Le | Encoding::Utf32Be => 4,
            _ => 1,
        }
    }

    /// `x` rounded down to a multiple of the unit.
    pub fn align_down(self, x: u64) -> u64 {
        x - x % self.unit() as u64
    }

    /// `x` rounded up to a multiple of the unit.
    pub fn align_up(self, x: u64) -> u64 {
        self.align_down(x.saturating_add(self.unit() as u64 - 1))
    }

    fn big_endian(self) -> bool {
        matches!(
            self,
            Encoding::Utf16Be | Encoding::Ucs2Be | Encoding::Utf32Be
        )
    }

    /// The byte-order mark skipped under this encoding (D26, D37): UTF-8's; UTF-16's of its order
    /// also under UCS-2; UTF-32's under UTF-32 (= UCS-4). Empty for the rest, under which a BOM's
    /// bytes are text from byte 0.
    pub fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8 => &[0xEF, 0xBB, 0xBF],
            Encoding::Utf16Le | Encoding::Ucs2Le => &[0xFF, 0xFE],
            Encoding::Utf16Be | Encoding::Ucs2Be => &[0xFE, 0xFF],
            Encoding::Utf32Le => &[0xFF, 0xFE, 0x00, 0x00],
            Encoding::Utf32Be => &[0x00, 0x00, 0xFE, 0xFF],
            _ => &[],
        }
    }

    /// The bytes skipped at the start of a file whose first bytes are `head` under this
    /// encoding: its BOM's length when the file starts with it, else 0. Line 1 starts there.
    pub fn bom_len(self, head: &[u8]) -> usize {
        let bom = self.bom();
        if !bom.is_empty() && head.starts_with(bom) {
            bom.len()
        } else {
            0
        }
    }

    /// The LF unit a line ends at (§7.3): `0a 00` in UTF-16LE and UCS-2LE, `00 0a` in the BE
    /// pair, `0a 00 00 00` / `00 00 00 0a` in UTF-32, the byte 0x0A in the rest.
    pub fn lf(self) -> &'static [u8] {
        match self {
            Encoding::Utf16Le | Encoding::Ucs2Le => &[0x0A, 0x00],
            Encoding::Utf16Be | Encoding::Ucs2Be => &[0x00, 0x0A],
            Encoding::Utf32Le => &[0x0A, 0x00, 0x00, 0x00],
            Encoding::Utf32Be => &[0x00, 0x00, 0x00, 0x0A],
            _ => b"\n",
        }
    }

    /// The CR unit: a line that ends `CR LF` has it just before its LF unit.
    pub fn cr(self) -> &'static [u8] {
        match self {
            Encoding::Utf16Le | Encoding::Ucs2Le => &[0x0D, 0x00],
            Encoding::Utf16Be | Encoding::Ucs2Be => &[0x00, 0x0D],
            Encoding::Utf32Le => &[0x0D, 0x00, 0x00, 0x00],
            Encoding::Utf32Be => &[0x00, 0x00, 0x00, 0x0D],
            _ => b"\r",
        }
    }

    /// Whether the two end lines at the same unit, so a change between them keeps the line index
    /// (D22): UTF-8, ASCII, Latin-1, CP437 and CP949 share the byte 0x0A; UTF-16LE and UCS-2LE
    /// share `0a 00`, UTF-16BE and UCS-2BE `00 0a`; each UTF-32 has its own.
    pub fn same_lines(self, other: Encoding) -> bool {
        self.lf() == other.lf()
    }

    /// The first LF unit in `bytes` (read from offset `at`) at an aligned offset — never a 0x0A
    /// byte inside another unit (U+AC0A 갊 is `0a ac` in UTF-16LE): its index in `bytes`.
    pub fn find_lf(self, bytes: &[u8], at: u64) -> Option<usize> {
        let (lf, k) = (self.lf(), self.lf_byte());
        let mut from = 0;
        while let Some(j) = bytes[from..].iter().position(|&b| b == b'\n') {
            let j = from + j;
            if let Some(i) = j.checked_sub(k).filter(|&i| self.is_unit(bytes, at, i, lf)) {
                return Some(i);
            }
            from = j + 1;
        }
        None
    }

    /// The last LF unit in `bytes` (read from offset `at`) at an aligned offset: its index.
    pub fn rfind_lf(self, bytes: &[u8], at: u64) -> Option<usize> {
        let (lf, k) = (self.lf(), self.lf_byte());
        let mut to = bytes.len();
        while let Some(j) = bytes[..to].iter().rposition(|&b| b == b'\n') {
            if let Some(i) = j.checked_sub(k).filter(|&i| self.is_unit(bytes, at, i, lf)) {
                return Some(i);
            }
            to = j;
        }
        None
    }

    /// Whether `bytes` (read from offset `at`) end with the CR unit at an aligned offset — a
    /// line's `\r` before its LF unit.
    pub fn ends_with_cr(self, bytes: &[u8], at: u64) -> bool {
        let n = self.unit();
        bytes.len() >= n && self.is_unit(bytes, at, bytes.len() - n, self.cr())
    }

    /// Where the 0x0A byte sits in the LF unit.
    fn lf_byte(self) -> usize {
        if self.big_endian() {
            self.unit() - 1
        } else {
            0
        }
    }

    /// Whether `unit` is in `bytes` at index `i`, an aligned offset (`bytes` read from `at`).
    fn is_unit(self, bytes: &[u8], at: u64, i: usize, unit: &[u8]) -> bool {
        (at + i as u64).is_multiple_of(self.unit() as u64)
            && bytes.get(i..i + unit.len()) == Some(unit)
    }

    /// One decoder per encoding (§7.3): the characters of `bytes` — a piece of a line that starts
    /// at a character start — and the bytes each takes. Invalid input never shifts alignment:
    /// after a U+FFFD the next unit is decoded at the next aligned offset (§7.1). `whole`: the
    /// piece runs to the end of what it is cut from (the line, or a restart point), so a
    /// character cut in two at its end is bad bytes and a partial last unit one U+FFFD; otherwise
    /// they are left for the next piece. Returns the characters and the bytes they cover. UTF-8
    /// is the text view's `wrap::decode` before VM-2.
    pub fn decode(self, bytes: &[u8], whole: bool) -> (Vec<Cell>, usize) {
        let be = self.big_endian();
        match self {
            Encoding::Ascii => bytewise(bytes, |b| {
                if b < 0x80 {
                    char::from(b)
                } else {
                    char::REPLACEMENT_CHARACTER
                }
            }),
            Encoding::Utf8 => decode_utf8(bytes, whole),
            Encoding::Utf16Le | Encoding::Utf16Be => decode_utf16(bytes, whole, be),
            Encoding::Ucs2Le | Encoding::Ucs2Be => decode_fixed(bytes, whole, 2, |u| {
                char::from_u32(u32::from(u16_of(u, be)))
            }),
            Encoding::Utf32Le | Encoding::Utf32Be => {
                decode_fixed(bytes, whole, 4, |u| char::from_u32(u32_of(u, be)))
            }
            Encoding::Latin1 => bytewise(bytes, char::from),
            Encoding::Cp437 => bytewise(bytes, |b| {
                if b < 0x80 {
                    char::from(b)
                } else {
                    chrome::cp437(b)
                }
            }),
            Encoding::Cp949 => cp949::decode(bytes, whole),
        }
    }

    /// How many bytes before a place the rules read (D36), so the text model supplies them
    /// without knowing the encoding: 3 in UTF-8 (a character cut at a grid point lacks at least
    /// one of its at most four bytes; the text view's `grid_cut` before VM-2 read 4, and the
    /// fourth never moves the cut), 2 in UTF-16 (the unit before), 0 in UCS-2, UTF-32 and the one-byte
    /// encodings, [`W`] in CP949 — more there when anchored at a line start within REACH (§7.4).
    pub fn look_back(self) -> usize {
        match self {
            Encoding::Utf8 => 3,
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            Encoding::Cp949 => W,
            _ => 0,
        }
    }

    /// §7.3 rule 1, a grid cut (D36): where both pieces cut at grid point `g` — a block edge, a
    /// GRID point of a line whose start is not known, `LINE_MAX` — meet, in [g − 3, g]. `g` is
    /// aligned; `near` holds [`Self::look_back`] bytes before it (fewer only when anchored at a
    /// line start closer than that). UTF-8: the start of a character cut in two at `g`, found
    /// from the bytes before it alone, as before VM-2 (`cut_char_start` below); UTF-16:
    /// g − 2 when the unit there is a high surrogate; the rest: `g`. The piece before decoded
    /// not `whole` ends at the same place.
    ///
    /// `None`: nothing proves a character start there — CP949 only (D28), which then shows the
    /// bytes from the place as placeholder marks up to the start [`Self::restart_start`] finds
    /// unanchored (its first sync byte, §7.4).
    pub fn grid_cut(self, near: Around, g: u64) -> Option<u64> {
        debug_assert_eq!(g % self.unit() as u64, 0, "a grid point is aligned");
        Some(match self {
            Encoding::Utf8 => {
                let before = near.before(g, 3);
                g - (before.len() - cut_char_start(before)) as u64
            }
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let before = near.before(g, 2);
                if before.len() == 2 && is_high(u16_of(before, self.big_endian())) {
                    g - 2
                } else {
                    g
                }
            }
            Encoding::Cp949 => return cp949::grid_cut(near, g),
            _ => g,
        })
    }

    /// §7.3 rule 2, a restart's start (wrap's restart near a 16 KiB multiple, `Wrapper::restart`):
    /// the first character start in a window that starts at `x`, forward. `x` is rounded up to
    /// the unit (the window's first unit); then UTF-8 skips up to 3 continuation bytes, as before
    /// VM-2 (`Wrapper::restart`); UTF-16 moves past that unit only when it is a low surrogate
    /// whose preceding unit is a high surrogate (D38); the rest take it. `near` holds
    /// [`Self::look_back`] bytes before `x` rounded down to the unit, and [`AHEAD`] from `x`.
    /// `None`: CP949 with no sync byte in the window and no phase known, as for
    /// [`Self::grid_cut`].
    pub fn restart_start(self, near: Around, x: u64) -> Option<u64> {
        if self == Encoding::Cp949 {
            return cp949::restart_start(near, x);
        }
        Some(self.forward(near, self.align_up(x)))
    }

    /// §7.3 rule 3, a jump's place — End, Go to `0x…`, `at=`, the scroll bar by bytes, the hex
    /// view's place: `p` rounded down to the unit, and in UTF-16 back one unit only across a
    /// valid pair (a low surrogate whose preceding unit is a high one), so a jump onto a pair's
    /// low half shows the pair whole; a standalone low surrogate stays, one U+FFFD as the
    /// whole-file decode shows it (D38). It picks the row; the row's own start comes from the
    /// line start, a grid cut or a restart, never from the jump. `near` as for
    /// [`Self::restart_start`].
    pub fn jump_place(self, near: Around, p: u64) -> u64 {
        let q = self.align_down(p);
        if matches!(self, Encoding::Utf16Le | Encoding::Utf16Be) && self.low_after_high(&near, q) {
            q - 2
        } else {
            q
        }
    }

    /// §7.3 rule 4, a place inside a line not wrapped — the byte `left` every row is shown from
    /// (Right/Left by `SIDEWAYS`, End's `end_left`, the sideways wheel): `left` rounded down to
    /// the unit, then moved to the first character start at or after it — UTF-8 skips up to 3
    /// continuation bytes, as before VM-2 (`Wrapper::start_at`); UTF-16 moves past a low
    /// surrogate whose preceding unit is a high surrogate. `near` as for
    /// [`Self::restart_start`]. `None`: CP949 with no proof before `left` (§7.4), placeholders.
    pub fn left_start(self, near: Around, left: u64) -> Option<u64> {
        if self == Encoding::Cp949 {
            return cp949::left_start(near, left);
        }
        Some(self.forward(near, self.align_down(left)))
    }

    /// Rules 2 and 4 from the aligned place `q`, in every encoding but CP949.
    fn forward(self, near: Around, q: u64) -> u64 {
        if near.anchored && q <= near.at {
            return near.at;
        }
        match self {
            Encoding::Utf8 => {
                let skip = near.after(q, 3).iter().take_while(|&&b| b & 0xC0 == 0x80);
                q + skip.count() as u64
            }
            Encoding::Utf16Le | Encoding::Utf16Be if self.low_after_high(&near, q) => q + 2,
            _ => q,
        }
    }

    /// UTF-16: whether the unit at the aligned `q` is the low half of a valid pair — a low
    /// surrogate whose preceding unit is a high one, the only look-back a pair needs (D38).
    fn low_after_high(self, near: &Around, q: u64) -> bool {
        let be = self.big_endian();
        // The unit at `q` first: a place at or past the bytes read has none, and no pair.
        let at = near.after(q, 2);
        if at.len() < 2 || !is_low(u16_of(at, be)) {
            return false;
        }
        let before = near.before(q, 2);
        before.len() == 2 && is_high(u16_of(before, be))
    }

    /// Whether a character start here needs a proof from before the place beyond
    /// [`Self::look_back`] — a line start within REACH or a phase checkpoint (D28): CP949 only.
    pub fn needs_phase(self) -> bool {
        self == Encoding::Cp949
    }

    /// The phase slot (D22, D28): whether a character begun in `bytes` — which start at a
    /// character start — is still pending at their end, the one bit a phase checkpoint keeps per
    /// GRID point of a line longer than REACH (§7.4). A checkpoint at G that says `true` proves a
    /// character start at G − 1, `false` one at G: pass it to the rules as [`Around::anchored`],
    /// and the next checkpoint is the phase of the bytes from there. `None` for every encoding
    /// but CP949 — a no-op, their starts are found from [`Self::look_back`] bytes.
    pub fn phase(self, bytes: &[u8]) -> Option<bool> {
        match self {
            Encoding::Cp949 => cp949::phase(bytes),
            _ => None,
        }
    }
}

/// A one-byte encoding: every byte one character, by `ch`.
fn bytewise(bytes: &[u8], ch: impl Fn(u8) -> char) -> (Vec<Cell>, usize) {
    let cells = bytes.iter().map(|&b| Cell { ch: ch(b), len: 1 });
    (cells.collect(), bytes.len())
}

/// Fixed units of `n` bytes (UCS-2, UTF-32), each a character by `ch` or one U+FFFD; a partial
/// last unit is one U+FFFD when `whole`, else left.
fn decode_fixed(
    bytes: &[u8],
    whole: bool,
    n: usize,
    ch: impl Fn(&[u8]) -> Option<char>,
) -> (Vec<Cell>, usize) {
    let units = bytes.len() / n * n;
    let mut out: Vec<Cell> = bytes
        .chunks_exact(n)
        .map(|u| Cell {
            ch: ch(u).unwrap_or(char::REPLACEMENT_CHARACTER),
            len: n as u8,
        })
        .collect();
    if !whole {
        return (out, units);
    }
    if units < bytes.len() {
        out.push(Cell {
            ch: char::REPLACEMENT_CHARACTER,
            len: (bytes.len() - units) as u8,
        });
    }
    (out, bytes.len())
}

/// UTF-16: a unit, or a high surrogate with the low one after it; an unpaired surrogate is one
/// U+FFFD per unit, and the unit after an unpaired high one is decoded on its own. Not `whole`,
/// a high surrogate as the last unit is left for the next piece, with a partial unit.
fn decode_utf16(bytes: &[u8], whole: bool, be: bool) -> (Vec<Cell>, usize) {
    let unit = |i: usize| u16_of(&bytes[i..], be);
    let units = bytes.len() / 2 * 2;
    let end = if whole {
        bytes.len()
    } else if units >= 2 && is_high(unit(units - 2)) {
        units - 2
    } else {
        units
    };
    let mut out = Vec::with_capacity(end / 2 + 1);
    let mut i = 0;
    while i < end {
        if end - i < 2 {
            out.push(Cell {
                ch: char::REPLACEMENT_CHARACTER,
                len: 1,
            });
            break;
        }
        let u = unit(i);
        let cell = if is_high(u) && i + 4 <= end && is_low(unit(i + 2)) {
            let c = 0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(unit(i + 2)) - 0xDC00);
            Cell {
                ch: char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER),
                len: 4,
            }
        } else {
            Cell {
                // A lone surrogate is no character.
                ch: char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER),
                len: 2,
            }
        };
        out.push(cell);
        i += usize::from(cell.len);
    }
    (out, end)
}

/// UTF-8, as `from_utf8_lossy` does (each maximal bad sequence one U+FFFD) — the text view's
/// `wrap::decode` before VM-2, whose `whole` it shares.
fn decode_utf8(bytes: &[u8], whole: bool) -> (Vec<Cell>, usize) {
    let end = if whole {
        bytes.len()
    } else {
        cut_char_start(bytes)
    };
    let mut out = Vec::with_capacity(end);
    let mut rest = &bytes[..end];
    while !rest.is_empty() {
        let (valid, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => {
                let ok = e.valid_up_to();
                // Valid up to there, by the error.
                let s = std::str::from_utf8(&rest[..ok]).unwrap_or_default();
                (s, e.error_len().unwrap_or(rest.len() - ok))
            }
        };
        out.extend(valid.chars().map(|ch| Cell {
            ch,
            len: ch.len_utf8() as u8,
        }));
        rest = &rest[valid.len()..];
        if bad > 0 {
            out.push(Cell {
                ch: char::REPLACEMENT_CHARACTER,
                len: bad as u8,
            });
            rest = &rest[bad..];
        }
    }
    (out, end)
}

/// Where a UTF-8 character cut in two at the end of `bytes` starts (`bytes.len()` when none is),
/// found from the end alone, whatever comes before: the last up-to-4 bytes from their last
/// non-continuation byte on must be the start of a real character (`from_utf8` checks the lead
/// byte, overlongs, surrogates and the U+10FFFF bound) that only lacks its end. Bytes that can
/// never start a character are not a cut character: they show as U+FFFD. The text view's
/// `cut_char_start` before VM-2.
fn cut_char_start(bytes: &[u8]) -> usize {
    let from = bytes.len().saturating_sub(4);
    let Some(i) = (from..bytes.len()).rev().find(|&i| bytes[i] & 0xC0 != 0x80) else {
        return bytes.len();
    };
    match std::str::from_utf8(&bytes[i..]) {
        Err(e) if e.valid_up_to() == 0 && e.error_len().is_none() => i,
        _ => bytes.len(),
    }
}

/// The 2-byte unit at the start of `b`.
fn u16_of(b: &[u8], be: bool) -> u16 {
    let pair = [b[0], b[1]];
    if be {
        u16::from_be_bytes(pair)
    } else {
        u16::from_le_bytes(pair)
    }
}

/// The 4-byte unit at the start of `b`.
fn u32_of(b: &[u8], be: bool) -> u32 {
    let quad = [b[0], b[1], b[2], b[3]];
    if be {
        u32::from_be_bytes(quad)
    } else {
        u32::from_le_bytes(quad)
    }
}

fn is_high(u: u16) -> bool {
    (0xD800..=0xDBFF).contains(&u)
}

fn is_low(u: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&u)
}

/// CP949 / EUC-KR (§7.4, VM-4): its decoder, sync bytes, rules and phase, and step 3 of [`pick`]
/// ([`cp949::guess`]), on encoding_rs's `EUC_KR`.
mod cp949;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cp949_tests;
