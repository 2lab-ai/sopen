//! The hex window (spec §6a): any file as rows of 16 bytes — offset, hex, ASCII — read through a
//! small page cache, so only the rows on screen are ever read and a file of any size scrolls
//! the same. Editing overwrites bytes only (the size never changes); Undo/Redo; Save (backup
//! first, conflict-checked overwrite) and Save copy (a new sibling), as in the image window.
//!
//! [`HexDoc`] is the edit model (the bytes as opened plus a sparse map of changed bytes) and
//! [`HexFile`] the write side; both are window-free, so the tests and the `SOPEN_AUTOSAVE` hook
//! run the exact code the window's buttons run.

use super::chrome::{self, HEX_KEYS};
use super::image::CANNOT_REPLACE;
use super::{Hooks, Origin, Outcome, Status};
use crate::remote::WriteOutcome;
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui::{self, Color32, Key, Pos2, Rect, Sense, Vec2};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Bytes per row.
pub const ROW: u64 = 16;
/// The page cache: pages of this many bytes, at most [`PAGES`] of them held.
const PAGE: u64 = 4096;
const PAGES: usize = 64;
/// Undo history bound (one step per typed digit or character).
const UNDO_STEPS: usize = 100_000;
/// The window's size when it opens as a hex window.
pub const WINDOW: [f32; 2] = [760.0, 560.0];
/// The window's title in its frame, as PC Shell names its hex view.
const TITLE: &str = "File Edit";

/// An offset as typed: `0x…` (hex) or decimal digits.
pub fn parse_offset(text: &str) -> Result<u64> {
    let t = text.trim();
    let bad = || anyhow!("'{t}' is not an offset (0x… for hex, or decimal)");
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) => {
            u64::from_str_radix(hex, 16).map_err(|_| bad())
        }
        Some(_) => Err(bad()),
        None if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => {
            t.parse().map_err(|_| bad())
        }
        None => Err(bad()),
    }
}

/// Bytes as hex digits, two per byte (`deadBEEF`).
pub fn parse_bytes(text: &str) -> Result<Vec<u8>> {
    let t = text.trim();
    if t.is_empty() || t.len() % 2 == 1 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("'{t}' is not bytes in hex (two digits per byte)");
    }
    (0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16).map_err(|_| anyhow!("'{t}' is not hex")))
        .collect()
}

/// `1234567` → `1,234,567`.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Hex digits the offset column needs for a file of `size` bytes (at least 4).
pub fn offset_digits(size: u64) -> usize {
    let last = size.saturating_sub(1);
    let bits = 64 - last.leading_zeros() as usize;
    bits.div_ceil(4).max(4)
}

/// Decimal digits the offset column needs for a file of `size` bytes (at least 4).
pub fn decimal_digits(size: u64) -> usize {
    size.saturating_sub(1).to_string().len().max(4)
}

/// A row's offset as PC Shell's File Edit prints it: decimal, then hex in parentheses —
/// `0016(0010)` — each as wide as the file's last offset needs (at least four digits), so the
/// column keeps its width down the file.
pub fn offset_text(at: u64, size: u64) -> String {
    let (dec, hex) = (decimal_digits(size), offset_digits(size));
    format!("{at:0dec$}({at:0hex$X})")
}

/// One row's 16 (or fewer, at the end) bytes as the hex column shows them: two digits each,
/// one space between, two between the 8th and the 9th.
pub fn hex_cells(bytes: &[Option<u8>]) -> String {
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            out.push_str(if i == 8 { "  " } else { " " });
        }
        match b {
            Some(b) => out.push_str(&format!("{b:02X}")),
            None => out.push_str("??"),
        }
    }
    out
}

/// The ASCII column's character for a byte: its code page 437 glyph, as PC Shell shows the
/// column — printable ASCII as itself, ☺ ♥ ▲ … for the control bytes, Ç é ░ ═ … for the upper
/// half, a blank for 0x00 — and `?` for a byte that could not be read (`??` beside it).
pub fn ascii_char(b: Option<u8>) -> char {
    match b {
        Some(b) => chrome::cp437(b),
        None => '?',
    }
}

/// Where the hex column draws byte `i` of a row, in characters from the column's start.
fn hex_col(i: u64) -> u64 {
    3 * i + u64::from(i >= 8)
}

/// Characters in a full row's hex column.
const HEX_CHARS: u64 = 16 * 3;

// ---------------------------------------------------------------------------------------------
// The edit model.

/// Reads a file through a small page cache: only pages that are asked for are read, at most
/// [`PAGES`] are held (the least recently used one goes first).
struct Pages {
    file: Arc<File>,
    size: u64,
    /// Page number → (bytes, last use). An unreadable page is held empty.
    held: HashMap<u64, (Vec<u8>, u64)>,
    tick: u64,
    /// The first read error, for the status line.
    error: Option<String>,
    /// Pages read so far (each read is one page).
    reads: u64,
}

impl Pages {
    fn byte(&mut self, at: u64) -> Option<u8> {
        if at >= self.size {
            return None;
        }
        let page = at / PAGE;
        self.tick += 1;
        let tick = self.tick;
        if !self.held.contains_key(&page) {
            if self.held.len() >= PAGES {
                let oldest = self.held.iter().min_by_key(|(_, (_, used))| *used);
                if let Some(old) = oldest.map(|(k, _)| *k) {
                    self.held.remove(&old);
                }
            }
            let start = page * PAGE;
            let mut buf = vec![0; (self.size - start).min(PAGE) as usize];
            self.reads += 1;
            if let Err(e) = self.file.read_exact_at(&mut buf, start) {
                self.error
                    .get_or_insert_with(|| format!("cannot read at 0x{start:X}: {e}"));
                buf.clear();
            }
            self.held.insert(page, (buf, tick));
        }
        let (buf, used) = self.held.get_mut(&page)?;
        *used = tick;
        buf.get((at - page * PAGE) as usize).copied()
    }

    fn resident(&self) -> usize {
        self.held.values().map(|(b, _)| b.capacity()).sum()
    }
}

/// Which column the cursor types into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Hex,
    Ascii,
}

/// The cursor: a byte, the column it types into, and (in the hex column) whether the next
/// digit is the byte's low nibble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub at: u64,
    pub pane: Pane,
    pub low: bool,
}

/// One change, as the bytes were shown: undone by putting `before` back, redone by `after`.
#[derive(Debug, Clone, Copy)]
struct Step {
    at: u64,
    before: u8,
    after: u8,
    /// Where the cursor was when the change was made (undo puts it back there).
    cursor: Cursor,
}

/// The bytes as opened (never written: read through [`Pages`]) and the edits made to them: a
/// sparse map offset → byte holding the byte wanted at every offset set — absolute values, as
/// shown, never said relative to the file under them. An entry may equal the file's byte (an
/// edit undone, a byte typed back); whether a byte is changed is a comparison with the file, so
/// when the file under the edits is replaced (a Save in place) nothing about the edits has to
/// be read from the old one.
pub struct HexDoc {
    pages: Pages,
    patches: BTreeMap<u64, u8>,
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    /// Undo steps kept (the oldest goes first).
    undo_cap: usize,
    /// The byte whose high nibble was just typed (its step is the last one): the low nibble
    /// typed next joins that step, so a byte typed as two digits undoes as one.
    open_step: Option<u64>,
    /// Bumped by every change (a repaint key; what is saved is compared by content).
    rev: u64,
    /// The bytes the file holds beyond the bytes read (what the last Save in it wrote), and
    /// the ones the last Save copy holds — absolute, like `patches`.
    saved: BTreeMap<u64, u8>,
    copied: Option<BTreeMap<u64, u8>>,
    /// `changed_count` at `rev` (it compares every entry with the file).
    counted: Option<(u64, usize)>,
    /// Bumped by everything that can change `dirty` / `unsaved` (edits, a Save or a Save copy
    /// landing, a rebase); their answers are kept for the generation they were given at.
    generation: u64,
    answers: Option<(u64, bool, bool)>,
}

/// Whether the bytes shown with `a` and with `b` over the file differ anywhere (a byte the file
/// cannot give counts as different). Walks both maps; reads the file only where one has an
/// entry and the other does not.
fn differ(pages: &mut Pages, a: &BTreeMap<u64, u8>, b: &BTreeMap<u64, u8>) -> bool {
    for (&at, &x) in a {
        match b.get(&at) {
            Some(&y) if x != y => return true,
            Some(_) => {}
            None if pages.byte(at) != Some(x) => return true,
            None => {}
        }
    }
    b.iter()
        .any(|(&at, &y)| !a.contains_key(&at) && pages.byte(at) != Some(y))
}

fn pages(file: Arc<File>, size: u64) -> Pages {
    Pages {
        file,
        size,
        held: HashMap::new(),
        tick: 0,
        error: None,
        reads: 0,
    }
}

impl HexDoc {
    /// The document over `file`, whose first `size` bytes are shown.
    pub fn new(file: Arc<File>, size: u64) -> Self {
        Self {
            pages: pages(file, size),
            patches: BTreeMap::new(),
            undo: VecDeque::new(),
            redo: Vec::new(),
            undo_cap: UNDO_STEPS,
            open_step: None,
            rev: 0,
            saved: BTreeMap::new(),
            copied: None,
            counted: None,
            generation: 0,
            answers: None,
        }
    }

    /// A smaller undo bound (tests reach it without a hundred thousand edits).
    #[cfg(test)]
    pub(crate) fn set_undo_cap(&mut self, cap: usize) {
        self.undo_cap = cap;
    }

