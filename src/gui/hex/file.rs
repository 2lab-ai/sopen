//! The hex window's write side (spec §5, §6a): the file as opened ([`Base`], [`HexFile`]), one
//! pass over its bytes that stages the edits and backs the original up, the conflict-checked
//! Save and Save copy.

use super::super::image::CANNOT_REPLACE;
use super::super::Origin;
use super::{grouped, Feed, InPlace};
use crate::remote::WriteOutcome;
use crate::stream::{ReaderKind, Source};
use anyhow::{bail, Context as _, Result};
use eframe::egui;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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

/// The local file a save's pass reads.
#[derive(Clone)]
struct Local {
    /// The bytes as opened, read through this handle only — never by path: a Save of a local
    /// file renames a new file over the path, and this keeps reading the original.
    file: Arc<File>,
    /// Where `file` was opened: the session's cached copy, or the local file itself.
    path: PathBuf,
    /// `file` when it was opened (and when a Save of a local file in place re-bound it).
    opened: Ident,
}

/// The interim Save's whole fetch as its worker reports it (06 §7.1): the window's status row
/// reads it and Esc sets `cancel`.
#[derive(Debug, Default)]
pub struct Fetching {
    /// The fetch is running: nothing has been written anywhere but the cache.
    pub active: AtomicBool,
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// Stops the fetch between its chunks; nothing is kept of it.
    pub cancel: AtomicBool,
}

/// What repaints a streamed window when its stream has something new (06 §5's `Waker`). The
/// Source is opened before the window exists, so its waker is made here first and the window
/// binds its context to it on its first pass; a wake before that has nothing to repaint.
#[derive(Clone, Default)]
pub struct Repaint(Arc<Mutex<Option<egui::Context>>>);

impl Repaint {
    /// The waker to open the Source with.
    pub fn waker(&self) -> crate::stream::Waker {
        let slot = self.0.clone();
        Arc::new(move || {
            let ctx = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        })
    }

    /// From now on a wake repaints `ctx`'s window.
    pub(super) fn bind(&self, ctx: &egui::Context) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(ctx.clone());
        }
    }

    /// Whether a wake repaints `ctx`'s window (tests).
    #[cfg(test)]
    pub(super) fn bound_to(&self, ctx: &egui::Context) -> bool {
        self.0.lock().unwrap().as_ref() == Some(ctx)
    }
}

/// The opened file: where it lives, the local file its bytes are read from (or the stream a
/// remote one is viewed through), and the local places sopen writes to.
pub struct HexFile {
    pub origin: Origin,
    /// The local file a save's pass reads. `None` for a streamed file (06 §7.1) until its first
    /// Save or Save copy has fetched the whole file into the cache ([`HexFile::fetch_base`]).
    local: Option<Local>,
    /// The stream a remote file is viewed through (06 §5); `None` for a file read in place.
    stream: Option<Source>,
    /// What the stream wakes.
    repaint: Repaint,
    /// A streamed file's `Origin` has been used (06 §2: it connects on first use — a Save or a
    /// Save copy): from then on it is what says whether this server can replace a file.
    connected: bool,
    /// Bytes in the file as opened.
    size: u64,
    /// The session's claim on the cached copy, held while the file is open.
    claim: Option<crate::cache::Claim>,
    /// The interim Save's whole fetch, for the status row and its Esc (06 §7.1, §8).
    pub(super) fetching: Arc<Fetching>,
    /// What that fetch said that the owner must read (an earlier copy's edits set aside), for
    /// the window to take.
    fetch_notices: Vec<String>,
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
            size: base.opened.len,
            local: Some(Local {
                file: Arc::new(base.file),
                path: base.path,
                opened: base.opened,
            }),
            stream: None,
            repaint: Repaint::default(),
            connected: true,
            claim,
            fetching: Arc::default(),
            fetch_notices: Vec::new(),
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

