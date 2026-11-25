//! The page server (09 §3.1, X-W1's result): one page view's `sopen-page` requests
//! ([`crate::scheme`]), answered from the page's root over one connection by one worker thread.
//!
//! - **A new request** runs §3.1's steps on the connection as a `RemoteFs`: percent-decode, `..`
//!   and NUL refused; `realpath` (a link counts by what it points at) inside the root, else 403
//!   counted on the row ([`Control::refused`]); `stat` — a folder's `index.html` checked the same
//!   way again; then `into_ranged`, whose `fstat` is the request's fingerprint.
//! - **A path checked once is not checked again** (change 1): a later range on it is served from
//!   the open handle, or from a `reopen` (2 RTT) whose fingerprint must match; a whole file asked
//!   again (a reload) is always reopened. A file whose size or mtime moved fails a request already
//!   under way — never padded — and is checked afresh for a new one; so is one that will not open
//!   or whose read fails, its kept blocks forgotten. A range inside one aligned
//!   [`CACHE_BLOCK`] reads the whole block, and later ranges inside it come from memory.
//! - **Turns** (§3.1, change 2): requests take turns of one piece each (≤ [`PIECE`], pipelined
//!   reads of 64 KiB, at most what the engine's window lets the request send), a new request's
//!   first turn being its steps and its first piece; while another request waits a piece is what
//!   the link carries in [`LINK_TIME`] (change 5).
//! - **Range** (RFC 9110, one range) → 206, past the end → 416, anything else → 200. **Type** by
//!   extension; a text type's charset from the file's first 8 KiB as the text view picks it —
//!   none when the text names its own by its type's rule (HTML's prescan) — so its head waits for
//!   that read.
//! - **Cancel**: a stopped request ends after the read in flight or the step in progress; what
//!   was asked of the handle is drained, never left for the next request.
//! - **Budget**: a preview reads at most [`PREVIEW_BUDGET`]; a request that would read past it
//!   fails before its head; [`Control::lift`] (the hand-over) removes it.
//!
//! A local page goes the same way, read in place ([`serve_local`]).

use super::{Throttle, Throttled};
use crate::encoding::Encoding;
use crate::kind;
use crate::remote::{self, Asked, Completion, LocalFs, Ranged, RemoteFs, Stat, Ticket, Wake};
use crate::scheme::{self, Body, Head, Reply, Request, RequestId, WINDOW};
use anyhow::{anyhow, bail, Context};
use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// A turn's piece at most (§3.1: ≤ 1 MiB): the scheme's window, so a piece planned within the
/// request's credit never waits for more mid-way.
pub const PIECE: u64 = WINDOW as u64;
/// One read on the handle: 06's block, what X-W1's prototype read at a time.
const READ: u64 = super::BLOCK;
/// A request takes a turn once it may send this much, or its rest: a pull engine gives credit
/// back 8 KiB a read, and one piece per read would cost a round trip each.
const TURN_MIN: u64 = 256 * 1024;
/// A small request reads its aligned block of this size; later ranges inside a kept block are
/// answered from memory (X-W1 change 1).
pub const CACHE_BLOCK: u64 = 256 * 1024;
/// The blocks a view keeps at most, the least recently used going first **[decision]**.
const CACHE_BYTES: u64 = 16 << 20;
/// The preview's budget (§3.1): 02 §4.3's bound for an image read whole, counted as bytes read.
pub const PREVIEW_BUDGET: u64 = 32 << 20;
/// While another request waits, a piece is what the link carries in about this long (change 5).
pub const LINK_TIME: Duration = Duration::from_millis(100);

/// The server's own request log: one line per answer and per stop.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;
/// Where the connection goes when the server ends.
pub type GiveBack = Box<dyn FnOnce(Box<dyn RemoteFs + Send>) + Send>;

/// How a view's server runs.
pub struct Options {
    /// Bytes the page may read in all (§3.1's preview budget); `None`: no budget (the viewer).
    pub budget: Option<u64>,
    /// `SOPEN_STREAM_THROTTLE`'s pacing on every file the server opens (06 §11).
    pub throttle: Option<Throttle>,
    pub log: Option<Log>,
    /// Gets the connection when the server ends — the preview's session given back (06 §13
    /// item 3); without it the connection is closed.
    pub give_back: Option<GiveBack>,
}

impl Options {
    /// A preview's page: the budget, the throttle hook.
    pub fn preview() -> Self {
        Self {
            budget: Some(PREVIEW_BUDGET),
            ..Self::viewer()
        }
    }

    /// The viewer's page: no budget, the throttle hook.
    pub fn viewer() -> Self {
        Self {
            budget: None,
            throttle: std::env::var("SOPEN_STREAM_THROTTLE")
                .ok()
                .and_then(|v| Throttle::parse(&v)),
            log: None,
            give_back: None,
        }
    }
}

/// The view's side of its server: the row's count and the budget.
#[derive(Clone)]
pub struct Control(Arc<Ledger>);

struct Ledger {
    refused: AtomicU64,
    budget: Mutex<Budget>,
}

struct Budget {
    limit: Option<u64>,
    used: u64,
    /// A request was failed for the budget.
    cut: bool,
}

impl Ledger {
    fn budget(&self) -> std::sync::MutexGuard<'_, Budget> {
        self.budget.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserves `n` bytes to read; `false` (and cut) when they would pass the budget.
    fn take(&self, n: u64) -> bool {
        let mut b = self.budget();
        if b.limit
            .is_some_and(|limit| b.used.saturating_add(n) > limit)
        {
            b.cut = true;
            return false;
        }
        b.used = b.used.saturating_add(n);
        true
    }

    /// Bytes reserved and never read: a request that ended early.
    fn give(&self, n: u64) {
        let mut b = self.budget();
        b.used = b.used.saturating_sub(n);
    }
}

impl Control {
    /// Requests refused for reaching outside the root (403): the row's count (§3.1 step 2).
    pub fn refused(&self) -> u64 {
        self.0.refused.load(Ordering::SeqCst)
    }

    /// The budget failed a request.
    pub fn cut(&self) -> bool {
        self.0.budget().cut
    }

    /// The hand-over: no budget from now on. `true` when the budget failed anything — the view
    /// reloads once, so the viewer shows the whole page (§3.1).
    pub fn lift(&self) -> bool {
        let mut b = self.0.budget();
        b.limit = None;
        b.cut
    }
}

/// A view's page server ([`scheme::Server`]): each call hands the work to its worker and returns.
/// Dropped (the view closed), the worker ends after the piece or the step in progress and gives
/// its connection back.
pub struct Page {
    tx: Sender<Msg>,
    /// A credit message is on its way: credits between are one wake.
    credited: Arc<AtomicBool>,
}

enum Msg {
    Start(Request, Reply),
    Stop(RequestId),
    Credit,
}

/// Serves the page under `root` (resolved, absolute: [`root`]) over `conn`.
pub fn serve(conn: Box<dyn RemoteFs + Send>, root: &str, options: Options) -> (Page, Control) {
    spawn(conn, root, options, false)
}

/// Serves a page on this machine, read in place (06 §1): `LocalFs` resolves and stats, files are
/// read by positioned reads on their own handle.
pub fn serve_local(root: &str, options: Options) -> (Page, Control) {
    spawn(Box::new(LocalFs::new()), root, options, true)
}

fn spawn(
    conn: Box<dyn RemoteFs + Send>,
    root: &str,
    options: Options,
    in_place: bool,
) -> (Page, Control) {
    let ledger = Arc::new(Ledger {
        refused: AtomicU64::new(0),
        budget: Mutex::new(Budget {
            limit: options.budget,
            used: 0,
            cut: false,
        }),
    });
    let (tx, rx) = mpsc::channel();
    let credited = Arc::new(AtomicBool::new(false));
    let root = match root.trim_end_matches('/') {
        "" => "/".to_string(),
        r => r.to_string(),
    };
    let worker = Worker {
        rx,
        credited: credited.clone(),
        link: Link::Conn(conn),
        in_place,
        root,
        checked: HashMap::new(),
        charsets: HashMap::new(),
        blocks: Blocks::default(),
        queue: VecDeque::new(),
        ledger: ledger.clone(),
        throttle: options.throttle,
        log: options.log,
        give_back: options.give_back,
        rate: None,
    };
    // Should the thread not start, the worker drops with its receiver: every request fails as
    // its reply drops.
    let _ = std::thread::Builder::new()
        .name("sopen-page".into())
        .spawn(move || worker.run());
    (Page { tx, credited }, Control(ledger))
}