    /// Undo steps held now.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn size(&self) -> u64 {
        self.pages.size
    }

    /// Rows in the grid.
    pub fn rows(&self) -> u64 {
        self.size().div_ceil(ROW)
    }

    /// The byte at `at` as edited (`None` past the end, or where the file cannot be read).
    pub fn byte(&mut self, at: u64) -> Option<u8> {
        if at >= self.size() {
            return None;
        }
        match self.patches.get(&at) {
            Some(&b) => Some(b),
            None => self.pages.byte(at),
        }
    }

    /// The byte the file holds.
    pub fn original(&mut self, at: u64) -> Option<u8> {
        self.pages.byte(at)
    }

    /// Whether the byte at `at` differs from the file (one that cannot be read counts as
    /// different).
    pub fn changed(&mut self, at: u64) -> bool {
        match self.patches.get(&at).copied() {
            Some(b) => self.pages.byte(at) != Some(b),
            None => false,
        }
    }

    /// How many bytes differ from the file.
    pub fn changed_count(&mut self) -> usize {
        if let Some((rev, n)) = self.counted {
            if rev == self.rev {
                return n;
            }
        }
        let entries: Vec<(u64, u8)> = self.patches.iter().map(|(&a, &b)| (a, b)).collect();
        let n = entries
            .into_iter()
            .filter(|&(at, b)| self.pages.byte(at) != Some(b))
            .count();
        self.counted = Some((self.rev, n));
        n
    }

    /// The bytes wanted at every offset set (absolute; some may equal the file's).
    pub fn patches(&self) -> &BTreeMap<u64, u8> {
        &self.patches
    }

    /// The first read error, if any.
    pub fn read_error(&self) -> Option<&str> {
        self.pages.error.as_deref()
    }

    /// Bytes the page cache holds now.
    pub fn resident(&self) -> usize {
        self.pages.resident()
    }

    /// Pages read from the file so far.
    pub fn reads(&self) -> u64 {
        self.pages.reads
    }

    /// Overwrites the byte at `at` with `value`, one undo step (`cursor` = where the cursor is,
    /// for the undo). Writing the byte already shown is no step. `false` past the end or where
    /// the file cannot be read.
    pub fn set(&mut self, at: u64, value: u8, cursor: Cursor) -> bool {
        self.change(at, value, cursor).is_some()
    }

    /// [`HexDoc::set`], saying whether it made an undo step (`Some(false)`: the byte already
    /// showed `value`; `None`: past the end or unreadable).
    fn change(&mut self, at: u64, value: u8, cursor: Cursor) -> Option<bool> {
        self.open_step = None;
        let before = self.byte(at)?;
        if before == value {
            return Some(false);
        }
        self.patches.insert(at, value);
        self.undo.push_back(Step {
            at,
            before,
            after: value,
            cursor,
        });
        while self.undo.len() > self.undo_cap.max(1) {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.bump();
        Some(true)
    }

    /// The low nibble typed right after the high one: into the high nibble's step when it
    /// made one, else a step of its own.
    fn set_low(&mut self, at: u64, value: u8, cursor: Cursor) -> bool {
        let open =
            self.open_step.take() == Some(at) && self.undo.back().is_some_and(|s| s.at == at);
        if !open {
            return self.set(at, value, cursor);
        }
        let step = self.undo.back_mut().expect("checked above");
        step.after = value;
        if step.before == value {
            self.undo.pop_back();
        }
        self.patches.insert(at, value);
        self.bump();
        true
    }

    /// `c` typed at `cursor`: in the hex column a hex digit overwrites the high nibble (the
    /// cursor stays for the low one), then the low nibble (the cursor moves on) — one undo step
    /// for the two; in the ASCII column a printable ASCII character overwrites the byte and the
    /// cursor moves on. Returns the cursor after it, or `None` when `c` does nothing there.
    pub fn type_char(&mut self, cursor: Cursor, c: char) -> Option<Cursor> {
        let last = self.size().checked_sub(1)?;
        let at = cursor.at.min(last);
        let current = self.byte(at)?;
        let next = Cursor {
            at: (at + 1).min(last),
            low: false,
            ..cursor
        };
        match cursor.pane {
            Pane::Hex => {
                let digit = c.to_digit(16)? as u8;
                if cursor.low {
                    self.set_low(at, (current & 0xf0) | digit, cursor)
                        .then_some(next)
                } else {
                    let made_step = self.change(at, (digit << 4) | (current & 0x0f), cursor)?;
                    if made_step {
                        self.open_step = Some(at);
                    }
                    Some(Cursor {
                        at,
                        low: true,
                        ..cursor
                    })
                }
            }
            Pane::Ascii => {
                if !(' '..='~').contains(&c) {
                    return None;
                }
                self.set(at, c as u8, cursor).then_some(next)
            }
        }
    }

    /// Undoes the last change; returns where the cursor goes.
    pub fn undo(&mut self) -> Option<Cursor> {
        self.open_step = None;
        let step = self.undo.pop_back()?;
        self.patches.insert(step.at, step.before);
        self.redo.push(step);
        self.bump();
        Some(Cursor {
            at: step.at,
            low: false,
            ..step.cursor
        })
    }

    /// Redoes the last undone change; returns where the cursor goes.
    pub fn redo(&mut self) -> Option<Cursor> {
        self.open_step = None;
        let step = self.redo.pop()?;
        self.patches.insert(step.at, step.after);
        self.undo.push_back(step);
        self.bump();
        Some(Cursor {
            at: step.at,
            low: false,
            ..step.cursor
        })
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Something shown changed.
    fn bump(&mut self) {
        self.rev += 1;
        self.generation += 1;
    }

    /// `dirty` and `unsaved`, worked out once per generation.
    fn answers(&mut self) -> (bool, bool) {
        if let Some((generation, dirty, unsaved)) = self.answers {
            if generation == self.generation {
                return (dirty, unsaved);
            }
        }
        let dirty = differ(&mut self.pages, &self.patches, &self.saved);
        let unsaved = dirty
            && match &self.copied {
                Some(copied) => differ(&mut self.pages, &self.patches, copied),
                None => true,
            };
        self.answers = Some((self.generation, dirty, unsaved));
        (dirty, unsaved)
    }

    /// The bytes differ from what the file holds now (what Save would write) — by content: an
    /// edit undone, or a byte typed back to its value, is no change.
    pub fn dirty(&mut self) -> bool {
        self.answers().0
    }

    /// The bytes are written nowhere — neither the file nor the last Save copy (the close
    /// prompt).
    pub fn unsaved(&mut self) -> bool {
        self.answers().1
    }

    /// A Save of `changes` landed: the file holds them now.
    pub fn mark_saved(&mut self, changes: BTreeMap<u64, u8>) {
        self.saved = changes;
        self.generation += 1;
    }

    /// A Save copy of `changes` landed.
    pub fn mark_copied(&mut self, changes: BTreeMap<u64, u8>) {
        self.copied = Some(changes);
        self.generation += 1;
    }

    /// The file was saved in place and is read through `file` from now on — it holds what was
    /// saved (checked by its hash before this). The edits, and every undo and redo step, are
    /// absolute bytes: they stay as they are. An entry is dropped only where it equals the new
    /// file's byte, read from `file`; nothing is read from the old one. What the last Save copy
    /// holds is forgotten, not translated: it was remembered against the old file (an offset
    /// left out meant the old byte), so it no longer says what the copy holds — closing asks
    /// unless the bytes equal the saved file. All or nothing: a byte `file` cannot give is an
    /// error and nothing is changed.
    pub fn rebase(&mut self, file: Arc<File>) -> Result<()> {
        let mut new = pages(file, self.pages.size);
        let mut keep = |m: &BTreeMap<u64, u8>| -> Result<BTreeMap<u64, u8>> {
            let mut out = BTreeMap::new();
            for (&at, &b) in m {
                let there = new.byte(at).ok_or_else(|| {
                    anyhow!(
                        "the saved file cannot be read at 0x{at:X} ({})",
                        new.error
                            .clone()
                            .unwrap_or_else(|| "no byte there".to_string())
                    )
                })?;
                if there != b {
                    out.insert(at, b);
                }
            }
            Ok(out)
        };
        let patches = keep(&self.patches)?;
        let saved = keep(&self.saved)?;
        new.reads += self.pages.reads;
        self.pages = new;
        self.patches = patches;
        self.saved = saved;
        self.copied = None;
        self.bump();
        Ok(())
    }
}

/// `SOPEN_STATE` for the hex window: the patches (each byte one undo step), then the cursor.
fn apply_state(doc: &mut HexDoc, hooks: &Hooks) -> Result<Cursor> {
    let state = &hooks.state;
    let size = doc.size();
    let mut cursor = Cursor::default();
    for (at, bytes) in &state.patches {
        let end = at.checked_add(bytes.len() as u64).unwrap_or(u64::MAX);
        if end > size {
            bail!(
                "SOPEN_STATE patch at 0x{at:X} runs past the end of the file ({} bytes)",
                grouped(size)
            );
        }
        for (i, &b) in bytes.iter().enumerate() {
            if !doc.set(at + i as u64, b, cursor) {
                bail!(
                    "SOPEN_STATE patch: cannot read the byte at 0x{:X}",
                    at + i as u64
                );
            }
        }
    }
    if let Some(at) = state.at {
        if at >= size {
            bail!(
                "SOPEN_STATE at=0x{at:X} is past the end of the file ({} bytes)",
                grouped(size)
            );
        }
        cursor.at = at;
    }
    Ok(cursor)
}

// ---------------------------------------------------------------------------------------------
// The write side.

/// What a file's handle says about it: a difference means its bytes may differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ident {
    len: u64,
    mtime: Option<SystemTime>,
    ctime: (i64, i64),
    ino: u64,
}