    /// A remote file viewed through `source` (06 §7.1): nothing is fetched to open it. Its first
    /// Save or Save copy fetches the whole file into the cache and runs on that copy (the interim
    /// Save, until 07's lands). `repaint` is what `source` was opened to wake
    /// ([`Repaint::waker`]).
    pub fn streamed(origin: Origin, source: Source, repaint: Repaint, cache_root: &Path) -> Self {
        let edits = crate::cache::edits_path(cache_root, origin.host(), &origin.abs);
        Self {
            origin,
            size: source.size(),
            local: None,
            stream: Some(source),
            repaint,
            connected: false,
            claim: None,
            fetching: Arc::default(),
            fetch_notices: Vec::new(),
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
        self.size
    }

    /// The handle a file read in place is read through (for [`HexDoc::new`]).
    ///
    /// # Panics
    /// On a streamed file, which has none: [`HexFile::feed`] is what every file has.
    pub fn base(&self) -> Arc<File> {
        self.local
            .as_ref()
            .expect("a file read in place")
            .file
            .clone()
    }

    /// The stream the file is viewed through, if it is.
    pub fn stream(&self) -> Option<Source> {
        self.stream.clone()
    }

    /// What the stream wakes: the window binds its context to it.
    pub(super) fn repaint(&self) -> Repaint {
        self.repaint.clone()
    }

    /// What the window reads the bytes through: the stream's reader at the screen (06 §7.1:
    /// one Interactive reader), or the local file in place.
    pub fn feed(&self) -> Box<dyn Feed> {
        match (&self.stream, &self.local) {
            (Some(source), _) => Box::new(source.reader(ReaderKind::Interactive)),
            (None, Some(local)) => Box::new(InPlace(local.file.clone())),
            (None, None) => unreachable!("a hex file is read in place or streamed"),
        }
    }

    /// Where the bytes are read from, for a message: the local file (the cached copy, or the
    /// file itself), or the remote one a stream reads.
    fn read_from(&self) -> String {
        match &self.local {
            Some(local) => local.path.display().to_string(),
            None => self.origin.display(),
        }
    }

    /// The local file a save's pass reads.
    fn local(&self) -> Result<Local> {
        self.local
            .clone()
            .context("the file has not been fetched for saving")
    }

    /// What the last fetch for a Save said that must be read whole, once.
    pub fn take_fetch_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.fetch_notices)
    }

    /// Whether Save can replace the original here (Save copy always can). A streamed file's
    /// `Origin` connects on its first use, which is a Save (06 §2): until then nothing is asked
    /// of it and the answer is yes — the first Save asks, before it fetches anything.
    pub fn can_replace(&self) -> bool {
        if self.stream.is_some() && !self.connected {
            return true;
        }
        self.origin.can_replace()
    }

    /// The interim Save's whole fetch (06 §7.1, until 07's Save lands): a streamed file's first
    /// Save or Save copy fetches the whole file into the cache exactly as an open without
    /// streaming does — claimed, recorded, pipelined, stopped by the fetch's cancel flag — and
    /// the save's pass then runs on that copy. The fetch must be the file the stream opened:
    /// `Some(why)` when its size or mtime is another, and then nothing is kept of it. Its stat
    /// (with its SHA-256) becomes the baseline of the overwrite (01 §5).
    fn fetch_base(&mut self) -> Result<Option<String>> {
        let Some(source) = self.stream.clone() else {
            return Ok(None);
        };
        if self.local.is_some() {
            return Ok(None);
        }
        let opened = source.opened();
        let fetching = self.fetching.clone();
        fetching.cancel.store(false, Ordering::SeqCst);
        fetching.done.store(0, Ordering::SeqCst);
        fetching.total.store(opened.size, Ordering::SeqCst);
        fetching.active.store(true, Ordering::SeqCst);
        // Whatever ends the fetch, the window stops showing it.
        struct Ended(Arc<Fetching>);
        impl Drop for Ended {
            fn drop(&mut self) {
                self.0.active.store(false, Ordering::SeqCst);
            }
        }
        let _ended = Ended(fetching.clone());
        let host = self.origin.host().to_string();
        let abs = self.origin.abs.clone();
        let fs = self.origin.fs()?;
        self.connected = true;
        let (fetched, _ms) = crate::cache::fetch_claimed_cancellable(
            fs,
            &host,
            &abs,
            &self.cache_root,
            &mut |done, total| {
                fetching.done.store(done, Ordering::SeqCst);
                fetching.total.store(total, Ordering::SeqCst);
            },
            &fetching.cancel,
        )?;
        if fetched.at.size != opened.size {
            return Ok(Some(format!(
                "{} bytes then, {} now",
                grouped(opened.size),
                grouped(fetched.at.size)
            )));
        }
        if fetched.at.mtime != opened.mtime {
            return Ok(Some("its modification time changed".to_string()));
        }
        let base = Base::open(&fetched.claim.local)?;
        self.origin.at = fetched.at;
        self.local = Some(Local {
            file: Arc::new(base.file),
            path: base.path,
            opened: base.opened,
        });
        self.claim = Some(fetched.claim);
        self.fetch_notices = fetched.notices;
        Ok(None)
    }

    /// The stream was reloaded by the owner (Cmd/Ctrl+R): the copy a Save fetched is of the
    /// version before, so the next Save fetches again.
    pub(super) fn forget_fetched(&mut self) {
        if let Some(source) = &self.stream {
            self.size = source.size();
            self.local = None;
            self.claim = None;
            // The backup made was of the version before: this one gets its own.
            self.backup = None;
        }
    }

    /// One of sopen's own writes landed (06 §5): the stream is reloaded at once, onto the file
    /// the write's own baseline names — `Live` on the saved file, or `Changed` when the remote
    /// is already another one (the write's notes say so). What is returned must be read: the
    /// view could not be reloaded.
    fn reload_saved(&self) -> Option<String> {
        let source = self.stream.as_ref()?;
        let e = source.reload_saved(self.origin.at.fingerprint()).err()?;
        Some(format!(
            "saved, but the view could not be reloaded ({e:#}) — Cmd/Ctrl+R reloads"
        ))
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
        let Local {
            file: base,
            path: base_path,
            opened,
        } = self.local()?;
        let before = Ident::of(&base, &base_path)?;
        if before != opened {
            return Ok(PassResult::Changed(opened.change(&before)));
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
                base.read_exact_at(&mut buf[..n], at)
                    .with_context(|| format!("cannot read {}", base_path.display()))?;
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
            bail!("cannot stat {} (test)", base_path.display());
        }
        let after = Ident::of(&base, &base_path)?;
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
        self.origin.is_local()
            && self
                .local
                .as_ref()
                .is_some_and(|l| l.path == Path::new(&self.origin.abs))
    }

    /// After a Save of a local file in place landed: the file now at the path is opened and
    /// becomes the file read — only if it holds exactly what was saved (`want`, the stage's
    /// SHA-256), read through its own handle, unchanged while it was read.
    fn rebind(&mut self, want: &[u8; 32]) -> Rebind {
        use sha2::Digest;
        let Some(read) = self.local.clone() else {
            return self.lose("the file saved is not the file read".to_string());
        };
        let check = (|| -> Result<Option<Base>> {
            let base = Base::open(&read.path)?;
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
            Ok((still && &same == want && base.opened.len == read.opened.len).then_some(base))
        })();
        match check {
            Ok(Some(base)) => {
                let file = Arc::new(base.file);
                self.local = Some(Local {
                    file: file.clone(),
                    path: base.path,
                    opened: base.opened,
                });
                Rebind::Rebound(file)
            }
            Ok(None) => self.lose(format!(
                "{} no longer holds what was saved (changed right after the save?)",
                read.path.display()
            )),
            Err(e) => self.lose(format!(
                "{} cannot be read back after the save ({e:#})",
                read.path.display()
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
        let mtime = match (&self.local, &self.stream) {
            (Some(local), _) => local.opened.mtime,
            (None, Some(source)) => Some(source.opened().mtime),
            (None, None) => None,
        }
        .map_or("unknown".to_string(), crate::cache::rfc3339);
        let mut text = format!(
            "# sopen hex edits, not saved: {} changed since it was opened ({why})\n\
             # source: {}\n\
             # as opened: {} bytes, modified {mtime}\n\
             # one line per run of changed bytes — offset: bytes (hex)\n",
            self.origin.display(),
            self.read_from(),
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
                self.read_from(),
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
        // A streamed file: its `Origin` connects here, at its first use, and says whether the
        // server can replace a file — before a byte of the whole fetch is asked for.
        if self.stream.is_some() && !self.connected {
            let name = self.origin.display();
            self.origin
                .fs()
                .with_context(|| format!("{name} was not touched"))?;
            self.connected = true;
        }
        if !self.can_replace() {
            bail!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.origin.display()
            );
        }
        match self.fetch_base() {
            Ok(None) => {}
            Ok(Some(why)) => return self.refuse_changed(patches, &why),
            Err(e) => return Err(e.context(format!("{} was not touched", self.origin.display()))),
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
                WriteOutcome::Written { now, mut notes } => {
                    self.origin.at = now;
                    let _ = std::fs::remove_file(&staged);
                    notes.extend(self.reload_saved());
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
        if let Some(why) = self.fetch_base()? {
            let kept = self.keep_patches(patches, &why)?;
            bail!(
                "no copy saved: {} changed since it was opened ({why}); nothing was written \
                 — your edits are listed in {}",
                self.read_from(),
                kept.display()
            );
        }
        let staged = match self.pass(patches, "copy", false)? {
            PassResult::Done(pass) => pass.stage,
            PassResult::Changed(why) => {
                let kept = self.keep_patches(patches, &why)?;
                bail!(
                    "no copy saved: {} changed since it was opened ({why}); nothing was written \
                     — your edits are listed in {}",
                    self.read_from(),
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