impl scheme::Server for Page {
    fn start(&mut self, request: Request, reply: Reply) {
        // A worker gone: the reply comes back in the error and fails as it drops.
        let _ = self.tx.send(Msg::Start(request, reply));
    }

    fn stop(&mut self, id: RequestId) {
        let _ = self.tx.send(Msg::Stop(id));
    }

    fn credit(&mut self, _id: RequestId) {
        if !self.credited.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Msg::Credit);
        }
    }
}

/// A page's root and its path under it (§3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub root: String,
    /// The file's path relative to the root: the page's URL path.
    pub path: String,
}

/// §3.1's root: `opened_on` — the folder the browser was opened on, when it is on the file's host
/// — if the file's resolved path is inside it, else the file's own folder; both resolved by
/// `realpath` on `conn`. A folder that no longer resolves counts as not given.
pub fn root(conn: &mut dyn RemoteFs, opened_on: Option<&str>, file: &str) -> anyhow::Result<Root> {
    let file = conn.realpath(file)?;
    let opened = opened_on.and_then(|f| conn.realpath(f).ok());
    let root = match opened {
        Some(o) if under(&o, &file).is_some_and(|rest| !rest.is_empty()) => o,
        _ => parent(&file),
    };
    let path = under(&root, &file).unwrap_or_default().to_string();
    Ok(Root { root, path })
}

/// `path` without `root/` when it is below `root`.
fn under<'p>(root: &str, path: &'p str) -> Option<&'p str> {
    match root {
        "/" => path.strip_prefix('/'),
        _ => path.strip_prefix(root)?.strip_prefix('/'),
    }
}

/// `path` is the root or below it.
fn inside(root: &str, path: &str) -> bool {
    path == root || under(root, path).is_some()
}

fn parent(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => path[..i].to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// The type and charset tables.

/// §3.1's type table: the extension, the type, and whether it is text (its head names the
/// charset the file's first 8 KiB give). `jpg` and `mjs` are `jpeg`'s and `js`'s own types.
const TYPES: &[(&str, &str, bool)] = &[
    ("html", "text/html", true),
    ("htm", "text/html", true),
    ("xml", "application/xml", true),
    ("xsl", "text/xsl", true),
    ("xslt", "text/xsl", true),
    ("css", "text/css", true),
    ("js", "text/javascript", true),
    ("mjs", "text/javascript", true),
    ("json", "application/json", true),
    ("svg", "image/svg+xml", true),
    ("png", "image/png", false),
    ("jpg", "image/jpeg", false),
    ("jpeg", "image/jpeg", false),
    ("gif", "image/gif", false),
    ("webp", "image/webp", false),
    ("woff", "font/woff", false),
    ("woff2", "font/woff2", false),
    ("ttf", "font/ttf", false),
    ("otf", "font/otf", false),
    ("mp4", "video/mp4", false),
    ("webm", "video/webm", false),
    ("mp3", "audio/mpeg", false),
    ("ogg", "audio/ogg", false),
    ("wav", "audio/wav", false),
    ("pdf", "application/pdf", false),
];

/// Anything the table does not name.
const OTHER: &str = "application/octet-stream";

/// The type of a file named `name`, and whether it is text.
fn type_of(name: &str) -> (&'static str, bool) {
    let ext = kind::extension(name);
    TYPES
        .iter()
        .find(|(e, _, _)| Some(*e) == ext.as_deref())
        .map_or((OTHER, false), |&(_, mime, text)| (mime, text))
}

/// The type a file named `name` is served as (§3.1's table, by its extension in any case).
pub fn mime(name: &str) -> &'static str {
    type_of(name).0
}

/// The charset a text type `mime`'s head names for a file whose first bytes are `sample` (its
/// first [`kind::SNIFF_BYTES`]; more is not looked at), `truncated` when the file goes on past
/// them: the text view's default (08 §7.2, `crate::encoding::pick`), so the page and its source
/// agree — `utf-8` by its BOM or when the sample is UTF-8 (`kind::utf8_text`: a character cut at
/// the edge accepted, never `looks_like_text`), `utf-16le`/`be` and `utf-32le`/`be` by BOM or the
/// NUL pattern, anything else none. None too when the text names its own, by its type's own rule
/// ([`names_charset`]): a header's charset would override it.
pub fn charset(mime: &str, sample: &[u8], truncated: bool) -> Option<&'static str> {
    let truncated = truncated || sample.len() > kind::SNIFF_BYTES;
    let sample = &sample[..sample.len().min(kind::SNIFF_BYTES)];
    if names_charset(mime, sample) {
        return None;
    }
    match crate::encoding::pick(sample, truncated).encoding {
        Encoding::Utf8 if sample.starts_with(Encoding::Utf8.bom()) => Some("utf-8"),
        Encoding::Utf8 => kind::utf8_text(sample, truncated).then_some("utf-8"),
        Encoding::Utf16Le => Some("utf-16le"),
        Encoding::Utf16Be => Some("utf-16be"),
        Encoding::Utf32Le => Some("utf-32le"),
        Encoding::Utf32Be => Some("utf-32be"),
        _ => None,
    }
}

/// The bytes HTML's prescan reads (WHATWG HTML, "prescan a byte stream to determine its
/// encoding"), and those CSS's `@charset` must sit in (CSS Syntax, "determine the fallback
/// encoding").
const PRESCAN_BYTES: usize = 1024;

/// A text that names its own encoding, by the rule its engine reads it by — only a label of an
/// encoding of the Encoding Standard ([`label`]) counts: HTML by the prescan of its first 1024 bytes
/// ([`prescan`]); XML (`application/xml`, XSLT, SVG) by its declaration's `encoding`, only at the
/// very start of the file, after a UTF-8 BOM if any; CSS by `@charset "LABEL";` as the file's
/// first bytes. A script or JSON names none.
fn names_charset(mime: &str, sample: &[u8]) -> bool {
    match mime {
        "text/html" => prescan(&sample[..sample.len().min(PRESCAN_BYTES)]).is_some(),
        "application/xml" | "text/xsl" | "image/svg+xml" => {
            let text = sample.strip_prefix(Encoding::Utf8.bom()).unwrap_or(sample);
            xml_encoding(text).is_some()
        }
        "text/css" => {
            let text = &sample[..sample.len().min(PRESCAN_BYTES)];
            let Some(rest) = text.strip_prefix(b"@charset \"") else {
                return false;
            };
            let end = rest.iter().position(|&b| b == b'"');
            end.is_some_and(|e| rest.get(e + 1) == Some(&b';') && label(&rest[..e]).is_some())
        }
        _ => false,
    }
}

/// HTML whitespace (tab, LF, FF, CR, space).
fn space(b: u8) -> bool {
    matches!(b, 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// WHATWG HTML's prescan over `b`: the encoding the first `<meta>` naming one names (as the
/// prescan returns it), skipping
/// comments, other tags (their attributes read, so a `>` inside a quoted value ends nothing) and
/// `<!`, `</`, `<?` constructs; `None` when none does before the bytes end — a tag cut at their
/// end names nothing.
fn prescan(b: &[u8]) -> Option<Enc> {
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            // To the `>` of the first `-->`; the dashes of `<!--` count.
            i += 2 + find(&rest[2..], b"-->")? + 2;
        } else if rest.len() > 5
            && rest[..5].eq_ignore_ascii_case(b"<meta")
            && (space(rest[5]) || rest[5] == b'/')
        {
            i += 5;
            if let Some(found) = meta(b, &mut i)? {
                // As the prescan returns it: a UTF-16 label is UTF-8 (the bytes it was read in are
                // ASCII), x-user-defined is windows-1252; anything else, `replacement` included.
                return Some(match found {
                    f if f == encoding_rs::UTF_16LE || f == encoding_rs::UTF_16BE => {
                        encoding_rs::UTF_8
                    }
                    f if f == encoding_rs::X_USER_DEFINED => encoding_rs::WINDOWS_1252,
                    f => f,
                });
            }
        } else if rest.len() > 1
            && rest[0] == b'<'
            && (rest[1].is_ascii_alphabetic()
                || (rest[1] == b'/' && rest.get(2).is_some_and(u8::is_ascii_alphabetic)))
        {
            i += b[i..].iter().position(|&c| space(c) || c == b'>')?;
            while let Attribute::Got(..) = attribute(b, &mut i) {}
            // The attributes ran to the end of the bytes: the tag is cut.
            b.get(i)?;
        } else if rest.starts_with(b"<!") || rest.starts_with(b"</") || rest.starts_with(b"<?") {
            i += rest.iter().position(|&c| c == b'>')?;
        }
        i += 1;
    }
    None
}