impl Ident {
    fn of(file: &File, path: &Path) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {}", path.display()))?;
        Ok(Self {
            len: meta.len(),
            mtime: meta.modified().ok(),
            ctime: (meta.ctime(), meta.ctime_nsec()),
            ino: meta.ino(),
        })
    }

    /// How `now` differs from `self`, for a message.
    fn change(&self, now: &Ident) -> String {
        if now.len != self.len {
            format!("{} bytes then, {} now", grouped(self.len), grouped(now.len))
        } else if now.mtime != self.mtime {
            "its modification time changed".to_string()
        } else {
            "it was written to or replaced (its change time moved)".to_string()
        }
    }
}

/// The local file a hex window reads, opened: the cached copy or the local file itself, by its
/// handle. Opening it is the only step of opening a hex window that can fail on the bytes, so
/// it comes before anything is handed over (the connection, the claim).
pub struct Base {
    file: File,
    path: PathBuf,
    opened: Ident,
}

impl Base {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {}", path.display()))?;
        if !meta.is_file() {
            bail!("{} is not a regular file", path.display());
        }
        let opened = Ident::of(&file, path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            opened,
        })
    }
}

/// The opened file: where it lives, the local file its bytes are read from, and the local places
/// sopen writes to.
pub struct HexFile {
    pub origin: Origin,
    /// The bytes as opened, read through this handle only — never by path: a Save of a local
    /// file renames a new file over the path, and this keeps reading the original.
    base: Arc<File>,
    /// Where `base` was opened: the session's cached copy, or the local file itself.
    base_path: PathBuf,
    /// `base` when it was opened (and when a Save of a local file in place re-bound it).
    opened: Ident,
    /// The session's claim on the cached copy, held while the file is open.
    _claim: Option<crate::cache::Claim>,
    /// Staged edits and refused ones: `<cache>/.edits/<host>/<abs>`.
    edits: PathBuf,
    /// `<cache>` (backups go to `<cache>/.backup/<host>/<abs>.<stamp>`).
    cache_root: PathBuf,
    /// Set by the first successful backup; later saves of the same session keep it.
    backup: Option<PathBuf>,
    /// What the last Save of a local file in place did to the file read (see [`Rebind`]), for
    /// the window to take.
    rebind: Option<Rebind>,
    /// Why nothing more can be saved from this file, if so.
    read_only: Option<String>,
    /// Called between the pre-pass observations (tests: a writer in that gap).
    #[cfg(test)]
    pub(crate) pre_pass_hook: Option<Box<dyn FnMut() + Send>>,
    /// Called after each chunk of a save's pass over the bytes (tests: a writer in the middle).
    #[cfg(test)]
    pub(crate) chunk_hook: Option<Box<dyn FnMut(u64) + Send>>,
    /// The stat that ends a save's pass fails (tests).
    #[cfg(test)]
    pub(crate) fail_stat_after_pass: bool,
}

/// Result of one Save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    /// `notes`: what the owner must still hear (e.g. the remote changed right after the save).
    Written {
        remote: String,
        backup: PathBuf,
        notes: Vec<String>,
    },
    /// Nothing was written: the file changed since it was opened. `kept` = where the edits
    /// went: the staged copy, or (when the bytes as opened changed) a list of the changes.
    Refused {
        message: String,
        kept: Option<PathBuf>,
    },
}

/// A save's pass over the bytes as opened.
struct Pass {
    /// The bytes with the changes applied, in a new owner-only file.
    stage: PathBuf,
    /// The bytes as opened, when the pass wrote the backup too.
    backup: Option<PathBuf>,
    /// SHA-256 of the bytes as opened.
    sha256: [u8; 32],
    /// SHA-256 of the stage: what a Save that lands puts in the file.
    stage_sha256: [u8; 32],
}

/// What a Save of a local file in place did to the file the window reads: the rename put a new
/// file at its path and the old one is gone from it.
pub enum Rebind {
    /// The new file holds exactly what was saved: it is read from now on (the window rebases
    /// its edits on it).
    Rebound(Arc<File>),
    /// It does not (another writer, right after): nothing more is saved from this window.
    Lost(String),
}

/// Files a save's pass made, removed when it is dropped unless kept: every way out of the pass
/// — an error, a refusal, a panic — leaves none behind.
struct Made(Vec<PathBuf>);

impl Made {
    fn keep(mut self) {
        self.0.clear();
    }
}

impl Drop for Made {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

enum PassResult {
    Done(Pass),
    /// The bytes as opened changed (before or during the pass); nothing is kept but this.
    Changed(String),
}

/// Bytes read per step of a save's pass.
const CHUNK: usize = 1 << 20;

impl HexFile {
    /// Opens `base` (the cached copy, or the local file itself) for `origin`. `claim` is held
    /// for as long as this lives.
    pub fn open(
        origin: Origin,
        base: &Path,
        claim: Option<crate::cache::Claim>,
        cache_root: &Path,
    ) -> Result<Self> {
        Ok(Self::new(origin, Base::open(base)?, claim, cache_root))
    }

    /// The hex file over an opened `base`; nothing here can fail.
    pub fn new(
        origin: Origin,
        base: Base,
        claim: Option<crate::cache::Claim>,
        cache_root: &Path,
    ) -> Self {
        let edits = crate::cache::edits_path(cache_root, origin.host(), &origin.abs);
        Self {
            origin,
            base: Arc::new(base.file),
            base_path: base.path,
            opened: base.opened,
            _claim: claim,
            edits,
            cache_root: cache_root.to_path_buf(),
            backup: None,
            rebind: None,
            read_only: None,
            #[cfg(test)]
            pre_pass_hook: None,
            #[cfg(test)]
            chunk_hook: None,
            #[cfg(test)]
            fail_stat_after_pass: false,
        }
    }

    /// Bytes in the file as opened.
    pub fn size(&self) -> u64 {
        self.opened.len
    }

    /// The handle the bytes are read through (for [`HexDoc::new`]).
    pub fn base(&self) -> Arc<File> {
        self.base.clone()
    }

    /// Whether Save can replace the original here (Save copy always can).
    pub fn can_replace(&self) -> bool {
        self.origin.can_replace()
    }

    /// The backup's path: `<cache>/.backup/<host>/<abs>.<stamp>`, `-<random>` added when another
    /// session's backup from the same second has the name.
    fn new_backup(&self) -> Result<(PathBuf, File)> {
        let first = crate::cache::backup_path(
            &self.cache_root,
            self.origin.host(),
            &self.origin.abs,
            SystemTime::now(),
        );
        crate::cache::create_unique(|attempt| {
            if attempt == 0 {
                return first.clone();
            }
            let mut name = first.file_name().unwrap_or_default().to_os_string();
            name.push(format!("-{}", &crate::remote::random_hex()[..8]));
            first.with_file_name(name)
        })
    }

    /// One pass over the bytes as opened: from the same reads, the stage (with `patches`
    /// applied), the backup when `backup` (the session's first Save), and their SHA-256. The
    /// handle's size, mtime, change time and inode are compared before the pass with what was
    /// opened, and after it with before: any difference and nothing is kept. Every file this
    /// pass creates is new (created exclusively, owner-only) and removed if the pass fails.
    fn pass(&mut self, patches: &BTreeMap<u64, u8>, tag: &str, backup: bool) -> Result<PassResult> {
        use sha2::Digest;
        if let Some(why) = &self.read_only {
            bail!("{why}");
        }
        // A writer at the former gap between two observations is now seen by this one.
        #[cfg(test)]
        if let Some(hook) = self.pre_pass_hook.as_mut() {
            hook();
        }
        let before = Ident::of(&self.base, &self.base_path)?;
        if before != self.opened {
            return Ok(PassResult::Changed(self.opened.change(&before)));
        }
        let (stage, mut stage_file) = super::create_keep(&self.edits, tag)?;
        let mut made = Made(vec![stage.clone()]);
        let mut backup_file = None;
        if backup {
            let (path, file) = self.new_backup()?;
            made.0.push(path.clone());
            backup_file = Some((path, file));
        }
        let mut hasher = sha2::Sha256::new();
        let mut stage_hasher = sha2::Sha256::new();
        let mut buf = vec![0u8; CHUNK];
        let len = self.size();
        let mut at = 0u64;
        let copied = (|| -> Result<()> {
            while at < len {
                let n = (len - at).min(CHUNK as u64) as usize;
                self.base
                    .read_exact_at(&mut buf[..n], at)
                    .with_context(|| format!("cannot read {}", self.base_path.display()))?;
                hasher.update(&buf[..n]);
                if let Some((path, file)) = backup_file.as_mut() {
                    file.write_all(&buf[..n])
                        .with_context(|| format!("cannot write the backup {}", path.display()))?;
                }
                for (&off, &b) in patches.range(at..at + n as u64) {
                    buf[(off - at) as usize] = b;
                }
                stage_hasher.update(&buf[..n]);
                stage_file
                    .write_all(&buf[..n])
                    .with_context(|| format!("cannot write {}", stage.display()))?;
                at += n as u64;
                #[cfg(test)]
                if let Some(hook) = self.chunk_hook.as_mut() {
                    hook(at);
                }
            }
            stage_file.flush()?;
            if let Some((_, file)) = backup_file.as_mut() {
                file.flush()?;
            }
            Ok(())
        })();
        #[cfg(test)]
        if self.fail_stat_after_pass {
            bail!("cannot stat {} (test)", self.base_path.display());
        }
        let after = Ident::of(&self.base, &self.base_path)?;
        if after != before {
            return Ok(PassResult::Changed(format!(
                "{} while it was being read",
                before.change(&after)
            )));
        }
        copied?;
        made.keep();
        Ok(PassResult::Done(Pass {
            stage,
            backup: backup_file.map(|(p, _)| p),
            sha256: hasher.finalize().into(),
            stage_sha256: stage_hasher.finalize().into(),
        }))
    }

