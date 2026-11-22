//! The image window's write side (spec §5): the opened file, its bytes as opened, the local
//! backup written before the first overwrite, Save's conflict-checked overwrite and Save copy's
//! new sibling. Window-free, so the tests and the `SOPEN_AUTOSAVE` hook run the exact code the
//! window's buttons run.

use super::Encoded;
use crate::gui::Origin;
use crate::remote::WriteOutcome;
use anyhow::{anyhow, bail, Context as _, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// The opened image file: where it lives, its bytes as opened (for the backup) and the local
/// places sopen writes to.
pub struct ImageFile {
    pub origin: Origin,
    /// The bytes as opened — what the backup holds. Kept in memory: a later fetch of the same
    /// file can replace the cached copy while this window is open.
    pub(super) original: Arc<Vec<u8>>,
    /// Encoded edits waiting to be uploaded, and refused ones: `<cache>/.edits/<host>/<abs>`.
    pub(super) edits: PathBuf,
    /// `<cache>` (backups go to `<cache>/.backup/<host>/<abs>.<stamp>`).
    pub(super) cache_root: PathBuf,
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
/// "outcome unknown — check <path>", never "not saved". The hex window says it the same way.
pub(in crate::gui) fn failed(e: &anyhow::Error, staged: &Path) -> anyhow::Error {
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
        let (path, mut file) = crate::gui::create_keep(&self.edits, tag)?;
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
                    let (kept, _) = crate::gui::create_keep(&self.edits, "conflict")?;
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
        let wanted = crate::gui::edited_name(&name);
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