/// A `<meta>`'s attributes from `i` (after `<meta`): the encoding it names (`charset`, or
/// `http-equiv=content-type` with a `content` naming one), `Some(None)` for none, `None` when the
/// bytes end inside it.
fn meta(b: &[u8], i: &mut usize) -> Option<Option<Enc>> {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let mut pragma = false;
    // `None`: no charset yet; `Some(true)`: from `content`, which needs the pragma.
    let mut need_pragma = None;
    let mut found: Option<Option<Enc>> = None;
    loop {
        match attribute(b, i) {
            Attribute::Out => return None,
            Attribute::End => break,
            Attribute::Got(name, value) => {
                if seen.contains(&name) {
                    continue;
                }
                match name.as_slice() {
                    b"http-equiv" => pragma |= value == b"content-type",
                    b"content" if found.is_none() => {
                        if let Some(e) = from_content(&value) {
                            found = Some(Some(e));
                            need_pragma = Some(true);
                        }
                    }
                    b"charset" => {
                        found = Some(label(&value));
                        need_pragma = Some(false);
                    }
                    _ => {}
                }
                seen.push(name);
            }
        }
    }
    Some(match need_pragma {
        None => None,
        Some(true) if !pragma => None,
        _ => found.flatten(),
    })
}

/// What WHATWG's "get an attribute" found at `i`.
enum Attribute {
    Got(Vec<u8>, Vec<u8>),
    /// `>`: there is none.
    End,
    /// The bytes ended.
    Out,
}

/// WHATWG HTML's "get an attribute": its name and value, lowercased; `i` left after it.
fn attribute(b: &[u8], i: &mut usize) -> Attribute {
    let at = |i: usize| b.get(i).copied();
    while at(*i).is_some_and(|c| space(c) || c == b'/') {
        *i += 1;
    }
    match at(*i) {
        None => return Attribute::Out,
        Some(b'>') => return Attribute::End,
        Some(_) => {}
    }
    let (mut name, mut value) = (Vec::new(), Vec::new());
    loop {
        let Some(c) = at(*i) else {
            return Attribute::Out;
        };
        if c == b'=' && !name.is_empty() {
            *i += 1;
            break;
        }
        if space(c) {
            while at(*i).is_some_and(space) {
                *i += 1;
            }
            match at(*i) {
                None => return Attribute::Out,
                Some(b'=') => {
                    *i += 1;
                    break;
                }
                Some(_) => return Attribute::Got(name, value),
            }
        }
        if c == b'/' || c == b'>' {
            return Attribute::Got(name, value);
        }
        name.push(c.to_ascii_lowercase());
        *i += 1;
    }
    while at(*i).is_some_and(space) {
        *i += 1;
    }
    match at(*i) {
        None => return Attribute::Out,
        Some(q @ (b'"' | b'\'')) => {
            *i += 1;
            loop {
                let Some(c) = at(*i) else {
                    return Attribute::Out;
                };
                *i += 1;
                if c == q {
                    return Attribute::Got(name, value);
                }
                value.push(c.to_ascii_lowercase());
            }
        }
        Some(b'>') => return Attribute::Got(name, value),
        Some(c) => {
            value.push(c.to_ascii_lowercase());
            *i += 1;
        }
    }
    loop {
        match at(*i) {
            None => return Attribute::Out,
            Some(c) if space(c) || c == b'>' => return Attribute::Got(name, value),
            Some(c) => {
                value.push(c.to_ascii_lowercase());
                *i += 1;
            }
        }
    }
}

/// WHATWG HTML's "extracting a character encoding from a meta element" (a `content` value).
fn from_content(s: &[u8]) -> Option<Enc> {
    let mut pos = 0;
    loop {
        let mut j = pos + find_ignoring_case(&s[pos..], b"charset")? + 7;
        while s.get(j).copied().is_some_and(space) {
            j += 1;
        }
        if s.get(j) != Some(&b'=') {
            pos = j;
            continue;
        }
        j += 1;
        while s.get(j).copied().is_some_and(space) {
            j += 1;
        }
        let c = *s.get(j)?;
        if c == b'"' || c == b'\'' {
            let rest = &s[j + 1..];
            let end = rest.iter().position(|&d| d == c)?;
            return label(&rest[..end]);
        }
        let end = s[j..]
            .iter()
            .position(|&d| space(d) || d == b';')
            .map_or(s.len(), |e| j + e);
        return label(&s[j..end]);
    }
}

/// The XML declaration's `encoding` when `text` starts with one (`<?xml` and whitespace).
fn xml_encoding(text: &[u8]) -> Option<Enc> {
    let rest = text.strip_prefix(b"<?xml")?;
    if !rest.first().copied().is_some_and(space) {
        return None;
    }
    let decl = &rest[..find(rest, b"?>")?];
    let mut pos = 0;
    while let Some(k) = find(&decl[pos..], b"encoding") {
        let at = pos + k;
        pos = at + 8;
        if !space(decl[at - 1]) {
            continue;
        }
        let mut j = pos;
        while decl.get(j).copied().is_some_and(space) {
            j += 1;
        }
        if decl.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while decl.get(j).copied().is_some_and(space) {
            j += 1;
        }
        let q = *decl.get(j)?;
        if q != b'"' && q != b'\'' {
            return None;
        }
        let name = &decl[j + 1..];
        return label(&name[..name.iter().position(|&c| c == q)?]);
    }
    None
}

fn find_ignoring_case(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

/// The Encoding Standard's "get an encoding" (`encoding_rs::Encoding::for_label`: whitespace
/// around the label and ASCII case ignored); `None` for a label of no encoding.
fn label(label: &[u8]) -> Option<Enc> {
    encoding_rs::Encoding::for_label(label)
}

/// An encoding of the Encoding Standard.
type Enc = &'static encoding_rs::Encoding;

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------------------------
// Range.

/// What a `Range` header asks of a `size`-byte file (RFC 9110 §14.1.2, §14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Span {
    /// No range, or one the server ignores — several ranges, another unit, a malformed one, or a
    /// suffix of an empty file: 200 and the whole file.
    Whole,
    /// Bytes `first..=last`: 206.
    Part(u64, u64),
    /// 416: the range starts at or past the end, or asks a suffix of nothing (`-0`).
    Unsatisfiable,
}

fn span(range: Option<&str>, size: u64) -> Span {
    // The field's own leading and trailing whitespace is not its value; inside the grammar
    // (`bytes=a-b`, `1#range-spec`) whitespace is only around a list's commas.
    let Some((unit, set)) = range.and_then(|r| r.trim().split_once('=')) else {
        return Span::Whole;
    };
    if !unit.eq_ignore_ascii_case("bytes") {
        return Span::Whole;
    }
    // A list's empty elements are nothing (RFC 9110 §5.6.1).
    let parts: Vec<&str> = set.split(',').collect();
    let last = parts.len() - 1;
    let mut specs = parts.iter().enumerate().filter_map(|(i, part)| {
        let part = if i > 0 { part.trim_start() } else { part };
        let part = if i < last { part.trim_end() } else { part };
        (!part.is_empty()).then_some(part)
    });
    let (Some(spec), None) = (specs.next(), specs.next()) else {
        return Span::Whole;
    };
    let Some((a, b)) = spec.split_once('-') else {
        return Span::Whole;
    };
    let (digits_a, digits_b) = (digits(a), digits(b));
    match (a.is_empty(), digits_a, b.is_empty(), digits_b) {
        // `-N`: the last N bytes.
        (true, _, false, Some(n)) => match (n.is_zero(), size) {
            (true, _) => Span::Unsatisfiable,
            (_, 0) => Span::Whole,
            _ => Span::Part(size.saturating_sub(n.value()), size - 1),
        },
        // `A-` and `A-B`: the order is the numbers', whatever their size.
        (false, Some(first), true, _) => from(first, None, size),
        (false, Some(first), false, Some(last)) if last >= first => from(first, Some(last), size),
        _ => Span::Whole,
    }
}