    /// Whether the file read is the file saved — a local file opened in place (a fetched copy
    /// is only read: no upload changes it).
    fn in_place(&self) -> bool {
        self.origin.is_local() && self.base_path == Path::new(&self.origin.abs)
    }

    /// After a Save of a local file in place landed: the file now at the path is opened and
    /// becomes the file read — only if it holds exactly what was saved (`want`, the stage's
    /// SHA-256), read through its own handle, unchanged while it was read.
    fn rebind(&mut self, want: &[u8; 32]) -> Rebind {
        use sha2::Digest;
        let check = (|| -> Result<Option<Base>> {
            let base = Base::open(&self.base_path)?;
            let mut hasher = sha2::Sha256::new();
            let mut buf = vec![0u8; CHUNK];
            let mut at = 0u64;
            while at < base.opened.len {
                let n = (base.opened.len - at).min(CHUNK as u64) as usize;
                base.file.read_exact_at(&mut buf[..n], at)?;
                hasher.update(&buf[..n]);
                at += n as u64;
            }
            let same: [u8; 32] = hasher.finalize().into();
            let still = Ident::of(&base.file, &base.path)? == base.opened;
            Ok((still && &same == want && base.opened.len == self.opened.len).then_some(base))
        })();
        match check {
            Ok(Some(base)) => {
                self.base = Arc::new(base.file);
                self.opened = base.opened;
                Rebind::Rebound(self.base.clone())
            }
            Ok(None) => self.lose(format!(
                "{} no longer holds what was saved (changed right after the save?)",
                self.base_path.display()
            )),
            Err(e) => self.lose(format!(
                "{} cannot be read back after the save ({e:#})",
                self.base_path.display()
            )),
        }
    }

    fn lose(&mut self, why: String) -> Rebind {
        self.read_only = Some(why.clone());
        Rebind::Lost(why)
    }

    /// What the last Save did to the file read, once.
    pub fn take_rebind(&mut self) -> Option<Rebind> {
        self.rebind.take()
    }

    /// Why nothing more can be saved from this file, if so.
    pub fn read_only(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// The changes, when the bytes they apply to changed: `<name>.hexpatch-<stamp>.txt` among
    /// the edits (owner-only, a new file), one `offset: bytes` line per run of changed bytes
    /// under a header naming the source as it was opened.
    pub(super) fn keep_patches(&self, patches: &BTreeMap<u64, u8>, why: &str) -> Result<PathBuf> {
        let stamp = crate::cache::utc_stamp(SystemTime::now());
        let name = self
            .edits
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (path, mut file) = crate::cache::create_unique(|attempt| {
            let unique = if attempt == 0 {
                String::new()
            } else {
                format!("-{}", &crate::remote::random_hex()[..8])
            };
            self.edits
                .with_file_name(format!("{name}.hexpatch-{stamp}{unique}.txt"))
        })?;
        let text = self.list_text(patches, why);
        // Owner-only whatever the umask took from the creating call.
        use std::os::unix::fs::PermissionsExt;
        let written = file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .and_then(|()| file.write_all(text.as_bytes()));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("cannot write {}", path.display()));
        }
        Ok(path)
    }

    /// The list of edits as a `.hexpatch` file holds it: a header naming the source, then one
    /// `offset: bytes` line per run of bytes (the bytes wanted, absolute).
    pub(super) fn list_text(&self, patches: &BTreeMap<u64, u8>, why: &str) -> String {
        let mtime = self
            .opened
            .mtime
            .map_or("unknown".to_string(), crate::cache::rfc3339);
        let mut text = format!(
            "# sopen hex edits, not saved: {} changed since it was opened ({why})\n\
             # source: {}\n\
             # as opened: {} bytes, modified {mtime}\n\
             # one line per run of changed bytes — offset: bytes (hex)\n",
            self.origin.display(),
            self.base_path.display(),
            self.size()
        );
        let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
        for (&off, &b) in patches {
            match runs.last_mut() {
                Some((start, bytes)) if *start + bytes.len() as u64 == off => bytes.push(b),
                _ => runs.push((off, vec![b])),
            }
        }
        for (start, bytes) in runs {
            let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
            text.push_str(&format!("0x{start:X}: {hex}\n"));
        }
        text
    }

    /// Nothing more is saved from this file.
    pub(super) fn set_read_only(&mut self, why: &str) {
        self.read_only.get_or_insert_with(|| why.to_string());
    }

    /// The file's name, for naming a list of its edits.
    pub(super) fn name(&self) -> String {
        self.origin.parent().1
    }

    /// The refusal for bytes as opened that changed: nothing written, the changes listed.
    fn refuse_changed(&self, patches: &BTreeMap<u64, u8>, why: &str) -> Result<Saved> {
        let kept = self.keep_patches(patches, why)?;
        Ok(Saved::Refused {
            message: format!(
                "not saved: {} changed since it was opened ({why}); nothing was written — your \
                 edits are listed in {}",
                self.base_path.display(),
                kept.display()
            ),
            kept: Some(kept),
        })
    }

    /// Save (spec §5, §6a), in this order: can this server replace a file in one step (refused
    /// before anything is written if not) → one pass over the bytes as opened that writes the
    /// backup of the original (the session's first Save; printed) and the stage, refused when
    /// those bytes changed → the conflict-checked overwrite, checked against the content hash
    /// from that pass when the baseline has none (a local file opened in place). A refusal or
    /// a transport error keeps the edits in a local file whose path the message names.
    pub fn save(&mut self, patches: &BTreeMap<u64, u8>) -> Result<Saved> {
        if !self.can_replace() {
            bail!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.origin.display()
            );
        }
        let first = self.backup.is_none();
        let pass = match self.pass(patches, "edited", first) {
            Ok(PassResult::Done(pass)) => pass,
            Ok(PassResult::Changed(why)) => return self.refuse_changed(patches, &why),
            Err(e) => return Err(e.context(format!("{} was not touched", self.origin.display()))),
        };
        if let Some(path) = &pass.backup {
            println!("backup of {} at {}", self.origin.display(), path.display());
            self.backup = Some(path.clone());
        }
        let backup = self.backup.clone().expect("made by the first pass");
        let (staged, stage_sha256) = (pass.stage, pass.stage_sha256);
        // A local file opened in place has no hash from a fetch: the pass's is what was opened.
        if self.origin.at.sha256.is_none() {
            self.origin.at.sha256 = Some(pass.sha256);
        }
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
                    // A local file opened in place was just renamed over: the file read is
                    // gone from its path. A fetched copy is untouched: it stays as opened.
                    if self.in_place() {
                        self.rebind = Some(self.rebind(&stage_sha256));
                    }
                    Ok(Saved::Written {
                        remote: self.origin.display(),
                        backup,
                        notes,
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
                    Ok(Saved::Refused {
                        message,
                        kept: Some(kept),
                    })
                }
            }
        })();
        result.map_err(|e| super::image::failed(&e, &staged))
    }

    /// Save copy: `<stem>.edited.<ext>` next to the original (`-N` when taken), never replacing
    /// anything; staged by the same pass as Save (refused when the bytes as opened changed,
    /// the changes listed). Returns the printed path of the new file.
    pub fn save_copy(&mut self, patches: &BTreeMap<u64, u8>) -> Result<String> {
        let staged = match self.pass(patches, "copy", false)? {
            PassResult::Done(pass) => pass.stage,
            PassResult::Changed(why) => {
                let kept = self.keep_patches(patches, &why)?;
                bail!(
                    "no copy saved: {} changed since it was opened ({why}); nothing was written \
                     — your edits are listed in {}",
                    self.base_path.display(),
                    kept.display()
                );
            }
        };
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
            Err(e) => Err(super::image::failed(&e, &staged)),
        }
    }
}

/// Opens the hex window, or the windowless `SOPEN_AUTOSAVE` run. `notices` (where earlier
/// copies' unsaved edits went) are shown whole in the window's rows.
pub fn open(file: HexFile, hooks: &Hooks, notices: Vec<String>) -> Result<Outcome> {
    let mut doc = HexDoc::new(file.base(), file.size());
    let cursor = apply_state(&mut doc, hooks)?;
    if hooks.autosave {
        return autosave(file, &mut doc, hooks);
    }
    let title = file.origin.display();
    if !file.can_replace() {
        eprintln!("sopen: {CANNOT_REPLACE}");
    }
    let outcome: Arc<Mutex<Outcome>> = Arc::default();
    let leftover: Arc<Mutex<Option<Job>>> = Arc::default();
    let mut window = HexWindow::new(
        file,
        doc,
        cursor,
        notices,
        outcome.clone(),
        leftover.clone(),
    );
    window.ask_close = hooks.state.ask_close;
    let run = super::run_window(&title, WINDOW, window, hooks);
    finish(&leftover, &outcome);
    run?;
    let outcome = outcome.lock().unwrap().clone();
    Ok(outcome)
}

