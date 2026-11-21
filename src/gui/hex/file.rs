//! The hex window's write side (spec §5, §6a): the file as opened ([`Base`], [`HexFile`]), one
//! pass over its bytes that stages the edits and backs the original up, the conflict-checked
//! Save and Save copy.

use super::super::image::CANNOT_REPLACE;
use super::super::Origin;
use super::grouped;
use crate::remote::WriteOutcome;
use anyhow::{bail, Context as _, Result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

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
    pub(super) edits: PathBuf,
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
pub(super) const CHUNK: usize = 1 << 20;

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
        let (stage, mut stage_file) = super::super::create_keep(&self.edits, tag)?;
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
    pub(in crate::gui) fn keep_patches(
        &self,
        patches: &BTreeMap<u64, u8>,
        why: &str,
    ) -> Result<PathBuf> {
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
    pub(in crate::gui) fn list_text(&self, patches: &BTreeMap<u64, u8>, why: &str) -> String {
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
    pub(in crate::gui) fn set_read_only(&mut self, why: &str) {
        self.read_only.get_or_insert_with(|| why.to_string());
    }

    /// The file's name, for naming a list of its edits.
    pub(in crate::gui) fn name(&self) -> String {
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
                    let (kept, _) = super::super::create_keep(&self.edits, "conflict")?;
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
        result.map_err(|e| super::super::image::failed(&e, &staged))
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
        let wanted = super::super::edited_name(&name);
        let result = self
            .origin
            .fs()
            .and_then(|fs| fs.write_new_sibling(&staged, &dir, &wanted));
        match result {
            Ok(final_name) => {
                let _ = std::fs::remove_file(&staged);
                Ok(self.origin.show(&crate::remote::join(&dir, &final_name)))
            }
            Err(e) => Err(super::super::image::failed(&e, &staged)),
        }
    }
}