/// `A-` or `A-B` (B ≥ A) of a `size`-byte file.
fn from(first: Digits, last: Option<Digits>, size: u64) -> Span {
    let first = first.value();
    if first >= size {
        return Span::Unsatisfiable;
    }
    let last = last.map_or(u64::MAX, Digits::value);
    Span::Part(first, last.min(size - 1))
}

/// A run of decimal digits, compared as numbers of any size (leading zeros dropped).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Digits<'a>(&'a str);

fn digits(s: &str) -> Option<Digits<'_>> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| {
        let trimmed = s.trim_start_matches('0');
        Digits(if trimmed.is_empty() { "0" } else { trimmed })
    })
}

impl Digits<'_> {
    fn is_zero(self) -> bool {
        self.0 == "0"
    }

    /// The number, `u64::MAX` past it: a first byte past any file is still past its end.
    fn value(self) -> u64 {
        self.0.parse().unwrap_or(u64::MAX)
    }
}

impl PartialOrd for Digits<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Digits<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.0.len(), self.0).cmp(&(other.0.len(), other.0))
    }
}

/// A piece while another request waits: what the link carries in [`LINK_TIME`] at `rate`
/// bytes/s, in whole reads, at least one (change 5); else, or with no rate measured yet, the
/// whole [`PIECE`].
fn piece_for(rate: Option<f64>, others: bool) -> u64 {
    match rate {
        Some(rate) if others => {
            let fits = (rate * LINK_TIME.as_secs_f64()) as u64;
            (fits / READ * READ).clamp(READ, PIECE)
        }
        _ => PIECE,
    }
}

// ---------------------------------------------------------------------------------------------
// The worker.

/// The connection, as a `RemoteFs` between files or as one file's handle.
enum Link {
    Conn(Box<dyn RemoteFs + Send>),
    File(Box<dyn Ranged>),
    /// The transport gave no reply: every request fails with this.
    Lost(String),
}

/// A path checked in this view (change 1): what it resolved to, the name its type is by, and
/// the fingerprint its handle's `fstat` gave.
#[derive(Clone)]
struct Checked {
    resolved: String,
    name: String,
    size: u64,
    mtime: SystemTime,
    /// Asked as a folder: `resolved` is its `index.html`.
    folder: bool,
}

/// A file as one fingerprint saw it: what its kept blocks and charset belong to.
type Version = (String, u64, SystemTime);

impl Checked {
    fn matches(&self, stat: &Stat) -> bool {
        (stat.size, stat.mtime) == (self.size, self.mtime)
    }

    fn version(&self) -> Version {
        (self.resolved.clone(), self.size, self.mtime)
    }
}

/// One request the worker has.
struct Req {
    id: RequestId,
    /// As the engine asked it (the log's).
    path: String,
    range: Option<String>,
    stage: Stage,
    /// Bytes reserved from the budget and not read yet: given back when it ends.
    owed: u64,
    ledger: Arc<Ledger>,
}

impl Drop for Req {
    fn drop(&mut self) {
        self.ledger.give(self.owed);
    }
}

enum Stage {
    /// Its first turn has not come.
    New(Reply),
    Open(Open),
    Spent,
}

/// A response going out in pieces.
struct Open {
    out: Out,
    file: Checked,
    /// The next byte to read, and the end of the response's bytes.
    at: u64,
    end: u64,
    code: u16,
}

impl Req {
    fn ready(&self) -> bool {
        match &self.stage {
            Stage::Open(o) => o.out.stopped() || o.out.room() >= TURN_MIN.min(o.end - o.at),
            Stage::New(_) | Stage::Spent => true,
        }
    }
}

/// Where a response's bytes go.
enum Out {
    /// The head waits for the file's first bytes, a text type's charset (§3.1): the body's
    /// first bytes, held until they reach the sample's length.
    Waiting {
        reply: Reply,
        plan: Plan,
        held: Vec<u8>,
    },
    Body(Body),
    Spent,
}

/// The head a response will have, but for its charset.
struct Plan {
    mime: &'static str,
    first: u64,
    last: u64,
    size: u64,
    part: bool,
}

impl Plan {
    fn head(&self, charset: Option<&str>) -> Option<Head> {
        let mime = match charset {
            Some(c) => format!("{}; charset={c}", self.mime),
            None => self.mime.to_string(),
        };
        match self.part {
            true => Head::part(mime, self.first, self.last, self.size),
            false => Some(Head::whole(mime, self.size)),
        }
    }
}

/// Why bytes stopped going out.
enum Halt {
    /// The engine stopped the request: nothing more goes.
    Stopped,
    Failed(String),
}

impl Out {
    fn stopped(&self) -> bool {
        match self {
            Out::Waiting { reply, .. } => reply.stopped(),
            Out::Body(body) => body.stopped(),
            Out::Spent => true,
        }
    }

    fn room(&self) -> u64 {
        match self {
            Out::Waiting { .. } => WINDOW as u64,
            Out::Body(body) => body.room() as u64,
            Out::Spent => 0,
        }
    }

    /// A waiting head goes, with `charset`, and what was held after it.
    fn resolve(&mut self, charset: Option<&'static str>) -> Result<(), Halt> {
        let Out::Waiting { reply, plan, held } = std::mem::replace(self, Out::Spent) else {
            return Ok(());
        };
        let Some(head) = plan.head(charset) else {
            reply.fail("the page server made a range it cannot answer");
            return Err(Halt::Failed("a range it cannot answer".into()));
        };
        let mut body = reply.head(head);
        let sent = match held.is_empty() {
            true => Ok(()),
            false => send(&mut body, held),
        };
        *self = Out::Body(body);
        sent
    }

    /// The body's next bytes, in order. A waiting head goes once `sample` bytes are held: the
    /// charset found then is returned.
    fn push(
        &mut self,
        bytes: &[u8],
        sample: usize,
        truncated: bool,
    ) -> Result<Option<Option<&'static str>>, Halt> {
        match self {
            Out::Body(body) => send(body, bytes.to_vec()).map(|()| None),
            Out::Waiting { held, plan, .. } => {
                held.extend_from_slice(bytes);
                if held.len() < sample {
                    return Ok(None);
                }
                let found = charset(plan.mime, &held[..sample], truncated);
                self.resolve(found).map(|()| Some(found))
            }
            Out::Spent => Err(Halt::Stopped),
        }
    }

    fn fail(self, why: &str) {
        match self {
            Out::Waiting { reply, .. } => reply.fail(why),
            Out::Body(body) => body.fail(why),
            Out::Spent => {}
        }
    }
}

fn send(body: &mut Body, bytes: Vec<u8>) -> Result<(), Halt> {
    match body.data(bytes) {
        Ok(()) => Ok(()),
        Err(scheme::Refused::Stopped) => Err(Halt::Stopped),
        Err(e) => Err(Halt::Failed(format!("the page server: {e}"))),
    }
}

/// Why the handle is not on a request's file.
enum NotOpen {
    /// Its size or mtime moved since it was checked: it is forgotten.
    Moved(String),
    Failed(String),
}

impl NotOpen {
    fn why(&self) -> &str {
        match self {
            NotOpen::Moved(why) | NotOpen::Failed(why) => why,
        }
    }
}

/// How the steps ended without a file.
enum NoFile {
    /// Outside the root: 403.
    Refused,
    /// A folder asked without its `/`: [`folder_page`].
    Folder,
    /// 404.
    Missing,
    Failed(String),
    Stopped,
}