/// The hex window for a file the binary dialog opened (the same window switches to it).
pub(super) fn embedded(
    file: HexFile,
    hooks: &Hooks,
    notices: Vec<String>,
    outcome: Arc<Mutex<Outcome>>,
    leftover: Arc<Mutex<Option<Job>>>,
) -> Result<HexWindow> {
    let mut doc = HexDoc::new(file.base(), file.size());
    let cursor = apply_state(&mut doc, hooks)?;
    let mut window = HexWindow::new(file, doc, cursor, notices, outcome, leftover);
    window.ask_close = hooks.state.ask_close;
    Ok(window)
}

/// The hex window inside another window (the file browser): the window [`open`] shows, without
/// the receipt hooks.
pub fn embed(file: HexFile, notices: Vec<String>) -> super::Embedded {
    let outcome: Arc<Mutex<Outcome>> = Arc::default();
    let leftover: Arc<Mutex<Option<Job>>> = Arc::default();
    let doc = HexDoc::new(file.base(), file.size());
    let window = HexWindow::new(
        file,
        doc,
        Cursor::default(),
        notices,
        outcome.clone(),
        leftover.clone(),
    );
    super::Embedded {
        window: Box::new(window),
        settle: Box::new(move |wait| settle(&leftover, &outcome, wait)),
    }
}

/// A save the window left running when it went away, as [`finish`] waits for it: with `wait`
/// waited for, else taken only once it has finished; printed and counted either way. The
/// window's outcome once none runs.
fn settle(leftover: &Mutex<Option<Job>>, outcome: &Mutex<Outcome>, wait: bool) -> Option<Outcome> {
    if wait {
        finish(leftover, outcome);
    } else {
        let mut slot = leftover.lock().unwrap();
        if let Some(job) = slot.as_ref() {
            let result = match job.rx.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(anyhow!("the save stopped without a result"))
                }
            };
            *slot = None;
            let _ = report(&result, &mut outcome.lock().unwrap());
        }
    }
    Some(outcome.lock().unwrap().clone())
}

/// Never exit with a write in flight without its outcome on the terminal: a save the window
/// handed over when it closed is waited for here.
pub(super) fn finish(leftover: &Mutex<Option<Job>>, outcome: &Mutex<Outcome>) {
    if let Some(job) = leftover.lock().unwrap().take() {
        eprintln!("sopen: the window closed during a save; waiting for it to finish");
        let result = job
            .rx
            .recv()
            .unwrap_or_else(|_| Err(anyhow!("the save stopped without a result")));
        let _ = report(&result, &mut outcome.lock().unwrap());
    }
}

fn autosave(mut file: HexFile, doc: &mut HexDoc, hooks: &Hooks) -> Result<Outcome> {
    let mut outcome = Outcome::default();
    if !doc.dirty() {
        println!("no changes; {} untouched", file.origin.display());
        return Ok(outcome);
    }
    if let Some(delay) = hooks.save_delay {
        eprintln!("waiting {} ms before saving", delay.as_millis());
        std::thread::sleep(delay);
    }
    match file.save(doc.patches())? {
        Saved::Written { remote, notes, .. } => {
            outcome.saved = true;
            let n = doc.changed_count();
            println!("saved {remote} ({} changed)", plural_bytes(n as u64));
            for note in notes {
                eprintln!("sopen: {note}");
            }
            if let Some(Rebind::Lost(why)) = file.take_rebind() {
                eprintln!("sopen: {why}");
            }
        }
        Saved::Refused { message, .. } => {
            outcome.conflict = true;
            outcome.refused = Some(message);
        }
    }
    Ok(outcome)
}

fn plural_bytes(n: u64) -> String {
    if n == 1 {
        "1 byte".to_string()
    } else {
        format!("{} bytes", grouped(n))
    }
}

// ---------------------------------------------------------------------------------------------
// The window.

/// Prints a finished save to the terminal, records it in `outcome` and returns the window's
/// one-line status and what must be shown whole (the disclosure rows). Also used for a save
/// still in flight when the window went away.
fn report(result: &Result<JobDone>, outcome: &mut Outcome) -> (Status, Vec<String>) {
    match result {
        Ok(JobDone::Saved(Saved::Written {
            remote,
            backup,
            notes,
        })) => {
            outcome.saved = true;
            outcome.refused = None;
            println!("saved {remote}");
            for note in notes {
                eprintln!("sopen: {note}");
            }
            let status = Status::info(format!(
                "saved {remote} · original backed up at {}",
                backup.display()
            ));
            (status, notes.clone())
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
        Ok(JobDone::Copy(path)) => {
            println!("saved a copy at {path}");
            (Status::info(format!("saved a copy at {path}")), Vec::new())
        }
        Err(e) => {
            eprintln!("sopen: {e:#}");
            (Status::error("save failed"), vec![format!("{e:#}")])
        }
    }
}

/// Closing was asked for while a save runs.
const CLOSES_AFTER_SAVE: &str = "the window closes when the save is done";

enum JobKind {
    Save { close_after: bool },
    SaveCopy { close_after: bool },
}

pub(super) struct Job {
    kind: JobKind,
    /// The changes the save writes (what the file holds if it lands).
    snapshot: BTreeMap<u64, u8>,
    rx: Receiver<Result<JobDone>>,
}

enum JobDone {
    Saved(Saved),
    /// The copy's printed path.
    Copy(String),
}

/// Where the grid was last laid out: the rows' rectangle and the character metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Layout {
    rows: Rect,
    row_h: f32,
    cw: f32,
    hex_x: f32,
    ascii_x: f32,
}

pub(super) struct HexWindow {
    /// `host:/abs`.
    title: String,
    /// False on a server without `posix-rename@openssh.com`: Save is off, Save copy works.
    can_replace: bool,
    doc: HexDoc,
    file: Arc<Mutex<HexFile>>,
    cursor: Cursor,
    /// The first row on screen.
    top_row: u64,
    /// Rows that fit on screen (the last pass's).
    visible: u64,
    /// The cursor moved by a key, a click or an edit: scroll it into view on the next pass.
    follow: bool,
    /// Wheel points not yet a whole row (trackpads scroll in points).
    wheel_rest: f32,
    /// The Go to field's text.
    goto: String,
    status: Status,
    job: Option<Job>,
    outcome: Arc<Mutex<Outcome>>,
    prompt: bool,
    close_ok: bool,
    /// Close was asked for while a save ran: close when it is done.
    close_when_done: bool,
    /// Where a save still in flight goes if the window is dropped anyway; its opener waits.
    leftover: Arc<Mutex<Option<Job>>>,
    painted: bool,
    /// The grid as last laid out, and how many rows the last pass painted.
    layout: Option<Layout>,
    rows_painted: u64,
    /// Why the window is read-only (the file saved in place changed right after, or cannot be
    /// read back), and where the edits are kept — the line shown whole in the rows.
    read_only: Option<String>,
    /// The reason alone, for the header of a list of the edits.
    read_only_why: String,
    /// Where the list of the edits was written, once it was.
    retained: Option<PathBuf>,
    /// Where "Write the list there" writes the list of edits.
    pub(super) keep_to: String,
    /// Run before each link that writing the list makes (tests: something put in the private
    /// folder, so its removal fails and is said).
    #[cfg(test)]
    pub(super) keep_link: Option<KeepLink>,
    /// Lines from open that must be read whole and stay (see [`super::disclosures`]).
    opened: Vec<String>,
    /// What the last write reported that must be read whole, and the next such report, held
    /// while the button is down.
    notices: Vec<String>,
    pending_notices: Option<Vec<String>>,
    /// `SOPEN_STATE` `close=ask`: closing is asked for on the first pass.
    ask_close: bool,
}

impl Drop for HexWindow {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            *self.leftover.lock().unwrap() = Some(job);
        }
    }
}

fn grid_id() -> egui::Id {
    egui::Id::unique("sopen-hex-grid")
}

fn goto_id() -> egui::Id {
    egui::Id::unique("sopen-hex-goto")
}

/// A stand-in run before each link of writing the list (tests).
#[cfg(test)]
type KeepLink = Box<dyn FnMut(super::binary::LinkStep, &Path) -> std::io::Result<()> + Send>;

/// The read-only window's "Write the list there" path field.
fn keep_id() -> egui::Id {
    egui::Id::unique("sopen-hex-keep-to")
}

impl HexWindow {
    fn new(
        file: HexFile,
        doc: HexDoc,
        cursor: Cursor,
        notices: Vec<String>,
        outcome: Arc<Mutex<Outcome>>,
        leftover: Arc<Mutex<Option<Job>>>,
    ) -> Self {
        let can_replace = file.can_replace();
        let opened = notices
            .into_iter()
            .chain((!can_replace).then(|| CANNOT_REPLACE.to_string()))
            .collect();
        Self {
            title: file.origin.display(),
            can_replace,
            doc,
            file: Arc::new(Mutex::new(file)),
            cursor,
            top_row: 0,
            visible: 1,
            follow: true,
            wheel_rest: 0.0,
            goto: String::new(),
            status: Status::default(),
            job: None,
            outcome,
            prompt: false,
            close_ok: false,
            close_when_done: false,
            leftover,
            painted: false,
            layout: None,
            rows_painted: 0,
            read_only: None,
            read_only_why: String::new(),
            retained: None,
            keep_to: String::new(),
            #[cfg(test)]
            keep_link: None,
            opened,
            notices: Vec::new(),
            pending_notices: None,
            ask_close: false,
        }
    }

    fn last(&self) -> Option<u64> {
        self.doc.size().checked_sub(1)
    }

