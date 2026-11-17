//! The hex window (spec §6a): any file as rows of 16 bytes — offset, hex, ASCII — read through a
//! small page cache, so only the rows on screen are ever read and a file of any size scrolls
//! the same. Editing overwrites bytes only (the size never changes); Undo/Redo; Save (backup
//! first, conflict-checked overwrite) and Save copy (a new sibling), as in the image window.
//!
//! [`HexDoc`] is the edit model (the bytes as opened plus a sparse map of changed bytes) and
//! [`HexFile`] the write side; both are window-free, so the tests and the `SOPEN_AUTOSAVE` hook
//! run the exact code the window's buttons run.

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
/// Width of the scroll bar at the grid's right edge.
const SCROLLBAR: f32 = 12.0;

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

/// The ASCII column's character for a byte: itself when printable, else `.`.
pub fn ascii_char(b: Option<u8>) -> char {
    match b {
        Some(b @ 0x20..=0x7e) => b as char,
        _ => '.',
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

/// One change, undone by writing `before` back (`None` = the original byte).
#[derive(Debug, Clone, Copy)]
struct Step {
    at: u64,
    before: Option<u8>,
    after: Option<u8>,
    /// Where the cursor was when the change was made (undo puts it back there).
    cursor: Cursor,
}

/// The bytes as opened (never written: read through [`Pages`]) and the changes made to them, a
/// sparse map offset → byte that holds only bytes that differ from the original.
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
    /// The changes the original file holds now (empty until a Save lands), and the ones the
    /// last Save copy holds.
    saved: BTreeMap<u64, u8>,
    copied: Option<BTreeMap<u64, u8>>,
}

impl HexDoc {
    /// The document over `file`, whose first `size` bytes are shown.
    pub fn new(file: Arc<File>, size: u64) -> Self {
        Self {
            pages: Pages {
                file,
                size,
                held: HashMap::new(),
                tick: 0,
                error: None,
                reads: 0,
            },
            patches: BTreeMap::new(),
            undo: VecDeque::new(),
            redo: Vec::new(),
            undo_cap: UNDO_STEPS,
            open_step: None,
            rev: 0,
            saved: BTreeMap::new(),
            copied: None,
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

    /// The byte as opened.
    pub fn original(&mut self, at: u64) -> Option<u8> {
        self.pages.byte(at)
    }

    /// Whether the byte at `at` differs from the file as opened.
    pub fn changed(&self, at: u64) -> bool {
        self.patches.contains_key(&at)
    }

    /// How many bytes differ from the file as opened.
    pub fn changed_count(&self) -> usize {
        self.patches.len()
    }

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

    fn put(&mut self, at: u64, value: Option<u8>) {
        match value {
            Some(b) => self.patches.insert(at, b),
            None => self.patches.remove(&at),
        };
    }

    /// Overwrites the byte at `at` with `value`, one undo step (`cursor` = where the cursor is,
    /// for the undo). Writing a byte's original value drops it from the changes. `false` past
    /// the end or where the file cannot be read.
    pub fn set(&mut self, at: u64, value: u8, cursor: Cursor) -> bool {
        self.change(at, value, cursor).is_some()
    }

    /// [`HexDoc::set`], saying whether it made an undo step (`Some(false)`: the byte already
    /// held `value`; `None`: past the end or unreadable).
    fn change(&mut self, at: u64, value: u8, cursor: Cursor) -> Option<bool> {
        self.open_step = None;
        let original = self.pages.byte(at)?;
        let before = self.patches.get(&at).copied();
        let after = (value != original).then_some(value);
        if before == after {
            return Some(false);
        }
        self.put(at, after);
        self.undo.push_back(Step {
            at,
            before,
            after,
            cursor,
        });
        while self.undo.len() > self.undo_cap.max(1) {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.rev += 1;
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
        let Some(original) = self.pages.byte(at) else {
            return false;
        };
        let after = (value != original).then_some(value);
        let step = self.undo.back_mut().expect("checked above");
        step.after = after;
        if step.before == after {
            self.undo.pop_back();
        }
        self.put(at, after);
        self.rev += 1;
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
        self.put(step.at, step.before);
        self.redo.push(step);
        self.rev += 1;
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
        self.put(step.at, step.after);
        self.undo.push_back(step);
        self.rev += 1;
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

    /// The bytes differ from what the original file holds now (what Save would write) — by
    /// content: an edit undone, or a byte typed back to its value, is no change.
    pub fn dirty(&self) -> bool {
        self.patches != self.saved
    }

    /// The bytes are written nowhere — neither the original nor the last Save copy (the close
    /// prompt).
    pub fn unsaved(&self) -> bool {
        self.dirty() && self.copied.as_ref() != Some(&self.patches)
    }

    /// A Save of `changes` landed: the original file holds them now.
    pub fn mark_saved(&mut self, changes: BTreeMap<u64, u8>) {
        self.saved = changes;
    }

    /// A Save copy of `changes` landed.
    pub fn mark_copied(&mut self, changes: BTreeMap<u64, u8>) {
        self.copied = Some(changes);
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
    /// `base` when it was opened (renewed after a Save of our own lands).
    opened: Ident,
    /// The session's claim on the cached copy, held while the file is open.
    _claim: Option<crate::cache::Claim>,
    /// Staged edits and refused ones: `<cache>/.edits/<host>/<abs>`.
    edits: PathBuf,
    /// `<cache>` (backups go to `<cache>/.backup/<host>/<abs>.<stamp>`).
    cache_root: PathBuf,
    /// Set by the first successful backup; later saves of the same session keep it.
    backup: Option<PathBuf>,
    /// Called between the pre-pass observations (tests: a writer in that gap).
    #[cfg(test)]
    pub(crate) pre_pass_hook: Option<Box<dyn FnMut() + Send>>,
    /// Called after each chunk of a save's pass over the bytes (tests: a writer in the middle).
    #[cfg(test)]
    pub(crate) chunk_hook: Option<Box<dyn FnMut(u64) + Send>>,
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
            #[cfg(test)]
            pre_pass_hook: None,
            #[cfg(test)]
            chunk_hook: None,
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
        let mut backup_file = None;
        if backup {
            match self.new_backup() {
                Ok(made) => backup_file = Some(made),
                Err(e) => {
                    let _ = std::fs::remove_file(&stage);
                    return Err(e);
                }
            }
        }
        let made: Vec<PathBuf> = std::iter::once(stage.clone())
            .chain(backup_file.as_ref().map(|(p, _)| p.clone()))
            .collect();
        let discard = |made: &[PathBuf]| {
            for path in made {
                let _ = std::fs::remove_file(path);
            }
        };
        let mut hasher = sha2::Sha256::new();
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
        if let Err(e) = copied {
            discard(&made);
            let after = Ident::of(&self.base, &self.base_path)?;
            if after != before {
                return Ok(PassResult::Changed(format!(
                    "{} while it was being read",
                    before.change(&after)
                )));
            }
            return Err(e);
        }
        let after = Ident::of(&self.base, &self.base_path)?;
        if after != before {
            discard(&made);
            return Ok(PassResult::Changed(format!(
                "{} while it was being read",
                before.change(&after)
            )));
        }
        Ok(PassResult::Done(Pass {
            stage,
            backup: backup_file.map(|(p, _)| p),
            sha256: hasher.finalize().into(),
        }))
    }

    /// The changes, when the bytes they apply to changed: `<name>.hexpatch-<stamp>.txt` among
    /// the edits (owner-only, a new file), one `offset: bytes` line per run of changed bytes
    /// under a header naming the source as it was opened.
    fn keep_patches(&self, patches: &BTreeMap<u64, u8>, why: &str) -> Result<PathBuf> {
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
        if let Err(e) = file.write_all(text.as_bytes()) {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("cannot write {}", path.display()));
        }
        Ok(path)
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
        let staged = pass.stage;
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
                    // A local file opened in place was just renamed over: its old inode's change
                    // time moved with the unlink. What it holds did not (checked by the pass).
                    if let Ok(now) = Ident::of(&self.base, &self.base_path) {
                        if (now.len, now.mtime, now.ino)
                            == (self.opened.len, self.opened.mtime, self.opened.ino)
                        {
                            self.opened = now;
                        }
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
        return autosave(file, &doc, hooks);
    }
    let title = file.origin.display();
    if !file.can_replace() {
        eprintln!("sopen: {CANNOT_REPLACE}");
    }
    let outcome: Arc<Mutex<Outcome>> = Arc::default();
    let leftover: Arc<Mutex<Option<Job>>> = Arc::default();
    let window = HexWindow::new(
        file,
        doc,
        cursor,
        notices,
        outcome.clone(),
        leftover.clone(),
    );
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
    Ok(HexWindow::new(
        file, doc, cursor, notices, outcome, leftover,
    ))
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

fn autosave(mut file: HexFile, doc: &HexDoc, hooks: &Hooks) -> Result<Outcome> {
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
    /// Lines from open that must be read whole and stay (see [`super::disclosures`]).
    opened: Vec<String>,
    /// What the last write reported that must be read whole, and the next such report, held
    /// while the button is down.
    notices: Vec<String>,
    pending_notices: Option<Vec<String>>,
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
            opened,
            notices: Vec::new(),
            pending_notices: None,
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
        self.pending_notices = Some(shown);
        let landed = match &result {
            Ok(JobDone::Saved(Saved::Written { .. })) => {
                self.doc.mark_saved(job.snapshot.clone());
                true
            }
            Ok(JobDone::Copy(..)) => {
                self.doc.mark_copied(job.snapshot.clone());
                true
            }
            _ => false,
        };
        let notes = matches!(&result, Ok(JobDone::Saved(Saved::Written { notes, .. })) if !notes.is_empty());
        let asked = matches!(
            job.kind,
            JobKind::Save { close_after: true } | JobKind::SaveCopy { close_after: true }
        ) || std::mem::take(&mut self.close_when_done);
        if !asked || !landed {
            return;
        }
        // An edit made while the save ran is not in what it wrote.
        let newer = *self.doc.patches() != job.snapshot;
        if newer || self.doc.unsaved() {
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Whether input may change anything: not under the close prompt, not once closing is
    /// decided.
    fn locked(&self) -> bool {
        self.prompt || self.close_ok
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
        if pressed(Key::Tab) {
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
                    egui::TextEdit::singleline(&mut self.goto)
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

    /// The rows: offsets, hex, ASCII, the cursor in both columns, changed bytes marked; and the
    /// scroll bar. Only the rows on screen are read and painted.
    fn grid(&mut self, ui: &mut egui::Ui, goto_focused: bool) {
        let ctx = ui.ctx().clone();
        let panel = ui.max_rect();
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let (cw, text_h) = ctx.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
        let row_h = (text_h + 3.0).ceil();
        let bar = Rect::from_min_max(Pos2::new(panel.right() - SCROLLBAR, panel.top()), panel.max);
        let rows_rect = Rect::from_min_max(
            Pos2::new(panel.left(), panel.top() + 4.0),
            Pos2::new(bar.left(), panel.bottom()),
        );
        let size = self.doc.size();
        let total = self.doc.rows();
        let visible = ((rows_rect.height() / row_h).floor() as u64).max(1);
        self.visible = visible;
        let max_top = total.saturating_sub(visible);

        let resp = ui.interact(rows_rect, grid_id(), Sense::click_and_drag());
        let bar_resp = ui.interact(bar, grid_id().with("bar"), Sense::click_and_drag());
        // The grid holds the keyboard (arrows and Tab act on the cursor, not on focus), except
        // while the Go to field or the close prompt has it.
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
        // The scroll bar: the thumb follows the pointer while the button is down on the track.
        let thumb_h = if total == 0 {
            bar.height()
        } else {
            (bar.height() * visible as f32 / total as f32)
                .clamp(20.0_f32.min(bar.height()), bar.height())
        };
        if bar_resp.is_pointer_button_down_on() && max_top > 0 {
            if let Some(p) = bar_resp.interact_pointer_pos() {
                let room = (bar.height() - thumb_h).max(1.0);
                let frac = ((p.y - bar.top() - thumb_h / 2.0) / room).clamp(0.0, 1.0);
                self.top_row = (f64::from(frac) * max_top as f64).round() as u64;
            }
        }

        let hex_x = rows_rect.left() + 8.0 + cw * (offset_digits(size) + 2) as f32;
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
        let back = ui.painter_at(panel);
        back.rect_filled(panel, 0.0, visuals.extreme_bg_color);
        // The rows never run under the scroll bar (on a narrow window the ASCII column is cut).
        let painter = ui.painter_at(Rect::from_min_max(
            panel.min,
            Pos2::new(bar.left(), panel.bottom()),
        ));
        if size == 0 {
            painter.text(
                rows_rect.center(),
                egui::Align2::CENTER_CENTER,
                "empty file — nothing to show",
                egui::FontId::proportional(14.0),
                visuals.weak_text_color(),
            );
        }
        let digits = offset_digits(size);
        let text = visuals.text_color();
        let changed_fg = visuals.warn_fg_color;
        let changed_bg = visuals.warn_fg_color.gamma_multiply(0.18);
        let cursor_bg = visuals.selection.bg_fill;
        let cursor_line = egui::Stroke::new(1.0, visuals.selection.stroke.color);
        let rows = ((rows_rect.height() / row_h).ceil() as u64).min(total - self.top_row);
        for k in 0..rows {
            let row = self.top_row + k;
            let y = rows_rect.top() + k as f32 * row_h;
            let off = row * ROW;
            let n = (size - off).min(ROW);
            painter.text(
                Pos2::new(rows_rect.left() + 8.0, y),
                egui::Align2::LEFT_TOP,
                format!("{off:0digits$X}"),
                font.clone(),
                visuals.weak_text_color(),
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
                let color = if changed { changed_fg } else { text };
                let hx = hex_x + cw * hex_col(i) as f32;
                let ax = ascii_x + cw * i as f32;
                if changed {
                    painter.rect_filled(cell(hx, 2.0), 2.0, changed_bg);
                    painter.rect_filled(cell(ax, 1.0), 2.0, changed_bg);
                }
                if at == self.cursor.at {
                    let (active, other) = match self.cursor.pane {
                        Pane::Hex => (cell(hx, 2.0), cell(ax, 1.0)),
                        Pane::Ascii => (cell(ax, 1.0), cell(hx, 2.0)),
                    };
                    painter.rect_filled(active, 2.0, cursor_bg);
                    painter.rect_stroke(other, 2.0, cursor_line, egui::StrokeKind::Inside);
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
        // The scroll bar's track and thumb.
        back.rect_filled(bar.shrink(2.0), 4.0, visuals.faint_bg_color);
        if max_top > 0 {
            let room = bar.height() - thumb_h;
            let top = bar.top() + room * (self.top_row as f64 / max_top as f64) as f32;
            let thumb =
                Rect::from_min_size(Pos2::new(bar.left(), top), Vec2::new(SCROLLBAR, thumb_h));
            back.rect_filled(thumb.shrink(2.0), 4.0, visuals.widgets.inactive.bg_fill);
        }
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
        let modal = egui::Modal::new(egui::Id::unique("sopen-hex-unsaved")).show(ctx, |ui| {
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

impl super::Window for HexWindow {
    /// One pass, in this order: Cmd/Ctrl+G (the Go to field takes the keyboard before it is
    /// laid out, so this frame's typing goes there), the toolbar, which then knows whether the
    /// field has this frame's typing, the keys and typing (to exactly one place: the field or
    /// the bytes), the panels and the grid, and last a finished save — after every input of
    /// this frame, so an edit made in the same frame counts.
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let cmd_g = !self.locked() && super::command_key(&ctx, Key::G, false);
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
        self.keys(&ctx, goto_owns);
        let goto_focused = goto_owns;
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
            .chain(&self.notices)
            .map(|n| (n.clone(), error))
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
            .show(ui, |ui| self.grid(ui, goto_focused));
        self.poll_job(&ctx);
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