/// The kept blocks of change 1 — block `k` of a file at one fingerprint — least recently used
/// first.
#[derive(Default)]
struct Blocks {
    kept: HashMap<(Version, u64), Bytes>,
    order: VecDeque<(Version, u64)>,
    bytes: u64,
}

impl Blocks {
    fn get(&mut self, file: &Checked, k: u64) -> Option<Bytes> {
        let key = (file.version(), k);
        let block = self.kept.get(&key)?.clone();
        if let Some(i) = self.order.iter().position(|o| *o == key) {
            self.order.remove(i);
        }
        self.order.push_back(key);
        Some(block)
    }

    fn put(&mut self, file: &Checked, k: u64, block: Bytes) {
        let key = (file.version(), k);
        self.bytes += block.len() as u64;
        if let Some(old) = self.kept.insert(key.clone(), block) {
            self.bytes -= old.len() as u64;
            self.order.retain(|o| *o != key);
        }
        self.order.push_back(key);
        while self.bytes > CACHE_BYTES {
            let Some(gone) = self.order.pop_front() else {
                break;
            };
            if let Some(b) = self.kept.remove(&gone) {
                self.bytes -= b.len() as u64;
            }
        }
    }

    /// Every version's blocks of `path`.
    fn purge(&mut self, path: &str) {
        self.order.retain(|((p, _, _), _)| p != path);
        let kept = std::mem::take(&mut self.kept);
        for (key, b) in kept {
            match key.0 .0 == path {
                true => self.bytes -= b.len() as u64,
                false => {
                    self.kept.insert(key, b);
                }
            }
        }
    }
}

/// One view's worker: its requests, its connection, what it checked and kept.
struct Worker {
    rx: Receiver<Msg>,
    credited: Arc<AtomicBool>,
    link: Link,
    in_place: bool,
    root: String,
    /// Change 1, by the joined path asked.
    checked: HashMap<String, Checked>,
    /// A text file's charset, once found.
    charsets: HashMap<Version, Option<&'static str>>,
    blocks: Blocks,
    queue: VecDeque<Req>,
    ledger: Arc<Ledger>,
    throttle: Option<Throttle>,
    log: Option<Log>,
    give_back: Option<GiveBack>,
    /// The link's bytes/s, as the last pieces came (change 5).
    rate: Option<f64>,
}

impl Worker {
    fn run(mut self) {
        // The request whose turn just ended: it goes back behind what arrived during its turn.
        let mut back = None;
        loop {
            loop {
                match self.rx.try_recv() {
                    Ok(msg) => self.take(msg),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return self.end(),
                }
            }
            if let Some(req) = back.take() {
                self.queue.push_back(req);
            }
            match self.queue.iter().position(Req::ready) {
                Some(i) => {
                    let req = self.queue.remove(i).expect("a ready request");
                    back = self.turn(req);
                }
                None => match self.rx.recv() {
                    Ok(msg) => self.take(msg),
                    Err(_) => return self.end(),
                },
            }
        }
    }

    fn take(&mut self, msg: Msg) {
        match msg {
            Msg::Start(request, reply) => self.queue.push_back(Req {
                id: request.id,
                path: request.path,
                range: request.range,
                stage: Stage::New(reply),
                owed: 0,
                ledger: self.ledger.clone(),
            }),
            Msg::Stop(id) => {
                if let Some(i) = self.queue.iter().position(|r| r.id == id) {
                    let req = self.queue.remove(i).expect("a queued request");
                    self.stopped(&req);
                }
            }
            // Before the queue is looked at: a credit after this sends a new message.
            Msg::Credit => self.credited.store(false, Ordering::SeqCst),
        }
    }

    /// The view closed: the connection goes back.
    fn end(mut self) {
        // The table stopped them all; each that was not drops as a failure.
        self.queue.clear();
        let conn = match std::mem::replace(&mut self.link, Link::Lost(String::new())) {
            Link::Conn(conn) => Some(conn),
            Link::File(file) => Some(file.into_connection()),
            Link::Lost(_) => None,
        };
        if let (Some(conn), Some(back)) = (conn, self.give_back.take()) {
            back(conn);
        }
    }

    fn note(&self, line: impl FnOnce() -> String) {
        if let Some(log) = &self.log {
            log(&line());
        }
    }

    fn answered(&self, req: &Req, code: u16) {
        self.note(|| format!("page {} {} {code}", req.id.get(), req.path));
    }

    fn stopped(&self, req: &Req) {
        self.note(|| format!("page {} {} stopped", req.id.get(), req.path));
    }

    fn failed(&self, req: &Req, out: Out, why: &str) {
        self.note(|| format!("page {} {} failed: {why}", req.id.get(), req.path));
        out.fail(why);
    }

    fn fail_reply(&self, req: &Req, reply: Reply, why: &str) {
        self.note(|| format!("page {} {} failed: {why}", req.id.get(), req.path));
        reply.fail(why);
    }

    /// A folder asked without its `/`: [`folder_page`], so the index is asked at `NAME/`.
    fn to_folder(&self, req: &Req, reply: Reply) {
        let page = folder_page(&req.path);
        self.answered(req, 200);
        let mut body = reply.head(Head::whole("text/html; charset=utf-8", page.len() as u64));
        if send(&mut body, page).is_ok() {
            let _ = body.done();
        }
    }

    /// A head with no body: 403, 404, 416.
    fn empty(&self, req: &Req, reply: Reply, head: Head) {
        if head.status() == scheme::Status::Forbidden {
            self.ledger.refused.fetch_add(1, Ordering::SeqCst);
        }
        self.answered(req, head.code());
        let _ = reply.head(head).done();
    }

    /// One turn of `req`; it back when it has more.
    fn turn(&mut self, mut req: Req) -> Option<Req> {
        match std::mem::replace(&mut req.stage, Stage::Spent) {
            Stage::New(reply) => self.first(req, reply, true),
            Stage::Open(open) => self.piece(req, open),
            Stage::Spent => None,
        }
    }