    /// Puts the cursor on `at` (clamped to the file) in `pane`, at the byte's high nibble.
    fn place(&mut self, at: u64, pane: Pane) {
        let Some(last) = self.last() else {
            return;
        };
        self.cursor = Cursor {
            at: at.min(last),
            pane,
            low: false,
        };
        self.follow = true;
    }

    /// Moves the cursor by `delta` bytes, staying in the file.
    fn step(&mut self, delta: i64) {
        self.place(
            self.cursor.at.saturating_add_signed(delta),
            self.cursor.pane,
        );
    }

    fn undo(&mut self) {
        if let Some(cursor) = self.doc.undo() {
            self.cursor = cursor;
            self.follow = true;
            self.status = Status::info("undone");
        }
    }

    fn redo(&mut self) {
        if let Some(cursor) = self.doc.redo() {
            self.cursor = cursor;
            self.follow = true;
            self.status = Status::info("redone");
        }
    }

    fn goto(&mut self) {
        let size = self.doc.size();
        match parse_offset(&self.goto) {
            Ok(at) if at < size => {
                self.place(at, self.cursor.pane);
                self.status = Status::info(format!("at 0x{at:X} ({at})"));
            }
            Ok(at) => {
                self.status = Status::error(format!(
                    "0x{at:X} is past the end of the file ({} bytes)",
                    grouped(size)
                ))
            }
            Err(e) => self.status = Status::error(format!("{e:#}")),
        }
    }

