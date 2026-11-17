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
    /// The byte whose high nibble was just typed (its step is the last one): the low nibble
    /// typed next joins that step, so a byte typed as two digits undoes as one.
    open_step: Option<u64>,
    /// Bumped by every change; `saved_rev` is the revision in the original file, `copied_rev`
    /// the one last written by Save copy.
    rev: u64,
    saved_rev: u64,
    copied_rev: u64,
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
            open_step: None,
            rev: 0,
            saved_rev: 0,
            copied_rev: 0,
        }
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
        self.open_step = None;
        let Some(original) = self.pages.byte(at) else {
            return false;
        };
        let before = self.patches.get(&at).copied();
        let after = (value != original).then_some(value);
        if before == after {
            return true;
        }
        self.put(at, after);
        self.undo.push_back(Step {
            at,
            before,
            after,
            cursor,
        });
        if self.undo.len() > UNDO_STEPS {
            self.undo.pop_front();
        }
        self.redo.clear();
        self.rev += 1;
        true
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
                    let steps = self.undo.len();
                    let done = self.set(at, (digit << 4) | (current & 0x0f), cursor);
                    if done && self.undo.len() > steps {
                        self.open_step = Some(at);
                    }
                    done.then_some(Cursor {
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

    /// The state differs from the original file (what Save would write).
    pub fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    /// The state is written nowhere — neither the original nor a Save copy (the close prompt).
    pub fn unsaved(&self) -> bool {
        self.dirty() && self.rev != self.copied_rev
    }

    pub fn mark_saved(&mut self, rev: u64) {
        self.saved_rev = rev;
    }

    pub fn mark_copied(&mut self, rev: u64) {
        self.copied_rev = rev;
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

/// The opened file: where it lives, the local file its bytes are read from, and the local places
/// sopen writes to.
pub struct HexFile {
    pub origin: Origin,
    /// The bytes as opened, read through this handle only — never by path: a Save of a local
    /// file renames a new file over the path, and this keeps reading the original.
    base: Arc<File>,
    /// Where `base` was opened: the session's cached copy, or the local file itself.
    base_path: PathBuf,
    /// `base`'s size and mtime when it was opened.
    opened: (u64, Option<SystemTime>),
    /// The session's claim on the cached copy, held while the file is open.
    _claim: Option<crate::cache::Claim>,
    /// Staged edits and refused ones: `<cache>/.edits/<host>/<abs>`.
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
    },
    /// Nothing was written: the file changed since it was opened. `kept` = where the edited
    /// copy went, when one had been staged.
    Refused {
        message: String,
        kept: Option<PathBuf>,
    },
}

/// Copies `len` bytes of `src` into `out` with `patches` applied, a megabyte at a time.
fn copy_patched(
    src: &File,
    len: u64,
    out: &mut File,
    patches: &BTreeMap<u64, u8>,
) -> std::io::Result<()> {
    let mut buf = vec![0u8; 1 << 20];
    let mut at = 0u64;
    while at < len {
        let n = (len - at).min(buf.len() as u64) as usize;
        src.read_exact_at(&mut buf[..n], at)?;
        for (&off, &b) in patches.range(at..at + n as u64) {
            buf[(off - at) as usize] = b;
        }
        out.write_all(&buf[..n])?;
        at += n as u64;
    }
    out.flush()
}

impl HexFile {
    /// Opens `base` (the cached copy, or the local file itself) for `origin`. `claim` is held
    /// for as long as this lives.
    pub fn open(
        origin: Origin,
        base: &Path,
        claim: Option<crate::cache::Claim>,
        cache_root: &Path,
    ) -> Result<Self> {
        let file = File::open(base).with_context(|| format!("cannot open {}", base.display()))?;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {}", base.display()))?;
        if !meta.is_file() {
            bail!("{} is not a regular file", base.display());
        }
        let edits = crate::cache::edits_path(cache_root, origin.host(), &origin.abs);
        Ok(Self {
            origin,
            base: Arc::new(file),
            base_path: base.to_path_buf(),
            opened: (meta.len(), meta.modified().ok()),
            _claim: claim,
            edits,
            cache_root: cache_root.to_path_buf(),
            backup: None,
        })
    }

    /// Bytes in the file as opened.
    pub fn size(&self) -> u64 {
        self.opened.0
    }

    /// The handle the bytes are read through (for [`HexDoc::new`]).
    pub fn base(&self) -> Arc<File> {
        self.base.clone()
    }

    /// Whether Save can replace the original here (Save copy always can).
    pub fn can_replace(&self) -> bool {
        self.origin.can_replace()
    }

    /// Whether the bytes as opened are still what `base` holds (size and mtime of the handle).
    fn base_changed(&self) -> Result<Option<String>> {
        let meta = self
            .base
            .metadata()
            .with_context(|| format!("cannot stat {}", self.base_path.display()))?;
        if (meta.len(), meta.modified().ok()) == self.opened {
            return Ok(None);
        }
        Ok(Some(format!(
            "{} changed since it was opened ({} bytes then, {} now); nothing was written",
            self.base_path.display(),
            grouped(self.opened.0),
            grouped(meta.len())
        )))
    }

    /// A new owner-only file next to where edits are kept holding the bytes with `patches`
    /// applied (never over or through an existing file).
    fn stage(&self, patches: &BTreeMap<u64, u8>, tag: &str) -> Result<PathBuf> {
        let (path, mut file) = super::create_keep(&self.edits, tag)?;
        if let Err(e) = copy_patched(&self.base, self.size(), &mut file, patches) {
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
        if let Err(e) = copy_patched(&self.base, self.size(), &mut file, &BTreeMap::new()) {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("cannot write the backup {}", path.display()));
        }
        println!("backup of {} at {}", self.origin.display(), path.display());
        self.backup = Some(path.clone());
        Ok(path)
    }

    /// Save (spec §5, §6a), in this order: can this server replace a file in one step (refused
    /// before anything is written if not) → are the bytes as opened unchanged (refused if not)
    /// → the local backup of the original (printed) → stage the original with `patches`
    /// applied → conflict-checked overwrite. A refusal or a transport error keeps the staged
    /// edit in a local file whose path the message names.
    pub fn save(&mut self, patches: &BTreeMap<u64, u8>) -> Result<Saved> {
        if !self.can_replace() {
            bail!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.origin.display()
            );
        }
        if let Some(message) = self.base_changed()? {
            return Ok(Saved::Refused {
                message: format!("not saved: {message}"),
                kept: None,
            });
        }
        let backup = self.ensure_backup()?;
        let staged = self.stage(patches, "edited").with_context(|| {
            format!(
                "{} was not touched (the original is backed up at {})",
                self.origin.display(),
                backup.display()
            )
        })?;
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
    /// anything. Returns the printed path of the new file.
    pub fn save_copy(&mut self, patches: &BTreeMap<u64, u8>) -> Result<String> {
        if let Some(message) = self.base_changed()? {
            bail!("no copy saved: {message}");
        }
        let staged = self.stage(patches, "copy")?;
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
    rev: u64,
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
    /// The Go to field's text, and a request to give it the keyboard.
    goto: String,
    focus_goto: bool,
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
            focus_goto: false,
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
        let file = self.file.clone();
        let copy = matches!(kind, JobKind::SaveCopy { .. });
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut file = file.lock().unwrap();
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if close_after || waited {
            if close_after && newer {
                self.status =
                    Status::info(format!("newer edits are not saved · {}", self.status.text));
            }
            self.prompt = true;
        }
    }

    /// Keys for the whole window (not while the Go to field or the close prompt has them).
    fn keys(&mut self, ctx: &egui::Context, goto_focused: bool) {
        use super::command_key;
        if self.prompt {
            return;
        }
        if command_key(ctx, Key::S, true) {
            self.start(ctx, JobKind::SaveCopy { close_after: false });
        } else if command_key(ctx, Key::S, false) {
            self.start(ctx, JobKind::Save { close_after: false });
        }
        if command_key(ctx, Key::G, false) {
            self.goto.clear();
            self.focus_goto = true;
        }
        if goto_focused {
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
            if std::mem::take(&mut self.focus_goto) {
                field.request_focus();
            }
            if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.goto();
            }
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
            let save = ui
                .add_enabled(
                    !busy && self.doc.dirty() && self.can_replace,
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
                .add_enabled(!busy, egui::Button::new("Save copy"))
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
    fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_job(&ctx);
        let goto_had_focus = ctx.memory(|m| m.focused()) == Some(goto_id());
        self.keys(&ctx, goto_had_focus);
        let field = egui::Panel::top("hexbar")
            .show(ui, |ui| {
                ui.add_space(3.0);
                let field = self.toolbar(ui);
                ui.add_space(1.0);
                field
            })
            .inner;
        let goto_focused = field.has_focus() || field.lost_focus();
        // A report that lands while the button is down waits until it is let go: its row would
        // move the grid under the pointer.
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