    /// A new request's first turn: its steps, its head, its first piece (change 2). `recheck`:
    /// a checked file whose fingerprint moved is checked afresh (once) — nothing has been sent.
    fn first(&mut self, mut req: Req, reply: Reply, recheck: bool) -> Option<Req> {
        if reply.stopped() {
            self.stopped(&req);
            return None;
        }
        let Some(joined) = join(&self.root, &req.path) else {
            let head = match decoded(&req.path) {
                Some(_) => Head::forbidden(),
                None => Head::not_found(),
            };
            self.empty(&req, reply, head);
            return None;
        };
        let slash = req.path.ends_with('/');
        let checked = self.checked.get(&joined).cloned();
        let was_checked = checked.is_some();
        let file = match checked {
            Some(file) if file.folder && !slash => {
                self.to_folder(&req, reply);
                return None;
            }
            Some(file) => file,
            None => match self.steps(&joined, slash, &reply) {
                Ok(file) => {
                    // One version of a file per view: another path to it (a folder's index, a
                    // link) checked at another fingerprint is forgotten with what it kept.
                    let other =
                        |c: &Checked| c.resolved == file.resolved && c.version() != file.version();
                    if self.checked.values().any(other) {
                        self.forget(&file.resolved);
                    }
                    self.checked.insert(joined, file.clone());
                    file
                }
                Err(no) => {
                    match no {
                        NoFile::Refused => self.empty(&req, reply, Head::forbidden()),
                        NoFile::Folder => self.to_folder(&req, reply),
                        NoFile::Missing => self.empty(&req, reply, Head::not_found()),
                        NoFile::Failed(why) => self.fail_reply(&req, reply, &why),
                        NoFile::Stopped => self.stopped(&req),
                    }
                    return None;
                }
            },
        };
        // A stop during the last step (the open) ends it here, before any read.
        if reply.stopped() {
            self.stopped(&req);
            return None;
        }
        // A whole file asked again (a reload) is opened again before anything is answered from
        // its check — its size, an empty file's empty answer — its fingerprint compared; runs of
        // ranges keep the handle (change 1).
        if was_checked && span(req.range.as_deref(), file.size) == Span::Whole {
            if let Err(no) = self.open(&file, true) {
                return self.not_open(req, reply, no, recheck);
            }
        }
        let size = file.size;
        let (first, end, part) = match span(req.range.as_deref(), size) {
            Span::Whole => (0, size, false),
            Span::Part(first, last) => (first, last + 1, true),
            Span::Unsatisfiable => {
                self.empty(&req, reply, Head::unsatisfiable(size));
                return None;
            }
        };
        let (mime, text) = type_of(&file.name);
        let sample = size.min(kind::SNIFF_BYTES as u64);
        let truncated = size > sample;
        let code = if part { 206 } else { 200 };
        let plan = Plan {
            mime,
            first,
            last: end.saturating_sub(1),
            size,
            part,
        };
        let mut out = Out::Waiting {
            reply,
            plan,
            held: Vec::new(),
        };
        // The charset when it is known without a read: none for another type; found before;
        // from a kept first block.
        let mut found: Option<Option<&'static str>> = match text {
            false => Some(None),
            true => self.charsets.get(&file.version()).copied(),
        };
        if found.is_none() {
            if let Some(b0) = self.blocks.get(&file, 0) {
                let f = charset(mime, &b0[..sample as usize], truncated);
                self.charsets.insert(file.version(), f);
                found = Some(f);
            }
        }
        // An empty file: its head (an empty sample is UTF-8) and its end.
        if end == first {
            match out.resolve(found.unwrap_or_else(|| charset(mime, &[], false))) {
                Ok(()) => self.close(&req, out, code),
                Err(halt) => {
                    self.halt(&req, out, halt);
                }
            }
            return None;
        }
        // A small range — inside one aligned block — reads that block and is kept (change 1);
        // a whole file is never answered from memory, so a reload sees it as it is.
        let k = first / CACHE_BLOCK;
        let block = (part && k == (end - 1) / CACHE_BLOCK).then_some(k);
        let kept = block.and_then(|k| self.blocks.get(&file, k));
        // The first bytes come with the body itself (its block 0, or a run from byte 0), or by
        // a read of their own before it (a range further on).
        let with_body = block == Some(0) || (block.is_none() && first == 0);
        let sniff = found.is_none() && !with_body;
        let body_cost = match (block, &kept) {
            (Some(_), Some(_)) => 0,
            (Some(k), None) => CACHE_BLOCK.min(size - k * CACHE_BLOCK),
            (None, _) => end - first,
        };
        let cost = body_cost + if sniff { sample } else { 0 };
        if !self.ledger.take(cost) {
            let why = "the preview's read budget is spent — Enter shows the whole page";
            self.failed(&req, out, why);
            return None;
        }
        req.owed = cost;
        if kept.is_none() || sniff {
            // A new whole-file request opens the file again (2 RTT, its fingerprint compared):
            // the kept handle is for runs of ranges.
            match self.open(&file, false) {
                Ok(()) => {}
                Err(no) => match out {
                    Out::Waiting { reply, .. } if was_checked => {
                        return self.not_open(req, reply, no, recheck);
                    }
                    out => {
                        self.failed(&req, out, no.why());
                        return None;
                    }
                },
            }
        }
        if sniff {
            match self.read_all(&mut req.owed, 0, sample) {
                Ok(head) => {
                    let f = charset(mime, &head, truncated);
                    self.charsets.insert(file.version(), f);
                    found = Some(f);
                }
                Err(halt) => return self.halt(&req, out, halt),
            }
        }
        // The head goes now, unless it waits for the body's first bytes.
        if let Some(f) = found {
            if let Err(halt) = out.resolve(f) {
                return self.halt(&req, out, halt);
            }
        }
        let Some(k) = block else {
            let open = Open {
                out,
                file,
                at: first,
                end,
                code,
            };
            return self.piece(req, open);
        };
        // A small request: its block, kept or read whole (change 1).
        let data = match kept {
            Some(data) => data,
            None => {
                let len = CACHE_BLOCK.min(size - k * CACHE_BLOCK);
                match self.read_all(&mut req.owed, k * CACHE_BLOCK, len) {
                    Ok(data) => {
                        let data = Bytes::from(data);
                        self.blocks.put(&file, k, data.clone());
                        data
                    }
                    Err(halt) => return self.halt(&req, out, halt),
                }
            }
        };
        // Block 0 holds the sample (a block is longer than it).
        if let Out::Waiting { .. } = out {
            let f = charset(mime, &data[..sample as usize], truncated);
            self.charsets.insert(file.version(), f);
            if let Err(halt) = out.resolve(f) {
                return self.halt(&req, out, halt);
            }
        }
        let from = (first - k * CACHE_BLOCK) as usize;
        match out.push(&data[from..from + (end - first) as usize], 0, false) {
            Ok(_) => self.close(&req, out, code),
            Err(halt) => {
                self.halt(&req, out, halt);
            }
        }
        None
    }

    /// A checked file that would not open again — it moved or is gone, and [`Worker::open`]
    /// forgot it: the request has sent nothing, so it runs its steps afresh, once (a 404 for a
    /// file that is gone, not a network error on every reload).
    fn not_open(&mut self, mut req: Req, reply: Reply, no: NotOpen, recheck: bool) -> Option<Req> {
        self.ledger.give(std::mem::take(&mut req.owed));
        match recheck {
            true => self.first(req, reply, false),
            false => {
                self.fail_reply(&req, reply, no.why());
                None
            }
        }
    }

    /// One piece of an open response.
    fn piece(&mut self, mut req: Req, mut open: Open) -> Option<Req> {
        if open.out.stopped() {
            self.stopped(&req);
            return None;
        }
        if let Err(no) = self.open(&open.file, false) {
            self.failed(&req, open.out, no.why());
            return None;
        }
        let others = self.queue.iter().any(Req::ready);
        let len = piece_for(self.rate, others)
            .min(open.out.room())
            .min(open.end - open.at);
        let sample = open.file.size.min(kind::SNIFF_BYTES as u64) as usize;
        let truncated = open.file.size > sample as u64;
        let mut found = None;
        let out = &mut open.out;
        let read = self.read_span(&mut req.owed, open.at, len, &mut |bytes| {
            if let Some(f) = out.push(bytes, sample, truncated)? {
                found = Some(f);
            }
            Ok(())
        });
        if let Err(halt) = read {
            return self.halt(&req, open.out, halt);
        }
        // Kept only once the piece is whole: a fault forgot the file, its charset with it.
        if let Some(f) = found {
            self.charsets.insert(open.file.version(), f);
        }
        open.at += len;
        if open.at < open.end {
            req.stage = Stage::Open(open);
            return Some(req);
        }
        self.close(&req, open.out, open.code);
        None
    }

    /// The response's end: its body's. A waiting head has gone by then: a run from byte 0 holds
    /// the sample (at most the file's first 8 KiB, so at most the run).
    fn close(&self, req: &Req, out: Out, code: u16) {
        match out {
            Out::Body(body) => match body.done() {
                Ok(()) => self.answered(req, code),
                Err(scheme::Refused::Stopped) => self.stopped(req),
                Err(e) => self.note(|| format!("page {} {} failed: {e}", req.id.get(), req.path)),
            },
            out => self.failed(req, out, "the page server ended before its head"),
        }
    }

    fn halt(&self, req: &Req, out: Out, halt: Halt) -> Option<Req> {
        match halt {
            Halt::Stopped => self.stopped(req),
            Halt::Failed(why) => self.failed(req, out, &why),
        }
        None
    }

    // -----------------------------------------------------------------------------------------
    // The connection.

    /// The connection as a `RemoteFs`: the open file's handle closed (`into_connection`).
    fn conn(&mut self) -> Result<Box<dyn RemoteFs + Send>, String> {
        match std::mem::replace(&mut self.link, Link::Lost(String::new())) {
            Link::Conn(conn) => Ok(conn),
            Link::File(file) => Ok(file.into_connection()),
            Link::Lost(why) => {
                self.link = Link::Lost(why.clone());
                Err(why)
            }
        }
    }

    /// `conn` becomes the transport of `path`: SFTP's `into_ranged`, or a local file in place;
    /// under the throttle hook.
    fn file_on(
        &self,
        conn: Box<dyn RemoteFs + Send>,
        path: &str,
    ) -> Result<Box<dyn Ranged>, remote::Refused> {
        let file = match self.in_place {
            true => InPlace::open(path).map_err(|error| remote::Refused { conn, error })?,
            false => conn.into_ranged(path)?,
        };
        Ok(match self.throttle {
            Some(t) => Throttled::wrap(file, t),
            None => file,
        })
    }

    /// Steps 2–4 for `joined` (§3.1): the file checked, its handle open.
    fn steps(&mut self, joined: &str, slash: bool, reply: &Reply) -> Result<Checked, NoFile> {
        let mut conn = self.conn().map_err(NoFile::Failed)?;
        let checked = self.check(&mut *conn, joined, slash, reply);
        let (resolved, name, folder) = match checked {
            Ok(found) => found,
            Err(no) => {
                self.link = Link::Conn(conn);
                return Err(no);
            }
        };
        match self.file_on(conn, &resolved) {
            Ok(file) => {
                let at = file.opened();
                self.link = Link::File(file);
                Ok(Checked {
                    resolved,
                    name,
                    size: at.size,
                    mtime: at.mtime,
                    folder,
                })
            }
            Err(remote::Refused { conn, error }) => {
                self.link = Link::Conn(conn);
                Err(NoFile::Failed(format!("cannot open {resolved}: {error:#}")))
            }
        }
    }

    /// Steps 2 and 3 on `conn`: the path served for `joined`, the name its type is by (the one
    /// asked, or `index.html`), and whether it is a folder's index — only for a folder asked with
    /// its `/` (`slash`); without it, [`NoFile::Folder`]. A stop ends them after the step in
    /// progress.
    fn check(
        &self,
        conn: &mut dyn RemoteFs,
        joined: &str,
        slash: bool,
        reply: &Reply,
    ) -> Result<(String, String, bool), NoFile> {
        let live = || match reply.stopped() {
            true => Err(NoFile::Stopped),
            false => Ok(()),
        };
        let resolved = self.resolve(conn, joined)?;
        live()?;
        let at = stat(conn, &resolved)?;
        live()?;
        if at.is_file {
            let name = joined.rsplit('/').next().unwrap_or_default();
            return Ok((resolved, name.to_string(), false));
        }
        if !at.is_dir {
            return Err(NoFile::Missing);
        }
        if !slash {
            return Err(NoFile::Folder);
        }
        // R2: the index goes through steps 2 and 3 again.
        let index = match resolved.as_str() {
            "/" => "/index.html".to_string(),
            dir => format!("{dir}/index.html"),
        };
        let resolved = self.resolve(conn, &index)?;
        live()?;
        let at = stat(conn, &resolved)?;
        live()?;
        match at.is_file {
            true => Ok((resolved, "index.html".to_string(), true)),
            false => Err(NoFile::Missing),
        }
    }

    /// Step 2: `realpath`, inside the root. A path that does not resolve is 404 when nothing is
    /// there, else the error.
    fn resolve(&self, conn: &mut dyn RemoteFs, path: &str) -> Result<String, NoFile> {
        match conn.realpath(path) {
            Ok(resolved) if inside(&self.root, &resolved) => Ok(resolved),
            Ok(_) => Err(NoFile::Refused),
            Err(error) => match conn.stat_opt(path) {
                Ok(None) => Err(NoFile::Missing),
                Ok(Some(_)) | Err(_) => Err(NoFile::Failed(format!("{error:#}"))),
            },
        }
    }

    /// The handle on `file`: kept when it is on it already, else a `reopen` (or an open from the
    /// connection) — its fingerprint compared either way. A file that moved is forgotten, so the
    /// next request checks it afresh.
    /// `fresh`: opened again even when the handle is on it (a new whole-file request). A file that
    /// would not open is forgotten too.
    fn open(&mut self, file: &Checked, fresh: bool) -> Result<(), NotOpen> {
        let opened = match std::mem::replace(&mut self.link, Link::Lost(String::new())) {
            Link::File(handle) if handle.path() == file.resolved && !fresh => Ok(handle),
            Link::File(handle) => handle.reopen(&file.resolved),
            Link::Conn(conn) => self.file_on(conn, &file.resolved),
            Link::Lost(why) => {
                self.link = Link::Lost(why.clone());
                return Err(NotOpen::Failed(why));
            }
        };
        match opened {
            Ok(handle) => {
                let moved = !file.matches(&handle.opened());
                self.link = Link::File(handle);
                if moved {
                    self.forget(&file.resolved);
                    return Err(NotOpen::Moved(format!(
                        "{}: the file changed since the page read it — Cmd/Ctrl+R reloads",
                        file.resolved
                    )));
                }
                Ok(())
            }
            Err(remote::Refused { conn, error }) => {
                self.link = Link::Conn(conn);
                self.forget(&file.resolved);
                let why = format!("cannot open {}: {error:#}", file.resolved);
                Err(NotOpen::Failed(why))
            }
        }
    }

    fn forget(&mut self, resolved: &str) {
        self.checked.retain(|_, c| c.resolved != resolved);
        self.charsets.retain(|(p, _, _), _| p != resolved);
        self.blocks.purge(resolved);
    }

    /// `len` bytes at `off` of the open file, whole.
    fn read_all(&mut self, owed: &mut u64, off: u64, len: u64) -> Result<Vec<u8>, Halt> {
        let mut out = Vec::with_capacity(len as usize);
        self.read_span(owed, off, len, &mut |bytes| {
            out.extend_from_slice(bytes);
            Ok(())
        })?;
        Ok(out)
    }

    /// Reads `len` bytes at `off` of the open file — 64 KiB reads, all sent at once — handing
    /// each to `sink` in file order as it arrives. A short reply's rest is read at its own offset
    /// (the `Ranged` contract); an empty one inside the file is an early end and fails. Every read
    /// sent is waited for, whatever happens, so nothing is left for the next request.
    fn read_span(
        &mut self,
        owed: &mut u64,
        off: u64,
        len: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), Halt>,
    ) -> Result<(), Halt> {
        let mut file = match std::mem::replace(&mut self.link, Link::Lost(String::new())) {
            Link::File(file) => file,
            other => {
                self.link = other;
                return Err(Halt::Failed("the page server has no file open".into()));
            }
        };
        let path = file.path().to_string();
        let mut slots = VecDeque::new();
        let mut at = off;
        while at < off + len {
            let n = READ.min(off + len - at);
            let ticket = file.read(at, n as u32);
            slots.push_back(Slot {
                off: at,
                len: n,
                state: Fill::Sent(ticket),
            });
            at += n;
        }
        *owed = owed.saturating_sub(len);
        let mut halt = None;
        // A reply that failed, ended early or did not fit: the file is not what was checked.
        let mut fault = false;
        let (mut t0, mut last, mut after) = (None, None, 0u64);
        let mut silent = 0;
        while slots.iter().any(|s| matches!(s.state, Fill::Sent(_))) {
            let Some(c) = file.next(None) else {
                // No reply and none can come: the transport is gone, and the handle with it.
                silent += 1;
                if silent < 3 && file.pending() > 0 {
                    continue;
                }
                let why = format!("{path}: the connection gave no reply");
                self.link = Link::Lost(why.clone());
                return Err(Halt::Failed(why));
            };
            silent = 0;
            let now = file.now();
            match t0 {
                None => t0 = Some(now),
                Some(_) => {
                    after += c.data().map_or(0, |b| b.len() as u64);
                    last = Some(now);
                }
            }
            let Some(i) = slots
                .iter()
                .position(|s| matches!(s.state, Fill::Sent(t) if t == c.ticket))
            else {
                continue;
            };
            let Completion { reply, .. } = c;
            let want = slots[i].len;
            let failure = match &reply {
                Err(e) => Some(format!("cannot read {path}: {e:#}")),
                Ok(remote::Reply::Data(b)) if b.is_empty() => Some(format!(
                    "{path} ended early: it changed since the page read it"
                )),
                Ok(remote::Reply::Data(b)) if b.len() as u64 > want => {
                    Some(format!("{path}: the server sent more than was asked"))
                }
                Ok(remote::Reply::Stat(_)) => Some(format!("{path}: a read answered with a stat")),
                Ok(remote::Reply::Data(_)) => None,
            };
            match (failure, reply) {
                (Some(why), _) => {
                    fault = true;
                    halt.get_or_insert(Halt::Failed(why));
                    slots[i].state = Fill::Dead;
                }
                (None, Ok(remote::Reply::Data(b))) => {
                    let n = b.len() as u64;
                    if n < want && halt.is_none() {
                        let rest = slots[i].off + n;
                        let ticket = file.read(rest, (want - n) as u32);
                        slots[i].len = n;
                        slots.insert(
                            i + 1,
                            Slot {
                                off: rest,
                                len: want - n,
                                state: Fill::Sent(ticket),
                            },
                        );
                    }
                    slots[i].state = Fill::Got(b);
                }
                (None, _) => slots[i].state = Fill::Dead,
            }
            // What is in order goes on; after a halt it is only waited for.
            while slots
                .front()
                .is_some_and(|s| matches!(s.state, Fill::Got(_) | Fill::Dead))
            {
                let slot = slots.pop_front().expect("a slot");
                if let (Fill::Got(bytes), None) = (slot.state, &halt) {
                    if let Err(h) = sink(&bytes) {
                        halt = Some(h);
                    }
                }
            }
        }
        self.link = Link::File(file);
        // Its check, its charset and its kept blocks are forgotten: the next request runs the
        // steps again, and no kept block of it is served.
        if fault {
            self.forget(&path);
        }
        if let (Some(t0), Some(last)) = (t0, last) {
            let took = last.saturating_duration_since(t0).as_secs_f64();
            if took > 0.0 && after > 0 {
                let rate = after as f64 / took;
                self.rate = Some(self.rate.map_or(rate, |r| (r + rate) / 2.0));
            }
        }
        match halt {
            None => Ok(()),
            Some(h) => Err(h),
        }
    }
}