    fn start(&mut self, ctx: &egui::Context, kind: JobKind) {
        if self.job.is_some() {
            return;
        }
        if let Some(why) = &self.read_only {
            return self.status = Status::error(why.clone());
        }
        if matches!(kind, JobKind::Save { .. }) {
            if !self.can_replace {
                return self.status = Status::error(CANNOT_REPLACE);
            }
            if !self.doc.dirty() {
                return self.status = Status::info("no changes to save");
            }
        }
        let patches = self.doc.patches().clone();
        let snapshot = patches.clone();
        let file = self.file.clone();
        let copy = matches!(kind, JobKind::SaveCopy { .. });
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            // A save whose worker died holding the file leaves it as it was: usable.
            let mut file = file.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let result = if copy {
                file.save_copy(&patches).map(JobDone::Copy)
            } else {
                file.save(&patches).map(JobDone::Saved)
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
        self.job = Some(Job { kind, snapshot, rx });
    }

    /// A finished save, taken after this frame's input (an edit or an undo in the same frame is
    /// in the comparison, never lost to a close). Closing — asked for in the prompt, or during
    /// the save — happens only when the save landed with nothing to read and nothing is unsaved;
    /// a save that did not land, or landed with notes, keeps the window open; newer edits ask.
    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else {
            return;
        };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(anyhow!(
                "the save stopped without a result (its worker ended); your edits are still \
                 here"
            )),
        };
        let job = self.job.take().expect("checked above");
        let (status, shown) = report(&result, &mut self.outcome.lock().unwrap());
        self.status = status;
        // An edit made while the save ran is not in what it wrote.
        let newer = *self.doc.patches() != job.snapshot;
        let landed = match &result {
            Ok(JobDone::Saved(Saved::Written { .. })) => {
                self.doc.mark_saved(job.snapshot.clone());
                let rebind = self
                    .file
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take_rebind();
                match rebind {
                    Some(Rebind::Rebound(file)) => {
                        if let Err(e) = self.doc.rebase(file) {
                            self.turn_read_only(&format!("{e:#}"));
                        }
                    }
                    Some(Rebind::Lost(why)) => self.turn_read_only(&why),
                    None => {}
                }
                true
            }
            Ok(JobDone::Copy(..)) => {
                self.doc.mark_copied(job.snapshot.clone());
                true
            }
            _ => false,
        };
        self.pending_notices = Some(shown);
        let notes = matches!(&result, Ok(JobDone::Saved(Saved::Written { notes, .. })) if !notes.is_empty())
            || self.read_only.is_some();
        // A close asked for during this save is used up by it, whatever it reports.
        let waited = std::mem::take(&mut self.close_when_done);
        let asked = waited
            || matches!(
                job.kind,
                JobKind::Save { close_after: true } | JobKind::SaveCopy { close_after: true }
            );
        if !asked || !landed {
            return;
        }
        if (newer || self.doc.unsaved()) && self.read_only.is_none() {
            if newer {
                self.status =
                    Status::info(format!("newer edits are not saved · {}", self.status.text));
            }
            self.prompt = true;
        } else if notes {
            self.status = Status::info(format!(
                "not closed: read the notes below · {}",
                self.status.text
            ));
        } else {
            self.close_ok = true;
            super::request_close(ctx);
        }
    }

    /// Whether input may change anything: not under the close prompt, not once closing is
    /// decided, not once the window is read-only.
    fn locked(&self) -> bool {
        self.prompt || self.close_ok || self.read_only.is_some()
    }

    /// Nothing more is edited or saved here (the file saved in place no longer holds what was
    /// saved, or cannot be read back): the edits are listed in a file of their own — or, when
    /// that fails, the window says so and offers to try again or to write the list elsewhere,
    /// and closing asks while they are unsaved.
    fn turn_read_only(&mut self, why: &str) {
        self.file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_read_only(why);
        self.read_only_why = why.to_string();
        if self.keep_to.is_empty() {
            let name = self
                .file
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .name();
            let dir = super::binary::download_dir();
            self.keep_to = super::binary::tilde(&dir.join(format!("{name}.hexpatch.txt")));
        }
        self.read_only = Some(format!("read-only now: {why}"));
        self.keep_edits();
    }

    /// Lists the edits among sopen's kept edits ("Keep edits…").
    fn keep_edits(&mut self) {
        let listed = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keep_patches(self.doc.patches(), &self.read_only_why);
        self.kept(
            listed
                .map(|p| (p, Vec::new()))
                .map_err(|e| format!("{e:#}")),
        );
    }

    /// Writes the list of the edits to the path in the field, as Save to… writes: a new file,
    /// never over anything ("Write the list there").
    fn keep_edits_at(&mut self) {
        let mut dest = super::binary::untilde(&self.keep_to);
        if dest.is_dir() {
            let name = self
                .file
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .name();
            dest = dest.join(format!("{name}.hexpatch.txt"));
        }
        let text = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .list_text(self.doc.patches(), &self.read_only_why);
        let fill = |f: &mut File| -> Result<u64> {
            f.write_all(text.as_bytes())?;
            Ok(text.len() as u64)
        };
        #[cfg(test)]
        let saved = match self.keep_link.as_mut() {
            Some(link) => super::binary::save_new_with(&dest, fill, link.as_mut()),
            None => super::binary::save_new(&dest, fill),
        };
        #[cfg(not(test))]
        let saved = super::binary::save_new(&dest, fill);
        let result = match saved {
            Ok(saved) => Ok((dest, saved.notes)),
            Err(super::binary::SaveError::Exists { next }) => {
                self.keep_to = super::binary::tilde(&next);
                Err(format!(
                    "{} exists — not replaced; the field now holds the next free name",
                    dest.display()
                ))
            }
            Err(e) => Err(e.to_string()),
        };
        self.kept(result);
    }

    /// Says where the edits are kept now, or why they are not.
    fn kept(&mut self, result: std::result::Result<(PathBuf, Vec<String>), String>) {
        let why = &self.read_only_why;
        let line = match result {
            Ok((path, notes)) => {
                let line = super::with_notes(
                    &format!(
                        "read-only now: {why}; your edits are listed in {}",
                        path.display()
                    ),
                    &notes,
                );
                self.retained = Some(path);
                line
            }
            Err(e) => format!(
                "read-only now: {why}; your edits could not be listed ({e}) — Keep edits… tries \
                 again, or write the list to a path of your choice"
            ),
        };
        eprintln!("sopen: {line}");
        self.read_only = Some(line);
    }

    /// The row under the read-only line while the edits are not kept anywhere.
    fn keep_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .button("Keep edits…")
                .on_hover_text("writes the list of your edits among sopen's kept edits")
                .clicked()
            {
                self.keep_edits();
            }
            let room = (ui.available_width() - 170.0).max(120.0);
            ui.add(
                chrome::field(&mut self.keep_to)
                    .id(keep_id())
                    .desired_width(room),
            );
            if ui
                .button("Write the list there")
                .on_hover_text("a new file at that path, never over an existing one")
                .clicked()
            {
                self.keep_edits_at();
            }
        });
    }

    /// Keys for the whole window. `goto_owns` = the Go to field has this frame's typing (it is
    /// focused, was clicked, lost focus on Enter, or Cmd/Ctrl+G gave it the keyboard): then no
    /// byte is typed and no key moves the cursor.
    fn keys(&mut self, ctx: &egui::Context, goto_owns: bool) {
        use super::command_key;
        if self.locked() {
            return;
        }
        if command_key(ctx, Key::S, true) {
            self.start(ctx, JobKind::SaveCopy { close_after: false });
        } else if command_key(ctx, Key::S, false) {
            self.start(ctx, JobKind::Save { close_after: false });
        }
        if goto_owns {
            return;
        }
        // F5 is PC Shell's Save, as Cmd/Ctrl+S — but a plain key: never while a field has
        // the typing.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F5)) {
            self.start(ctx, JobKind::Save { close_after: false });
        }
        if command_key(ctx, Key::Z, true) {
            self.redo();
        } else if command_key(ctx, Key::Z, false) {
            self.undo();
        }
        let Some(last) = self.last() else {
            return;
        };
        let page = (self.visible.max(1) * ROW) as i64;
        let row_start = self.cursor.at - self.cursor.at % ROW;
        let mods = egui::Modifiers::NONE;
        let (home, end) = (
            command_key(ctx, Key::Home, false),
            command_key(ctx, Key::End, false),
        );
        if home {
            self.place(0, self.cursor.pane);
        }
        if end {
            self.place(last, self.cursor.pane);
        }
        let pressed = |key: Key| ctx.input_mut(|i| i.consume_key(mods, key));
        if pressed(Key::ArrowLeft) {
            self.step(-1);
        }
        if pressed(Key::ArrowRight) {
            self.step(1);
        }
        if pressed(Key::ArrowUp) {
            self.step(-(ROW as i64));
        }
        if pressed(Key::ArrowDown) {
            self.step(ROW as i64);
        }
        if pressed(Key::PageUp) {
            self.step(-page);
        }
        if pressed(Key::PageDown) {
            self.step(page);
        }
        if pressed(Key::Home) {
            self.place(row_start, self.cursor.pane);
        }
        if pressed(Key::End) {
            self.place(row_start + ROW - 1, self.cursor.pane);
        }
        // F8 is PC Shell's Asc/hx: the typing column, as Tab.
        if pressed(Key::Tab) || pressed(Key::F8) {
            let pane = match self.cursor.pane {
                Pane::Hex => Pane::Ascii,
                Pane::Ascii => Pane::Hex,
            };
            self.place(self.cursor.at, pane);
        }
        let typed: Vec<char> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) => Some(t.chars().collect::<Vec<_>>()),
                    _ => None,
                })
                .flatten()
                .collect()
        });
        for c in typed {
            match self.doc.type_char(self.cursor, c) {
                Some(next) => {
                    self.cursor = next;
                    self.follow = true;
                }
                None if self.cursor.pane == Pane::Ascii && !c.is_control() => {
                    self.status = Status::error(format!(
                        "'{c}' is not printable ASCII; the ASCII column takes ' ' to '~'"
                    ));
                }
                None => {}
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let ctx = ui.ctx().clone();
        let busy = self.job.is_some();
        let free = !self.locked();
        // One row, whatever the state: nothing in it comes, goes or changes its label.
        ui.horizontal(|ui| {
            ui.label("Go to");
            let field = ui
                .add(
                    chrome::field(&mut self.goto)
                        .id(goto_id())
                        .hint_text("0x… or decimal")
                        .desired_width(130.0),
                )
                .on_hover_text("⌘/Ctrl+G · Enter goes there");
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.goto();
            }
            ui.separator();
            if ui
                .add_enabled(free && self.doc.can_undo(), egui::Button::new("Undo"))
                .on_hover_text("⌘/Ctrl+Z")
                .clicked()
            {
                self.undo();
            }
            if ui
                .add_enabled(free && self.doc.can_redo(), egui::Button::new("Redo"))
                .on_hover_text("⇧⌘/Shift+Ctrl+Z")
                .clicked()
            {
                self.redo();
            }
            ui.separator();
            let save = ui
                .add_enabled(
                    free && !busy && self.doc.dirty() && self.can_replace,
                    egui::Button::new("Save"),
                )
                .on_hover_text("⌘/Ctrl+S — overwrites the original after a local backup")
                .on_disabled_hover_text(if !self.can_replace {
                    CANNOT_REPLACE
                } else if busy {
                    "a save is running"
                } else {
                    "no changes to save"
                });
            if save.clicked() {
                self.start(&ctx, JobKind::Save { close_after: false });
            }
            let copy = ui
                .add_enabled(free && !busy, egui::Button::new("Save copy"))
                .on_hover_text("⇧⌘/Shift+Ctrl+S — a new <name>.edited file next to the original")
                .on_disabled_hover_text("a save is running");
            if copy.clicked() {
                self.start(&ctx, JobKind::SaveCopy { close_after: false });
            }
            field
        })
        .inner
    }

    /// The cursor's offset and byte, as the status bar shows them.
    pub(super) fn cursor_line(&mut self) -> String {
        if self.doc.size() == 0 {
            return "empty file".to_string();
        }
        let at = self.cursor.at;
        let byte = match self.doc.byte(at) {
            Some(b @ 0x20..=0x7e) => format!("0x{b:02X} {b} '{}'", b as char),
            Some(b) => format!("0x{b:02X} {b}"),
            None => "unreadable".to_string(),
        };
        format!("0x{at:X} ({at}) · {byte}")
    }

    /// One line whatever it says (see [`super::one_line`]): the grid above must not move.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let cursor = self.cursor_line();
        let size = self.doc.size();
        let changed = self.doc.changed_count() as u64;
        let unsaved = self.doc.unsaved();
        if let Some(e) = self.doc.read_error() {
            if !self.status.error {
                self.status = Status::error(e.to_string());
            }
        }
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(cursor).monospace()).extend());
            ui.separator();
            ui.add(egui::Label::new(crate::human(size)).extend())
                .on_hover_text(format!("{} bytes", grouped(size)));
            ui.separator();
            ui.add(egui::Label::new(format!("{} changed", plural_bytes(changed))).extend());
            if unsaved {
                ui.separator();
                ui.add(egui::Label::new("unsaved").extend());
            }
            if self.job.is_some() || !self.status.text.is_empty() {
                ui.separator();
            }
            if self.job.is_some() {
                // No taller than the status text beside it, so the bar keeps its height.
                let size = ui.text_style_height(&egui::TextStyle::Body);
                ui.add(egui::Spinner::new().size(size));
            }
            self.status.show(ui);
        });
    }

    /// The window, as PC Shell's File Edit: its frame, the file on its first line, then the
    /// rows — offsets, hex, ASCII, the cursor in both columns, changed bytes marked — and the
    /// scroll bar on its right border. Only the rows on screen are read and painted.
    fn grid(&mut self, ui: &mut egui::Ui, goto_focused: bool) {
        let ctx = ui.ctx().clone();
        let outer = ui.max_rect();
        let p = chrome::Palette::of(ui.visuals());
        let panel = chrome::frame(&ui.painter_at(outer), outer, TITLE, true);
        let font = chrome::mono(ui);
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        // Rows touch, a cell apart, as on a text screen.
        let row_h = text_h.ceil();
        let bar = Rect::from_min_max(
            Pos2::new(panel.right(), panel.top()),
            Pos2::new(panel.right() + chrome::CELL.x, panel.bottom()),
        );
        // The file's name on the first line, a blank line, then the rows.
        let rows_rect = Rect::from_min_max(
            Pos2::new(
                panel.left(),
                (panel.top() + 2.0 * row_h).min(panel.bottom()),
            ),
            panel.max,
        );
        let size = self.doc.size();
        let total = self.doc.rows();
        let visible = ((rows_rect.height() / row_h).floor() as u64).max(1);
        self.visible = visible;
        let max_top = total.saturating_sub(visible);

        let resp = ui.interact(rows_rect, grid_id(), Sense::click_and_drag());
        let bar_resp = ui.interact(bar, grid_id().with("bar"), Sense::click_and_drag());
        // The grid holds the keyboard (arrows and Tab act on the cursor, not on focus), except
        // while a field (Go to, the read-only path field) or the close prompt has it.
        if !goto_focused && !self.prompt {
            if ctx.memory(|m| m.focused()) != Some(grid_id()) {
                resp.request_focus();
            }
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    grid_id(),
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        // Wheel / trackpad: whole rows; a line is three.
        if resp.hovered() || bar_resp.hovered() {
            let mut rows = 0f32;
            ctx.input(|i| {
                for event in &i.events {
                    if let egui::Event::MouseWheel { unit, delta, .. } = event {
                        rows -= match unit {
                            egui::MouseWheelUnit::Line => delta.y * 3.0,
                            egui::MouseWheelUnit::Page => delta.y * visible as f32,
                            egui::MouseWheelUnit::Point => delta.y / row_h,
                        };
                    }
                }
            });
            if rows != 0.0 {
                let rows = rows + self.wheel_rest;
                let whole = rows.trunc();
                self.wheel_rest = rows - whole;
                let moved = whole as i64;
                self.top_row = if moved < 0 {
                    self.top_row.saturating_sub(moved.unsigned_abs())
                } else {
                    self.top_row.saturating_add(moved as u64)
                };
            }
        }
        // The scroll bar: the thumb (a cell, between the arrows) follows the pointer while the
        // button is down on the bar.
        if bar_resp.is_pointer_button_down_on() && max_top > 0 {
            if let Some(at) = bar_resp.interact_pointer_pos() {
                let cell = chrome::CELL.y;
                let room = (bar.height() - 3.0 * cell).max(1.0);
                let frac = ((at.y - bar.top() - 1.5 * cell) / room).clamp(0.0, 1.0);
                self.top_row = (f64::from(frac) * max_top as f64).round() as u64;
            }
        }

        let offset_chars = offset_text(0, size).chars().count();
        let hex_x = rows_rect.left() + cw * (offset_chars + 1) as f32;
        let ascii_x = hex_x + cw * (HEX_CHARS + 2) as f32;
        // A click places the cursor on the byte under it, in that column.
        if resp.clicked() || resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let row = self.top_row + ((p.y - rows_rect.top()) / row_h).max(0.0) as u64;
                let hit = if p.x >= ascii_x - cw {
                    let i = ((p.x - ascii_x) / cw).max(0.0) as u64;
                    Some((i.min(ROW - 1), Pane::Ascii))
                } else if p.x >= hex_x - cw / 2.0 {
                    let c = ((p.x - hex_x) / cw).max(0.0) as u64;
                    let i = if c < 24 {
                        c / 3
                    } else {
                        8 + c.saturating_sub(25) / 3
                    };
                    Some((i.min(ROW - 1), Pane::Hex))
                } else {
                    None
                };
                if let Some((i, pane)) = hit {
                    self.place(row * ROW + i, pane);
                }
            }
        }
        if std::mem::take(&mut self.follow) && size > 0 {
            let row = self.cursor.at / ROW;
            if row < self.top_row {
                self.top_row = row;
            } else if row >= self.top_row + visible {
                self.top_row = row + 1 - visible;
            }
        }
        self.top_row = self.top_row.min(max_top);

        let visuals = ui.visuals().clone();
        // The rows never run under the scroll bar (on a narrow window the ASCII column is cut).
        let painter = ui.painter_at(panel);
        let title = chrome::fit_left(&ctx, &self.title, &font, panel.width());
        painter.text(
            panel.min,
            egui::Align2::LEFT_TOP,
            title,
            font.clone(),
            p.text,
        );
        if size == 0 {
            painter.text(
                rows_rect.center(),
                egui::Align2::CENTER_CENTER,
                "empty file — nothing to show",
                font.clone(),
                p.dim,
            );
        }
        let text = p.text;
        let (changed_fg, changed_bg) = (p.changed, p.changed_back);
        let cursor_bg = visuals.selection.bg_fill;
        let cursor_line = egui::Stroke::new(1.0, visuals.selection.stroke.color);
        let rows = ((rows_rect.height() / row_h).ceil() as u64).min(total - self.top_row);
        for k in 0..rows {
            let row = self.top_row + k;
            let y = rows_rect.top() + k as f32 * row_h;
            let off = row * ROW;
            let n = (size - off).min(ROW);
            painter.text(
                Pos2::new(rows_rect.left(), y),
                egui::Align2::LEFT_TOP,
                offset_text(off, size),
                font.clone(),
                p.dim,
            );
            let cell = |x: f32, chars: f32| {
                Rect::from_min_size(
                    Pos2::new(x - 1.0, y - 1.0),
                    Vec2::new(cw * chars + 2.0, row_h),
                )
            };
            let mut bytes = Vec::with_capacity(n as usize);
            let mut hex_job = egui::text::LayoutJob::default();
            let mut ascii_job = egui::text::LayoutJob::default();
            let fmt = |color: Color32| egui::TextFormat::simple(font.clone(), color);
            for i in 0..n {
                let at = off + i;
                let b = self.doc.byte(at);
                bytes.push(b);
                let changed = self.doc.changed(at);
                let color = if at == self.cursor.at {
                    p.bar_text
                } else if changed {
                    changed_fg
                } else {
                    text
                };
                let hx = hex_x + cw * hex_col(i) as f32;
                let ax = ascii_x + cw * i as f32;
                if changed {
                    painter.rect_filled(cell(hx, 2.0), 0.0, changed_bg);
                    painter.rect_filled(cell(ax, 1.0), 0.0, changed_bg);
                }
                if at == self.cursor.at {
                    let (active, other) = match self.cursor.pane {
                        Pane::Hex => (cell(hx, 2.0), cell(ax, 1.0)),
                        Pane::Ascii => (cell(ax, 1.0), cell(hx, 2.0)),
                    };
                    painter.rect_filled(active, 0.0, cursor_bg);
                    painter.rect_stroke(other, 0.0, cursor_line, egui::StrokeKind::Inside);
                    if self.cursor.pane == Pane::Hex && self.cursor.low {
                        // The next digit goes into the low nibble: underline it.
                        let x = hx + cw;
                        painter.line_segment(
                            [
                                Pos2::new(x, y + row_h - 3.0),
                                Pos2::new(x + cw, y + row_h - 3.0),
                            ],
                            egui::Stroke::new(2.0, text),
                        );
                    }
                }
                if i > 0 {
                    hex_job.append(if i == 8 { "  " } else { " " }, 0.0, fmt(text));
                }
                let digits_text = match b {
                    Some(b) => format!("{b:02X}"),
                    None => "??".to_string(),
                };
                hex_job.append(&digits_text, 0.0, fmt(color));
                ascii_job.append(&ascii_char(b).to_string(), 0.0, fmt(color));
            }
            let hex_galley = ctx.fonts_mut(|f| f.layout_job(hex_job));
            let ascii_galley = ctx.fonts_mut(|f| f.layout_job(ascii_job));
            painter.galley(Pos2::new(hex_x, y), hex_galley, text);
            painter.galley(Pos2::new(ascii_x, y), ascii_galley, text);
        }
        self.rows_painted = rows;
        chrome::scroll_bar(
            &ui.painter_at(outer),
            bar,
            (max_top > 0).then(|| (self.top_row as f64 / max_top as f64) as f32),
        );
        self.layout = Some(Layout {
            rows: rows_rect,
            row_h,
            cw,
            hex_x,
            ascii_x,
        });
        self.painted = true;
    }

    fn close_prompt(&mut self, ctx: &egui::Context) {
        if !self.prompt {
            return;
        }
        let id = egui::Id::unique("sopen-hex-unsaved");
        let modal = super::chrome::dialog(ctx, id, "Unsaved changes", |ui| {
            ui.set_width(360.0);
            let read_only = self.read_only.is_some();
            ui.label(if read_only {
                format!(
                    "Your edits to {} are kept nowhere and cannot be saved here; close anyway?",
                    self.title
                )
            } else {
                format!("Save the changes to {} before closing?", self.title)
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if read_only {
                } else if self.can_replace && ui.button("Save").clicked() {
                    self.prompt = false;
                    self.start(ctx, JobKind::Save { close_after: true });
                }
                if !read_only && !self.can_replace && ui.button("Save copy").clicked() {
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

impl super::Window for HexWindow {
    /// One pass, in this order: Cmd/Ctrl+G (the Go to field takes the keyboard before it is
    /// laid out, so this frame's typing goes there), the toolbar, which then knows whether the
    /// field has this frame's typing, the keys and typing (to exactly one place: the field or
    /// the bytes), the panels and the grid, and last a finished save — after every input of
    /// this frame, so an edit made in the same frame counts.
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if std::mem::take(&mut self.ask_close) {
            // `SOPEN_STATE` `close=ask`: as if closing were asked for (the prompt's receipt).
            self.close_requested();
        }
        // F6 is PC Shell's Goto, as Cmd/Ctrl+G — but a plain key: not while a field (Go to,
        // the read-only path field) has the typing.
        let cmd_g = !self.locked()
            && (super::command_key(&ctx, Key::G, false)
                || (!ctx.text_edit_focused()
                    && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F6))));
        if cmd_g {
            self.goto.clear();
            ctx.memory_mut(|m| m.request_focus(goto_id()));
        }
        let field = egui::Panel::top("hexbar")
            .show(ui, |ui| {
                ui.add_space(3.0);
                let field = self.toolbar(ui);
                ui.add_space(1.0);
                field
            })
            .inner;
        let goto_owns = cmd_g || field.has_focus() || field.lost_focus() || field.clicked();
        // The F-key row, the window's last (on its own: the browser around it has the row).
        super::own_keys(ui, HEX_KEYS);
        // The read-only window's path field (laid out below, with the rows) owns the typing of
        // a frame it had the keyboard in, as the Go to field does.
        let keep_had = ctx.memory(|m| m.focused()) == Some(keep_id());
        self.keys(&ctx, goto_owns || keep_had);
        // A report that lands while the button is down waits until it is let go. Its rows sit
        // below the grid: they take rows off its bottom, never move its top or its rows.
        if !ctx.input(|i| i.pointer.primary_down()) {
            if let Some(next) = self.pending_notices.take() {
                self.notices = next;
            }
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(2.0);
            self.status_bar(ui);
            ui.add_space(2.0);
        });
        let error = ui.visuals().error_fg_color;
        let lines: Vec<(String, Color32)> = self
            .opened
            .iter()
            .chain(&self.read_only)
            .chain(&self.notices)
            .map(|n| (n.clone(), error))
            .collect();
        let keep_row = self.read_only.is_some() && self.retained.is_none();
        if !lines.is_empty() || keep_row {
            egui::Panel::bottom("disclosures").show(ui, |ui| {
                ui.add_space(2.0);
                super::disclosures(ui, &lines);
                if keep_row {
                    self.keep_row(ui);
                }
                ui.add_space(2.0);
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                // A field that took the keyboard this frame (a click into the path field) keeps
                // it: the grid does not take it back.
                let field_owns = goto_owns || ctx.memory(|m| m.focused()) == Some(keep_id());
                self.grid(ui, field_owns)
            });
        self.poll_job(&ctx);
        self.close_prompt(&ctx);
    }

    fn ready(&self) -> bool {
        self.painted && self.job.is_none()
    }

    fn keys(&self) -> &'static [chrome::FKey] {
        HEX_KEYS
    }

    fn close_requested(&mut self) -> bool {
        if self.close_ok {
            return true;
        }
        // Read-only: nothing can be saved; closing asks only while the edits are kept nowhere.
        if self.read_only.is_some() && self.job.is_none() {
            if self.retained.is_some() || !self.doc.unsaved() {
                return true;
            }
            self.prompt = true;
            return false;
        }
        if self.job.is_some() {
            // Never close mid-write: the window closes once the save has reported.
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

#[cfg(test)]
mod tests;