/// One read of a span.
struct Slot {
    off: u64,
    len: u64,
    state: Fill,
}

enum Fill {
    Sent(Ticket),
    Got(Bytes),
    /// Failed: nothing to deliver.
    Dead,
}

fn stat(conn: &mut dyn RemoteFs, path: &str) -> Result<Stat, NoFile> {
    match conn.stat_opt(path) {
        Ok(Some(at)) => Ok(at),
        Ok(None) => Err(NoFile::Missing),
        Err(e) => Err(NoFile::Failed(format!("{e:#}"))),
    }
}

/// What a folder asked without its `/` (`encoded`: the path as asked) gets instead of its index,
/// whose relative links would resolve one level up there (§3.1 step 3): the scheme has no
/// redirect status, so a page that sends the engine to `NAME/` — NAME the last segment as the
/// request had it, with any byte that could end the attribute or the refresh's URL, or make
/// `NAME:` a scheme, percent-encoded.
fn folder_page(encoded: &str) -> Vec<u8> {
    let mut name = String::new();
    for b in encoded.rsplit('/').next().unwrap_or_default().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => name.push(char::from(b)),
            b'-' | b'.' | b'_' | b'~' | b'%' | b'!' | b'$' | b'(' | b')' | b'*' | b'+' | b',' => {
                name.push(char::from(b))
            }
            b';' | b'=' | b'@' => name.push(char::from(b)),
            _ => name.push_str(&format!("%{b:02X}")),
        }
    }
    format!(
        "<!doctype html><meta http-equiv=\"refresh\" content=\"0;url={name}/\">\
         <a href=\"{name}/\">{name}/</a>"
    )
    .into_bytes()
}

/// `%XX` decoded (a `%` not before two hex digits stays itself, as a URL parser keeps it).
fn decoded(encoded: &str) -> Option<String> {
    let b = encoded.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: Option<&u8>| c.and_then(|&c| char::from(c).to_digit(16));
    while i < b.len() {
        if b[i] == b'%' {
            if let (Some(h), Some(l)) = (hex(b.get(i + 1)), hex(b.get(i + 2))) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// Step 1: the asked path decoded and joined under `root`; `None` when it is refused (a NUL, or
/// a `..` — never resolved, so nothing outside the root is even looked at) or names nothing a
/// path can hold (not UTF-8: [`decoded`] tells the two apart). `.` and empty components are
/// nothing.
fn join(root: &str, encoded: &str) -> Option<String> {
    let path = decoded(encoded)?;
    if path.contains('\0') {
        return None;
    }
    let mut out = root.to_string();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            name => {
                if !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(name);
            }
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// A local page, in place.

/// A local file read in place (06 §1) as a [`Ranged`]: each request answered as it is queued,
/// in order. `LocalFs` streams only in tests; this is the page server's own.
struct InPlace {
    file: std::fs::File,
    path: String,
    opened: Stat,
    done: VecDeque<Completion>,
    next: u64,
}

impl InPlace {
    fn open(path: &str) -> anyhow::Result<Box<dyn Ranged>> {
        // Without blocking: a FIFO opened for reading waits for a writer. Nothing else changes —
        // a regular file's reads are positioned and never wait.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .with_context(|| format!("cannot open {path}"))?;
        let opened = stat_of(&file.metadata()?);
        if !opened.is_file {
            bail!("{path} is not a regular file");
        }
        Ok(Box::new(InPlace {
            file,
            path: path.to_string(),
            opened,
            done: VecDeque::new(),
            next: 0,
        }))
    }

    fn answer(&mut self, asked: Asked, reply: anyhow::Result<remote::Reply>) -> Ticket {
        let ticket = Ticket(self.next);
        self.next += 1;
        self.done.push_back(Completion {
            ticket,
            asked,
            reply,
            took: Duration::ZERO,
            at: Instant::now(),
        });
        ticket
    }
}

fn stat_of(meta: &std::fs::Metadata) -> Stat {
    Stat {
        is_dir: meta.is_dir(),
        is_file: meta.is_file(),
        is_symlink: meta.file_type().is_symlink(),
        size: meta.len(),
        mtime: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
        mode: meta.permissions().mode() & 0o7777,
        uid: Some(meta.uid()),
        gid: Some(meta.gid()),
        missing: None,
        sha256: None,
    }
}

impl Ranged for InPlace {
    fn path(&self) -> &str {
        &self.path
    }

    fn opened(&self) -> Stat {
        self.opened
    }

    fn read(&mut self, off: u64, len: u32) -> Ticket {
        let len = self.opened.size.saturating_sub(off).min(u64::from(len)) as u32;
        let mut buf = vec![0; len as usize];
        let file = &self.file;
        let got = super::fill(|b, at| file.read_at(b, at), off, &mut buf, None);
        let reply = got
            .map(|n| {
                buf.truncate(n);
                remote::Reply::Data(buf.into())
            })
            .map_err(|e| anyhow!("{e}"));
        self.answer(Asked::Read { off, len }, reply)
    }

    fn fstat(&mut self) -> Ticket {
        let reply = self
            .file
            .metadata()
            .map(|m| remote::Reply::Stat(Some(stat_of(&m))))
            .map_err(anyhow::Error::from);
        self.answer(Asked::Fstat, reply)
    }

    fn stat_path(&mut self) -> Ticket {
        let reply = match std::fs::metadata(&self.path) {
            Ok(m) => Ok(remote::Reply::Stat(Some(stat_of(&m)))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(remote::Reply::Stat(None)),
            Err(e) => Err(anyhow::Error::from(e)),
        };
        self.answer(Asked::StatPath, reply)
    }

    fn pending(&self) -> usize {
        self.done.len()
    }

    /// Every request is answered as it is queued, so with none queued nothing can come; a caller
    /// that asked to wait (`Throttled` holding a reply until it is due) is given the time.
    fn next(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        let next = self.done.pop_front();
        if let (None, Some(wait)) = (&next, timeout) {
            std::thread::sleep(wait);
        }
        next
    }

    fn waker(&self) -> Wake {
        Wake::new(|| {})
    }

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn reopen(self: Box<Self>, path: &str) -> Result<Box<dyn Ranged>, remote::Refused> {
        InPlace::open(path).map_err(|error| remote::Refused {
            conn: Box::new(LocalFs::new()),
            error,
        })
    }

    fn into_connection(self: Box<Self>) -> Box<dyn RemoteFs + Send> {
        Box::new(LocalFs::new())
    }

    fn close(self: Box<Self>) {}
}

#[cfg(test)]
mod tests;
