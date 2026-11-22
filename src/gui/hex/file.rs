//! The hex window's write side: the file as opened ([`Base`], [`HexFile`]) and every way its
//! edits are written (07 §4–§7).
//!
//! - **A file with a local base** — a local file read in place, or a remote file read from its
//!   copy in the cache — keeps 01 §6a's Save: one pass over its bytes that stages the edits and
//!   backs the original up, then the conflict-checked overwrite ([`HexFile::save`],
//!   [`HexFile::save_copy`]).
//! - **A streamed file** (06 §7.1) has no local base: its Save is ED1's ([`HexFile::save_streamed`],
//!   `remote::patch`), on the viewer's `Origin`, recorded in the journal before anything changes
//!   the remote. Its baseline at most 256 MiB is the Bulk pass over the stream ([`Checking`]).
//! - Save As writes the edits to a new local file through `gui::save_as` ([`HexProducer`]).
//! - What an earlier session left — journals, cut-off Saves, undo files — is found on open
//!   ([`recover`]) and offered in the window's recovery rows.

use super::super::image::CANNOT_REPLACE;
use super::super::Origin;
use super::{grouped, Feed, InPlace};
use crate::journal::{self, Journal, Kind, Sha};
use crate::remote::patch::{self, Baseline, BaselinePass, Patch, PatchFs, Progress, UndoFile};
use crate::remote::{Fingerprint, WriteOutcome, HASH_CHECK_MAX};
use crate::stream::{ReadError, ReaderKind, Source};
use anyhow::{anyhow, bail, Context as _, Result};
use eframe::egui;
use sha2::Digest as _;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write as _;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

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
        Self::of_as(file, &path.display().to_string())
    }

    /// [`Ident::of`], the file named `shown` in what is said.
    fn of_as(file: &File, shown: &str) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {shown}"))?;
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
        Self::open_as(path, &path.display().to_string())
    }

    /// [`Base::open`] of a remote file's copy in sopen's cache: what fails is said about
    /// `shown`, the file as its owner knows it (`host:/abs`) — never about the copy's path.
    pub fn open_as(path: &Path, shown: &str) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {shown}"))?;
        let meta = file
            .metadata()
            .with_context(|| format!("cannot stat {shown}"))?;
        if !meta.is_file() {
            bail!("{shown} is not a regular file");
        }
        let opened = Ident::of_as(&file, shown)?;
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

/// The baseline pass of a streamed file of at most 256 MiB (07 §4.5): a Bulk reader on the
/// stream hashes the whole file in the background, from the first edit, and keeps an 8-byte
/// hash of every page. The window reads its progress (`checking the file for Save — 37 %`); a
/// Save, a Save copy or a Save As that needs the hash waits for it ([`Checking::baseline`]).
/// One pass at a time, for the version open now: a reload makes the next one read that version.
#[derive(Default)]
pub struct Checking {
    state: Mutex<PassState>,
    ended: Condvar,
    /// A pass is reading (the status row says so).
    pub active: AtomicBool,
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// Stops a pass between its reads (the window went away).
    pub cancel: AtomicBool,
    /// Passes begun (tests).
    #[cfg(test)]
    pub(crate) runs: AtomicU64,
    /// Waits for a pass another thread runs, one counted for each 100 ms a caller spends
    /// waiting (tests: a caller has reached the wait once this goes up).
    #[cfg(test)]
    pub(crate) waiters: AtomicU64,
}

#[derive(Default)]
enum PassState {
    #[default]
    Idle,
    Running,
    /// The pass over the version with this `(size, mtime)`.
    Done(u64, SystemTime, std::result::Result<Baseline, String>),
}

impl Checking {
    /// The pass over the version `source` has open now: the one already made, or one made now —
    /// waiting for a pass another thread runs. A pass that failed for this version is made
    /// again (a Save asks once more; the frames never do, see [`Checking::start`]).
    ///
    /// `cancel` (Esc in a Save or a Save As) ends the wait, and a pass this call runs itself
    /// between its reads, as [`Cancelled`](crate::remote::Cancelled).
    pub fn baseline(&self, source: &Source, cancel: &dyn Fn() -> bool) -> Result<Baseline> {
        let opened = source.opened();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if cancel() {
                bail!(crate::remote::Cancelled);
            }
            match &*state {
                PassState::Done(size, mtime, Ok(pass))
                    if (*size, *mtime) == (opened.size, opened.mtime) =>
                {
                    return Ok(pass.clone());
                }
                PassState::Running => {
                    #[cfg(test)]
                    self.waiters.fetch_add(1, Ordering::SeqCst);
                    state = self
                        .ended
                        .wait_timeout(state, Duration::from_millis(100))
                        .unwrap_or_else(|p| p.into_inner())
                        .0;
                }
                _ => break,
            }
        }
        *state = PassState::Running;
        drop(state);
        let made = self.make(source, Some(cancel));
        if cancel() {
            bail!(crate::remote::Cancelled);
        }
        made.map_err(|why| anyhow!(why))
    }

    /// Runs the pass, the state already `Running` (claimed by the caller), and records how it
    /// ended for the version it read.
    fn make(
        &self,
        source: &Source,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> std::result::Result<Baseline, String> {
        let opened = source.opened();
        let result = self.run(source, cancel).map_err(|e| format!("{e:#}"));
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        *state = PassState::Done(opened.size, opened.mtime, result.clone());
        self.ended.notify_all();
        result
    }

    /// The pass's result for the version `source` has open, if one has ended.
    pub fn ended(&self, source: &Source) -> Option<std::result::Result<Baseline, String>> {
        let opened = source.opened();
        match &*self.state.lock().unwrap_or_else(|p| p.into_inner()) {
            PassState::Done(size, mtime, result)
                if (*size, *mtime) == (opened.size, opened.mtime) =>
            {
                Some(result.clone())
            }
            _ => None,
        }
    }

    /// Starts the pass in the background, unless one runs or has ended for this version — even
    /// one that failed: that one is made again only by a Save that needs it, or for the next
    /// version a reload opens.
    pub fn start(self: &Arc<Self>, source: &Source) {
        if source.size() > HASH_CHECK_MAX {
            return;
        }
        let opened = source.opened();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        match &*state {
            PassState::Running => return,
            PassState::Done(size, mtime, _) if (*size, *mtime) == (opened.size, opened.mtime) => {
                return
            }
            _ => {}
        }
        // Claimed here, before the thread exists: the next frame sees it running.
        *state = PassState::Running;
        drop(state);
        let (me, source) = (self.clone(), source.clone());
        std::thread::spawn(move || {
            let _ = me.make(&source, None);
        });
    }

    /// One Bulk read through the whole version open now, at most 256 MiB; `cancel` too stops it
    /// between its reads.
    fn run(&self, source: &Source, cancel: Option<&dyn Fn() -> bool>) -> Result<Baseline> {
        #[cfg(test)]
        self.runs.fetch_add(1, Ordering::SeqCst);
        let opened = source.opened();
        let size = opened.size;
        if size > HASH_CHECK_MAX {
            bail!("over 256 MiB, a save checks the pages under the edits, not a whole hash");
        }
        self.done.store(0, Ordering::SeqCst);
        self.total.store(size, Ordering::SeqCst);
        self.active.store(true, Ordering::SeqCst);
        struct Ended<'a>(&'a AtomicBool);
        impl Drop for Ended<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _ended = Ended(&self.active);
        let mut reader = source.reader(ReaderKind::Bulk);
        let mut pass = BaselinePass::new();
        let mut buf = vec![0u8; CHUNK];
        let mut at = 0u64;
        while at < size {
            if cancel.is_some_and(|c| c()) {
                bail!(crate::remote::Cancelled);
            }
            let want = (size - at).min(CHUNK as u64) as usize;
            let got = match reader.read_at(at, &mut buf[..want], &self.cancel) {
                Ok(0) => bail!(
                    "the file ended at {} of {} bytes",
                    grouped(at),
                    grouped(size)
                ),
                Ok(n) => n,
                Err(ReadError::Changed) => bail!("the file changed since it was opened"),
                Err(e) => bail!("the file could not be read for the check ({e})"),
            };
            pass.update(&buf[..got]);
            at += got as u64;
            self.done.store(at, Ordering::SeqCst);
        }
        let now = source.opened();
        if (now.size, now.mtime) != (opened.size, opened.mtime) {
            bail!("the file was reloaded during the check");
        }
        Ok(pass.finish())
    }
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
    /// The local file a save's pass reads. `None` for a streamed file (06 §7.1): its Saves read
    /// the remote fresh, on the `Origin` (07 §4.2).
    local: Option<Local>,
    /// The stream a remote file is viewed through (06 §5); `None` for a file read in place.
    stream: Option<Source>,
    /// What the stream wakes.
    repaint: Repaint,
    /// A streamed file's `Origin` has been used (06 §2: it connects on first use — a Save, a
    /// Save copy, a Save As, or the recovery of an earlier session's edits): from then on it is
    /// what says whether this server can replace a file.
    connected: bool,
    /// Bytes in the file as opened.
    size: u64,
    /// The session's claim on the cached copy, held while the file is open (dropping it frees
    /// the copy).
    _claim: Option<crate::cache::Claim>,
    /// A streamed file's baseline pass (07 §4.5), shared with the window.
    pub(super) checking: Arc<Checking>,
    /// A streamed file's undo file (07 §5.1): one per session of a version, made at its first
    /// Save. The number is the window's reload count when it was made: a version reloaded by
    /// the owner gets its own.
    undo: Option<Box<(u64, UndoFile)>>,
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
    /// A streamed Save's steps, heard before each request (tests: a stop or a lost reply, E18).
    #[cfg(test)]
    pub(crate) step_hook: Option<Box<dyn FnMut(patch::Step) -> patch::At + Send>>,
    /// The copy's pace (tests: [`patch::Pace::timed`]'s rule at small sizes).
    #[cfg(test)]
    pub(crate) pace: Option<Box<patch::Pace>>,
    /// The rate a streamed Save says its cost at, instead of the stream's (tests: an exact
    /// line).
    #[cfg(test)]
    pub(crate) rate: Option<f64>,
    /// Where the journals are written (tests: a disk that counts what is written).
    pub(super) journal_fs: Arc<dyn journal::Fs>,
    /// Tests: how many times recovery listed the journals again (its lock retry).
    #[cfg(test)]
    pub(crate) retries: Arc<AtomicU64>,
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
            _claim: claim,
            checking: Arc::default(),
            undo: None,
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
            #[cfg(test)]
            step_hook: None,
            #[cfg(test)]
            pace: None,
            #[cfg(test)]
            rate: None,
            journal_fs: journal::disk(),
            #[cfg(test)]
            retries: Arc::default(),
        }
    }

    /// A remote file viewed through `source` (06 §7.1): nothing is fetched to open it, and
    /// nothing is fetched to save it (07 §4.2). `repaint` is what `source` was opened to wake
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
            _claim: None,
            checking: Arc::default(),
            undo: None,
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
            #[cfg(test)]
            step_hook: None,
            #[cfg(test)]
            pace: None,
            #[cfg(test)]
            rate: None,
            journal_fs: journal::disk(),
            #[cfg(test)]
            retries: Arc::default(),
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

    /// Whether Save can replace the original here (Save copy always can). A streamed file's
    /// `Origin` connects on its first use (06 §2): until then nothing is asked of it and the
    /// answer is yes — the first Save asks before it sends anything that changes the remote.
    pub fn can_replace(&self) -> bool {
        if self.stream.is_some() && !self.connected {
            return true;
        }
        self.origin.can_replace()
    }

    /// One of sopen's own writes landed (06 §5): the stream is reloaded at once, onto the file
    /// the write's own baseline names — `Live` on the saved file, or `Changed` when the remote
    /// is already another one (the write's notes say so). What is returned must be read: the
    /// view could not be reloaded.
    fn reload_saved(&self, baseline: Fingerprint) -> Option<String> {
        let source = self.stream.as_ref()?;
        let e = source.reload_saved(baseline).err()?;
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

    /// Save of a file with a local base (spec §5, §6a; 07 §4.1), in this order: can this server
    /// replace a file in one step (refused before anything is written if not) → one pass over
    /// the bytes as opened that writes the backup of the original (the session's first Save;
    /// printed) and the stage, refused when those bytes changed → the conflict-checked
    /// overwrite, checked against the content hash from that pass when the baseline has none (a
    /// local file opened in place). A refusal or a transport error keeps the edits in a local
    /// file whose path the message names. A streamed file saves by [`HexFile::save_streamed`].
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

    /// Save copy of a file with a local base: `<stem>.edited.<ext>` next to the original (`-N`
    /// when taken), never replacing anything; staged by the same pass as Save (refused when the
    /// bytes as opened changed, the changes listed). Returns the printed path of the new file.
    /// A streamed file's is [`HexFile::copy_streamed`].
    pub fn save_copy(&mut self, patches: &BTreeMap<u64, u8>) -> Result<String> {
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

// ---------------------------------------------------------------------------------------------
// A streamed file's Save, Save copy and Write in place (07 §4.2–§4.4, §6): ED1's, on the
// viewer's `Origin`, recorded in the journal before anything changes the remote.

/// What a streamed Save writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Way {
    /// Over the original, by a temp and a rename (§4.2, §4.3).
    Save,
    /// A new sibling `<stem>.edited<ext>` (§6).
    Copy,
    /// Into the original itself (§4.4), only from its dialog.
    InPlace,
}

/// A streamed Save as the window hands it over.
pub(super) struct Request {
    pub way: Way,
    /// The changed bytes, offset → byte wanted: what the file holds if it lands.
    pub changes: BTreeMap<u64, u8>,
    /// The journal snapshot of the window's edits: their baseline, evidence and text.
    pub snap: journal::Snapshot,
    /// The window's reload count: a version reloaded by the owner gets an undo file of its own.
    pub session: u64,
}

/// Where a streamed Save is (the status row), its cancel (Esc), and whether it has kept the
/// edits at its start in the journal (the window's `kept` line says so only then).
#[derive(Default)]
pub(super) struct Saving {
    pub progress: Mutex<Option<Progress>>,
    pub cancel: AtomicBool,
    pub kept: AtomicBool,
    /// Why the edits could not be kept at the Save's start, when its worker could say.
    pub kept_why: Mutex<Option<String>>,
}

/// How a streamed Save ended when it did not fail.
#[derive(Debug)]
pub(super) enum Streamed {
    /// It landed (Save, Write in place): the next baseline and evidence, the undo file, what was
    /// checked; `notes` what the owner must hear besides.
    Saved {
        saved: patch::Saved,
        notes: Vec<String>,
    },
    /// Save copy landed: the copy as `host:/abs`.
    Copied {
        path: String,
        checked: Option<String>,
        notes: Vec<String>,
    },
    /// Nothing was written. `other_ways`: the refusal's row offers Write in place… and Save As…
    /// (§4.1).
    Refused {
        why: String,
        other_ways: bool,
        notes: Vec<String>,
    },
    /// A Save of this session that was cut off (its outcome unknown) had landed after all: found
    /// by the reconciliation that starts the next one, so nothing was written again.
    FoundLanded {
        line: String,
        next: Option<(Fingerprint, journal::Seen)>,
    },
    /// A Save of this session that was cut off did not land, and left a file at its temp path
    /// that sopen cannot prove its own (§3.1): nothing was written; the record stays, holding
    /// the question, until the owner answers it — Remove it or Keep it.
    Ask(LeftTemp),
}

/// The connection a streamed Save runs on: the `Origin`, connected now if it was not (06 §2).
/// A session sshd refuses says 06 §6's line; nothing was sent.
fn connect<'a>(origin: &'a mut Origin, connected: &mut bool) -> Result<&'a mut dyn PatchFs> {
    let shown = origin.display();
    let fs = origin
        .fs()
        .map_err(|e| anyhow!("{shown} was not touched: {}", crate::stream::explain(&e)))?;
    *connected = true;
    fs.patcher().with_context(|| {
        format!("{shown} was not touched: this connection cannot write part of a file")
    })
}

impl HexFile {
    /// Saves a streamed file's edits (07 §4.2–§4.4, §6), in this order:
    ///
    /// 1. the edits kept in the journal at once (the window's, or one made now);
    /// 2. the `Origin`, connected now if it was not — a refused session fails here, nothing sent;
    ///    then a Save the journal still records (one of this session cut off with no reply)
    ///    reconciled from fresh reads before anything is sent again: not landed, its provable
    ///    temp removed and the record ended; landed, nothing is written again; anything else
    ///    refused, the line naming what to check;
    /// 3. at most 256 MiB, the baseline pass over the version as opened, waited for;
    /// 4. ED1's Save, Save copy or Write in place, which records each phase in the journal
    ///    before the request that needs it.
    ///
    /// A Save that lands retires the journal (no edit is made while it runs, so nothing is left
    /// unsaved) and reloads the stream onto the saved file. A refusal or a failure leaves the
    /// journal in `slot` with the edits.
    pub(super) fn save_streamed(
        &mut self,
        req: &Request,
        slot: &Mutex<Option<Journal>>,
        saving: &Saving,
        wake: &(dyn Fn() + Sync),
    ) -> Result<Streamed> {
        let source = self
            .stream
            .clone()
            .context("a streamed save needs its stream")?;
        let (host, abs) = (self.origin.host().to_string(), self.origin.abs.clone());
        let shown = self.origin.display();
        let cache = self.cache_root.clone();
        let checking = self.checking.clone();
        let mut snap = req.snap.clone();
        // The edits are kept at once, before anything else (07 §3): whatever becomes of this
        // Save — a refused session included — they are in the journal.
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        // A cut-off Save's record stays beside it (its generation is kept until reconciled).
        let kept = match slot.as_mut() {
            Some(journal) => journal.publish(&snap).map(|_| ()),
            None => Journal::create(self.journal_fs.clone(), &cache, &host, &abs, Kind::Hex)
                .and_then(|made| slot.insert(made).publish(&snap).map(|_| ())),
        };
        if let Err(e) = kept {
            *saving.kept_why.lock().unwrap_or_else(|p| p.into_inner()) = Some(format!("{e:#}"));
            return Err(e.context(format!(
                "{shown} was not touched: the edits could not be kept in a journal"
            )));
        }
        let journal = slot.as_mut().expect("kept above");
        saving.kept.store(true, Ordering::SeqCst);
        let fs = connect(&mut self.origin, &mut self.connected)?;
        if let Some(intent) = journal.pending_save().cloned() {
            let found = journal::reconcile(&intent, &host, &mut patch::Fresh::new(fs));
            match found.outcome {
                // A temp of uncertain ownership: the record keeps the question (as on open).
                journal::Outcome::NotLanded => match &found.temp {
                    journal::Temp::Keep { path, why } => {
                        return Ok(Streamed::Ask(LeftTemp {
                            path: path.clone(),
                            line: format!("{why} — remove it?"),
                            seen: looked_at(fs, path),
                            proven: false,
                        }));
                    }
                    temp => {
                        // Provably this Save's: removed now. A removal that fails keeps the
                        // record, and the question is asked — Remove it tries again, Keep it
                        // ends it (as on open).
                        if let journal::Temp::Remove(temp) = temp {
                            if let Err(e) = fs.remove(temp) {
                                return Ok(Streamed::Ask(LeftTemp {
                                    path: temp.clone(),
                                    line: format!(
                                        "the interrupted save's temporary file {host}:{temp} \
                                         could not be removed ({e:#}) — remove it?"
                                    ),
                                    seen: looked_at(fs, temp),
                                    proven: true,
                                }));
                            }
                        }
                        journal.clear_save()?;
                    }
                },
                journal::Outcome::Landed => {
                    journal.clear_save()?;
                    return Ok(Streamed::FoundLanded {
                        line: found.line.replace("(found on reopen)", "(found now)"),
                        next: found.next,
                    });
                }
                journal::Outcome::InPart | journal::Outcome::Unknown => {
                    return Ok(Streamed::Refused {
                        why: found.line,
                        other_ways: false,
                        notes: Vec::new(),
                    });
                }
            }
        }
        let pass = if snap.baseline.size <= HASH_CHECK_MAX && snap.baseline.sha256.is_none() {
            let opened = source.opened();
            if (opened.size, opened.mtime) != (snap.baseline.size, snap.baseline.mtime) {
                bail!("{shown} was not touched: the view is of another version now; save again");
            }
            let pass = checking
                .baseline(&source, &|| saving.cancel.load(Ordering::SeqCst))
                .with_context(|| format!("{shown} was not touched: it could not be checked"))?;
            snap.baseline.sha256 = Some(pass.sha256);
            Some(pass)
        } else {
            None
        };
        let runs = patch::runs_of(&req.changes);
        let job = patch::Job {
            host: &host,
            target: &abs,
            runs: &runs,
            pass: pass.as_ref(),
            rate: source.rate(),
        };
        #[cfg(test)]
        let job = patch::Job {
            rate: self.rate.or(job.rate),
            ..job
        };
        let mut report = |p: Progress| {
            *saving.progress.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
            wake();
        };
        let mut clock = patch::stopwatch();
        let mut ctl = patch::Ctl::new(&saving.cancel, &mut report, &mut clock);
        #[cfg(test)]
        {
            if let Some(pace) = &self.pace {
                ctl.pace = (**pace).clone();
            }
            ctl.hook = match self.step_hook.as_deref_mut() {
                Some(hook) => Some(hook),
                None => None,
            };
        }
        let outcome = match req.way {
            Way::Copy => {
                return Ok(match patch::save_copy(fs, &job, &snap, &mut ctl)? {
                    patch::CopyOutcome::Saved {
                        path,
                        checked,
                        notes,
                    } => Streamed::Copied {
                        path: format!("{host}:{path}"),
                        checked,
                        notes,
                    },
                    patch::CopyOutcome::Refused { why, notes } => Streamed::Refused {
                        other_ways: why.offers_other_ways(),
                        why: why.to_string(),
                        notes,
                    },
                })
            }
            Way::Save | Way::InPlace => {
                if self
                    .undo
                    .as_deref()
                    .is_none_or(|(session, _)| *session != req.session)
                {
                    let undo = UndoFile::new(&cache, &host, &abs, snap.baseline, SystemTime::now());
                    self.undo = Some(Box::new((req.session, undo)));
                }
                let undo = &mut self.undo.as_mut().expect("made above").1;
                if req.way == Way::Save {
                    patch::save(fs, &job, journal, &snap, undo, &mut ctl)?
                } else {
                    patch::write_in_place(fs, &job, journal, &snap, undo, &mut ctl)?
                }
            }
        };
        match outcome {
            patch::SaveOutcome::Saved(saved) => {
                let mut notes = saved.notes.clone();
                if let Err(e) = slot.take().expect("held").save_landed(None) {
                    notes.push(format!(
                        "saved, but its journal could not be removed ({e:#})"
                    ));
                }
                notes.extend(self.reload_saved(saved.next));
                Ok(Streamed::Saved { saved, notes })
            }
            patch::SaveOutcome::Refused { why, notes } => Ok(Streamed::Refused {
                other_ways: why.offers_other_ways(),
                why: why.to_string(),
                notes,
            }),
        }
    }

    /// The path the session's undo file has, or will have once the first Save reserves it (the
    /// Write in place dialog names it).
    pub(super) fn undo_path(&self, session: u64, baseline: Fingerprint) -> PathBuf {
        match self.undo.as_deref() {
            Some((s, undo)) if *s == session => undo.path().to_path_buf(),
            _ => UndoFile::new(
                &self.cache_root,
                self.origin.host(),
                &self.origin.abs,
                baseline,
                SystemTime::now(),
            )
            .path()
            .to_path_buf(),
        }
    }

    /// Removes the file a question about an interrupted Save's temp was about, after the owner
    /// chose to (its row) — that file only. With the question's record (`record`), the record
    /// is reconciled again first, from fresh reads: a temp it proves the Save's is removed as a
    /// reconcile removes one; one it proved before and no longer does is left. Otherwise the
    /// file at the path now must be a regular file with every attribute the question saw (SFTP
    /// has no inode to compare; a file put there since with all of them equal is not told
    /// apart). Anything else — another file, one that cannot be looked at, a question that could
    /// not look at its file — is left, and said.
    pub(super) fn remove_left(
        &mut self,
        left: &LeftTemp,
        record: Option<&journal::SaveIntent>,
    ) -> Result<()> {
        let shown = self.origin.show(&left.path);
        let host = self.origin.host().to_string();
        let fs = connect(&mut self.origin, &mut self.connected)?;
        if let Some(intent) = record {
            let found = journal::reconcile(intent, &host, &mut patch::Fresh::new(&mut *fs));
            match &found.temp {
                journal::Temp::Remove(path) if *path == left.path => {
                    return fs
                        .remove(path)
                        .with_context(|| format!("{shown} was not removed"));
                }
                journal::Temp::Gone(_) => return Ok(()),
                _ if left.proven => bail!(
                    "{shown} is not the file the question was about (the interrupted save's \
                     record no longer proves it its own) — not removed"
                ),
                _ => {}
            }
        } else if left.proven {
            bail!("{shown} cannot be checked against the interrupted save's record — not removed");
        }
        let Some(seen) = left.seen else {
            bail!(
                "{shown} could not be looked at when the question was asked, so it cannot be told \
                 from another file — not removed"
            );
        };
        let now = fs
            .lstat_opt(&left.path)
            .with_context(|| format!("{shown} could not be looked at — not removed"))?;
        match now {
            // Gone already: nothing to remove.
            None => Ok(()),
            Some(now) if now.missing.is_none() && seen.matches(&now) => fs
                .remove(&left.path)
                .with_context(|| format!("{shown} was not removed")),
            Some(now) => bail!(
                "{shown} is not the file the question was about (it is {} now, modified {}) — \
                 not removed",
                bytes_word(now.size as usize),
                crate::cache::rfc3339(now.mtime)
            ),
        }
    }

    /// The owner's answer to a live session's question about a temp (`Streamed::Ask`): the
    /// record must still be the one that asked it (naming that temp); `remove` then removes the
    /// file it was about ([`HexFile::remove_left`] — a failure keeps the question and the
    /// record). Then the record ends, and the next Save goes ahead.
    pub(super) fn answer_left(
        &mut self,
        left: &LeftTemp,
        remove: bool,
        slot: &Mutex<Option<Journal>>,
    ) -> Result<String> {
        let shown = self.origin.show(&left.path);
        if remove {
            let asked = slot
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .and_then(|j| j.pending_save().cloned());
            let Some(intent) = asked.filter(|i| record_temp(i) == Some(left.path.as_str())) else {
                bail!("the interrupted save's record no longer names {shown} — not removed");
            };
            self.remove_left(left, Some(&intent))?;
        }
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(journal) = slot.as_mut().filter(|j| j.pending_save().is_some()) {
            journal.clear_save()?;
        }
        Ok(if remove {
            format!("removed {shown}")
        } else {
            format!("kept {shown}; it will not be asked about again")
        })
    }

    /// The window's baseline as opened: the stream's fingerprint, or the local base's.
    pub(super) fn opened(&self) -> Fingerprint {
        match (&self.stream, &self.local) {
            (Some(source), _) => source.opened(),
            (None, Some(local)) if self.in_place() => Fingerprint {
                size: local.opened.len,
                mtime: local.opened.mtime.unwrap_or(SystemTime::UNIX_EPOCH),
                sha256: None,
            },
            _ => self.origin.at.fingerprint(),
        }
    }

    /// `(host, abs, cache)`: where this file's journals and undo files are.
    pub(super) fn place(&self) -> (String, String, PathBuf) {
        (
            self.origin.host().to_string(),
            self.origin.abs.clone(),
            self.cache_root.clone(),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// The journal's text for hex edits (07 §3 `edits`).

/// The journal's `edits` of a hex window: the `.hexpatch` lines, one run of changed bytes a line
/// with the bytes it replaced — `0xOFF: NEW was OLD`. `changes`: offset → (wanted, replaced).
pub(super) fn edits_text(changes: &BTreeMap<u64, (u8, u8)>) -> String {
    let mut text = String::from(
        "# sopen hex edits — one line per run of changed bytes: offset: the bytes wanted (hex) \
         was the bytes replaced\n",
    );
    let mut runs: Vec<(u64, Vec<u8>, Vec<u8>)> = Vec::new();
    for (&off, &(new, old)) in changes {
        match runs.last_mut() {
            Some((start, n, o)) if *start + n.len() as u64 == off => {
                n.push(new);
                o.push(old);
            }
            _ => runs.push((off, vec![new], vec![old])),
        }
    }
    for (start, new, old) in runs {
        text.push_str(&format!(
            "0x{start:X}: {} was {}\n",
            crate::cache::hex(&new).to_uppercase(),
            crate::cache::hex(&old).to_uppercase()
        ));
    }
    text
}

/// The runs of a journal's hex `edits` (offset, the bytes wanted); `#` lines and blank lines are
/// not edits.
pub(super) fn parse_edits(text: &str) -> Result<Vec<(u64, Vec<u8>)>> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = (|| -> Result<(u64, Vec<u8>)> {
            let (off, rest) = line.split_once(": ").context("no `: `")?;
            let off = off.strip_prefix("0x").context("an offset is 0x…")?;
            let off = u64::from_str_radix(off, 16)?;
            let new = rest.split_once(" was ").map_or(rest, |(new, _)| new);
            Ok((off, super::parse_bytes(new)?))
        })();
        out.push(parsed.with_context(|| format!("`{line}` is not a hex edit"))?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Save As (07 §7): the edits applied to the file read fresh, into `gui::save_as`'s `part`.

/// What one fill read: the source's stat around it (a streamed file's `fstat`; a local base's
/// identity), the hashes of the edited pages as read, and the whole file's.
struct Read {
    changed: Option<String>,
    pages: BTreeMap<u64, Sha>,
    whole: Option<Sha>,
    /// The whole SHA-256 the file must have (its baseline, at most 256 MiB), when known.
    want: Option<Sha>,
    bytes: u64,
}

/// The hex window's [`Producer`](super::super::save_as::Producer): the source read fresh — a
/// streamed file on the `Origin` through a handle opened at the start of each fill, with up to
/// 4 MiB in flight (never the view's cache), a local base through its handle — the patches
/// applied on the way into `part`. Its check: the source's `(size, mtime)` still the baseline,
/// the edited pages as the window showed them, and at most 256 MiB the whole hash the baseline's.
pub(super) struct HexProducer {
    pub file: Arc<Mutex<HexFile>>,
    pub changes: BTreeMap<u64, u8>,
    /// The window's evidence: each edited page as shown.
    pub seen: BTreeMap<u64, Sha>,
    pub baseline: Fingerprint,
    /// `SOPEN_SAVE_AS_HOLD_MS` (a receipt): the fill holds this long at its first progress at or
    /// past 30 %, and says so in `held`.
    pub hold: Option<Duration>,
    pub held: Option<Arc<AtomicBool>>,
    /// Tests: the hold lasts until this is set (or the save is cancelled) instead of `hold`.
    #[cfg(test)]
    pub release: Option<Arc<AtomicBool>>,
    read: Option<Read>,
}

impl HexProducer {
    pub fn new(
        file: Arc<Mutex<HexFile>>,
        changes: BTreeMap<u64, u8>,
        seen: BTreeMap<u64, Sha>,
        baseline: Fingerprint,
    ) -> Self {
        Self {
            file,
            changes,
            seen,
            baseline,
            hold: None,
            held: None,
            #[cfg(test)]
            release: None,
            read: None,
        }
    }
}

/// Takes the file's bytes in order: the patches applied, the old bytes hashed whole and by
/// edited page.
struct Through<'a> {
    changes: &'a BTreeMap<u64, u8>,
    pages: BTreeMap<u64, sha2::Sha256>,
    whole: Option<sha2::Sha256>,
}

impl<'a> Through<'a> {
    fn new(changes: &'a BTreeMap<u64, u8>, whole: bool) -> Self {
        let pages = changes
            .keys()
            .map(|at| (at / journal::PAGE * journal::PAGE, sha2::Sha256::new()))
            .collect();
        Self {
            changes,
            pages,
            whole: whole.then(sha2::Sha256::new),
        }
    }

    /// `chunk` is what the file holds at `at`; returns it with the patches applied.
    fn feed(&mut self, at: u64, chunk: &[u8]) -> Vec<u8> {
        let end = at + chunk.len() as u64;
        if let Some(h) = self.whole.as_mut() {
            h.update(chunk);
        }
        for (&page, h) in self
            .pages
            .range_mut(at / journal::PAGE * journal::PAGE..end)
        {
            let (from, to) = (page.max(at), (page + journal::PAGE).min(end));
            if from < to {
                h.update(&chunk[(from - at) as usize..(to - at) as usize]);
            }
        }
        let mut out = chunk.to_vec();
        for (&off, &b) in self.changes.range(at..end) {
            out[(off - at) as usize] = b;
        }
        out
    }

    fn finish(self) -> (BTreeMap<u64, Sha>, Option<Sha>) {
        let pages = self
            .pages
            .into_iter()
            .map(|(p, h)| (p, h.finalize().into()))
            .collect();
        (pages, self.whole.map(|h| h.finalize().into()))
    }
}

impl super::super::save_as::Producer for HexProducer {
    fn fill(&mut self, part: &mut File, ctl: &super::super::save_as::Ctl<'_>) -> Result<u64> {
        self.read = None;
        let mut file = self.file.lock().unwrap_or_else(|p| p.into_inner());
        let size = self.baseline.size;
        let whole = size <= HASH_CHECK_MAX;
        let hold = self.hold;
        let held_flag = self.held.clone();
        #[cfg(test)]
        let release = self.release.clone();
        let mut held = false;
        let mut through = Through::new(&self.changes, whole);
        let mut write = |at: u64, chunk: &[u8]| -> Result<()> {
            if ctl.cancelled() {
                return Err(anyhow!(crate::remote::Cancelled));
            }
            part.write_all(&through.feed(at, chunk))?;
            let done = at + chunk.len() as u64;
            ctl.progress(done);
            if let Some(hold) = hold.filter(|_| !held && done * 10 >= size * 3) {
                held = true;
                if let Some(flag) = &held_flag {
                    flag.store(true, Ordering::SeqCst);
                }
                let until = std::time::Instant::now() + hold;
                let due = || {
                    #[cfg(test)]
                    if let Some(release) = &release {
                        return release.load(Ordering::SeqCst);
                    }
                    std::time::Instant::now() >= until
                };
                while !due() && !ctl.cancelled() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        };
        let (changed, bytes, want) = match file.stream.clone() {
            Some(source) => {
                // The whole hash to hold the read to, at most 256 MiB: the pass over the version
                // as opened, unless a Save since gave the baseline its own.
                let want = match self.baseline.sha256 {
                    Some(sha) => Some(sha),
                    None if whole => {
                        Some(file.checking.baseline(&source, &|| ctl.cancelled())?.sha256)
                    }
                    None => None,
                };
                let abs = file.origin.abs.clone();
                let file = &mut *file;
                let fs = connect(&mut file.origin, &mut file.connected)?;
                let mut src = fs.open(&abs, patch::OpenFor::Read)?;
                let read = (|| -> Result<(Option<String>, u64)> {
                    let before = fs.fstat(&mut src)?;
                    let n = fs.read_through(&mut src, size, None, &mut |at, chunk| {
                        write(at, chunk).map(|()| None)
                    })?;
                    let after = fs.fstat(&mut src)?;
                    let baseline = &self.baseline;
                    let differs = |s: &crate::remote::Stat| {
                        (s.size != baseline.size || s.mtime != baseline.mtime)
                            .then(|| crate::remote::describe_change(baseline, s))
                    };
                    Ok((differs(&before).or_else(|| differs(&after)), n))
                })();
                fs.close(src);
                let (changed, n) = read?;
                (changed, n, want)
            }
            None => {
                let local = file.local()?;
                let before = Ident::of(&local.file, &local.path)?;
                let mut buf = vec![0u8; CHUNK];
                let mut at = 0u64;
                while at < size {
                    let n = (size - at).min(CHUNK as u64) as usize;
                    local
                        .file
                        .read_exact_at(&mut buf[..n], at)
                        .with_context(|| format!("cannot read {}", local.path.display()))?;
                    write(at, &buf[..n])?;
                    at += n as u64;
                }
                let after = Ident::of(&local.file, &local.path)?;
                let changed = [before, after]
                    .iter()
                    .find(|now| **now != local.opened)
                    .map(|now| local.opened.change(now));
                (changed, at, self.baseline.sha256)
            }
        };
        let (pages, whole) = through.finish();
        self.read = Some(Read {
            changed,
            pages,
            whole,
            want,
            bytes,
        });
        Ok(bytes)
    }

    fn check(&mut self, written: u64) -> std::result::Result<(), String> {
        let read = self.read.take().ok_or("nothing was read")?;
        let shown = self
            .file
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .origin
            .display();
        let why = if read.bytes != self.baseline.size || written != self.baseline.size {
            Some(format!(
                "it ended after {} of {} bytes",
                grouped(read.bytes),
                grouped(self.baseline.size)
            ))
        } else if let Some(changed) = read.changed {
            Some(changed)
        } else if let Some((page, _)) = self
            .seen
            .iter()
            .find(|(page, sha)| read.pages.get(page).is_some_and(|now| now != *sha))
        {
            Some(format!(
                "the bytes under your edits at 0x{page:X} are not the ones shown"
            ))
        } else if read.want.is_some() && read.whole.is_some() && read.want != read.whole {
            Some("its content changed (same size and modification time)".to_string())
        } else {
            None
        };
        match why {
            Some(why) => Err(format!("{shown} changed while it was read ({why})")),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What an earlier session left (07 §3 Recovery, §3.1, §5.1): the recovery rows.

/// One recovery row.
pub(super) enum Row {
    /// A journal of an earlier session: what Restore may do, and the edits it holds.
    Edits {
        orphan: journal::Orphan,
        line: String,
        check: journal::Restore,
        edits: Vec<(u64, Vec<u8>)>,
        temp: Option<LeftTemp>,
    },
    /// A journal that holds nothing that can be applied here: kept and named.
    Kept {
        orphan: journal::Orphan,
        line: String,
        temp: Option<LeftTemp>,
    },
    /// An undo file (§5.1): Load as edits.
    Undo {
        line: String,
        size: u64,
        runs: Vec<Patch>,
    },
    /// A file at the temp path of a Save that landed, which sopen could not verify is its own:
    /// its removal is offered, never done unasked (§3.1). (A Save that did not land keeps the
    /// question in its journal's row: [`LeftTemp`].)
    Temp {
        path: String,
        line: String,
        seen: Option<journal::FileStat>,
    },
    /// What was settled (a landed Save found, a provable temp removed): said once.
    Note(String),
}

/// A file at an interrupted Save's temp path that sopen could not verify is its own (§3.1): its
/// removal is offered, never done unasked. The question lives in the journal's `save` record,
/// which stays until it is answered — Remove it, or Keep it — so Later asks it again on the
/// next open; Restore and Discard of that journal wait for the answer.
#[derive(Debug, Clone)]
pub(super) struct LeftTemp {
    pub path: String,
    pub line: String,
    /// The file at `path` when the question was asked ([`looked_at`]): Remove it removes that
    /// file only.
    pub seen: Option<journal::FileStat>,
    /// The record proved the file the Save's own (its removal failed): Remove it removes it
    /// only while the record's check — the one a reconcile makes, contents included from
    /// `staged` on — still proves it.
    pub proven: bool,
}

/// The attributes of the regular file at `path` (its `lstat`, every attribute there), or
/// `None`: nothing there, a link or another kind of file, an attribute the server left out, or
/// no answer.
pub(super) fn looked_at(fs: &mut dyn PatchFs, path: &str) -> Option<journal::FileStat> {
    match fs.lstat_opt(path) {
        Ok(Some(stat)) if stat.is_file && !stat.is_symlink && stat.missing.is_none() => {
            Some(journal::FileStat::of(&stat))
        }
        _ => None,
    }
}

/// The temp a Save's record names (none for Write in place).
pub(super) fn record_temp(intent: &journal::SaveIntent) -> Option<&str> {
    match &intent.phase {
        journal::Phase::Naming { temp }
        | journal::Phase::Created { temp, .. }
        | journal::Phase::Staged { temp, .. }
        | journal::Phase::Renaming { temp, .. } => Some(temp),
        journal::Phase::Writing { .. } => None,
    }
}

/// `2026-11-20 11:42 UTC`.
fn when(t: SystemTime) -> String {
    let r = crate::cache::rfc3339(t);
    format!("{} {} UTC", &r[..10], &r[11..16])
}

/// `1 byte`, `4 bytes`.
fn bytes_word(n: usize) -> String {
    if n == 1 {
        "1 byte".to_string()
    } else {
        format!("{} bytes", grouped(n as u64))
    }
}

/// `<file>.journal-<UTC stamp>-<8 hex>` exactly, as `Journal::create` names one.
fn journal_name(entry: &str, file: &str) -> bool {
    let Some(id) = entry
        .strip_prefix(file)
        .and_then(|rest| rest.strip_prefix(".journal-"))
    else {
        return false;
    };
    let b = id.as_bytes();
    b.len() == 25
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z'
        && b[16] == b'-'
        && b[17..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

/// The journals of this file (`abs` on `host`) on disk: each folder named as a journal of it
/// and — where its generation can be read — written for this host and path (a journal of
/// another file whose cache path maps to the same place is not this file's: the listing skips
/// it). One whose generation cannot be read counts: the listing names it too, once its lock is
/// free. What a listing should find once no other session holds one.
pub(super) fn this_files_journals(cache: &Path, host: &str, abs: &str) -> Vec<PathBuf> {
    let base = crate::cache::edits_path(cache, host, abs);
    let (Some(home), Some(name)) = (base.parent(), base.file_name()) else {
        return Vec::new();
    };
    let file = name.to_string_lossy();
    let head_is_ours = |dir: &Path| -> bool {
        let gen = std::fs::read(dir.join("CURRENT"))
            .ok()
            .and_then(|bytes| journal::parse_current(&bytes).ok())
            .and_then(|at| std::fs::read(dir.join(format!("gen-{}", at.n))).ok())
            .and_then(|bytes| journal::parse_generation(&bytes).ok());
        gen.is_none_or(|g| g.head.host == host && g.head.path == abs)
    };
    std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| journal_name(&e.file_name().to_string_lossy(), &file))
        .map(|e| e.path())
        .filter(|dir| head_is_ours(dir))
        .collect()
}

/// How long a journal whose lock is taken is tried again (§3 Recovery: a child another thread
/// is starting holds a copy of the lock's descriptor from its fork to its exec).
const LOCK_RETRY: Duration = Duration::from_millis(500);

/// Undo-file rows shown at most (newest first); the rest are said as a count.
const UNDO_ROWS: usize = 3;

/// The journals, cut-off Saves and undo files an earlier session left for this file, as rows:
/// a recorded Save reconciled first from fresh reads of the remote (a temp removed only when
/// provably this Save's; one whose ownership is uncertain offered for removal), then each
/// journal's edits checked against the file as it is now (`journal::restore_check`: the pages
/// under the edits read fresh, never only the metadata). The `Origin` connects only when there
/// is a journal to check. A journal that holds no edit is removed: there is nothing to offer.
pub(super) fn recover(file: &Mutex<HexFile>) -> Vec<Row> {
    let (host, abs, cache, fs) = {
        let file = file.lock().unwrap_or_else(|p| p.into_inner());
        let (host, abs, cache) = file.place();
        (host, abs, cache, file.journal_fs.clone())
    };
    #[cfg(test)]
    let retries = file
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retries
        .clone();
    let want = this_files_journals(&cache, &host, &abs).len();
    let started = std::time::Instant::now();
    let mut orphans = Vec::new();
    loop {
        match journal::orphans(&fs, &cache, &host, &abs) {
            Ok(found) => orphans = found,
            Err(e) => crate::debug!("journal: no listing: {e:#}"),
        }
        if orphans.len() >= want || started.elapsed() >= LOCK_RETRY {
            break;
        }
        orphans.clear();
        #[cfg(test)]
        retries.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut rows = Vec::new();
    if !orphans.is_empty() {
        let mut file = file.lock().unwrap_or_else(|p| p.into_inner());
        let file = &mut *file;
        let mut fs = connect(&mut file.origin, &mut file.connected);
        for orphan in orphans {
            let inspect: Option<&mut dyn PatchFs> = match &mut fs {
                Ok(fs) => Some(&mut **fs),
                Err(_) => None,
            };
            rows.extend(recover_one(orphan, &host, inspect));
        }
        if let Err(e) = &fs {
            rows.push(Row::Note(format!(
                "the kept edits could not be checked against the file: {e:#}"
            )));
        }
    }
    let undos = patch::UndoFile::list(&cache, &host, &abs);
    let shown = format!("{host}:{abs}");
    for path in undos.iter().take(UNDO_ROWS) {
        let undo = std::fs::read_to_string(path)
            .map_err(anyhow::Error::from)
            .and_then(|text| UndoFile::parse(&text));
        let Ok(undo) = undo else {
            continue;
        };
        if undo.source != shown && host != crate::cache::LOCAL_HOST {
            continue;
        }
        let n: usize = undo.runs.iter().map(|r| r.bytes.len()).sum();
        let saved = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map_or("an earlier session".to_string(), when);
        rows.push(Row::Undo {
            line: format!(
                "bytes replaced by the save at {saved} ({}) kept at {}",
                bytes_word(n),
                super::super::binary::tilde(path)
            ),
            size: undo.size,
            runs: undo.runs,
        });
    }
    if undos.len() > UNDO_ROWS {
        rows.push(Row::Note(format!(
            "{} older undo files of this file are kept beside the ones above",
            undos.len() - UNDO_ROWS
        )));
    }
    rows
}

/// One orphan's rows: its cut-off Save reconciled, then its edits checked.
fn recover_one(
    mut orphan: journal::Orphan,
    host: &str,
    mut fs: Option<&mut dyn PatchFs>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let dir = super::super::binary::tilde(orphan.dir());
    let mut said: Vec<String> = orphan.notes.clone();
    let mut left = None;
    let reconciled = match fs.as_deref_mut() {
        Some(fs) => orphan.reconcile(&mut patch::Fresh::new(fs)),
        None => None,
    };
    if let Some(r) = reconciled {
        match &r.temp {
            journal::Temp::Keep { path, why } if r.outcome == journal::Outcome::Landed => {
                let seen = fs.as_deref_mut().and_then(|fs| looked_at(fs, path));
                rows.push(Row::Temp {
                    path: path.clone(),
                    line: format!("{why} — remove it?"),
                    seen,
                })
            }
            journal::Temp::Keep { path, why } => {
                left = Some(LeftTemp {
                    path: path.clone(),
                    line: format!("{why} — remove it?"),
                    seen: fs.as_deref_mut().and_then(|fs| looked_at(fs, path)),
                    proven: false,
                })
            }
            journal::Temp::Remove(path) if r.outcome == journal::Outcome::NotLanded => {
                // Provably this Save's: removed now. A removal that fails keeps the record, and
                // the question is asked — Remove it tries again, Keep it ends it.
                match fs.as_deref_mut().map(|fs| fs.remove(path)) {
                    Some(Ok(())) => {}
                    Some(Err(e)) => {
                        left = Some(LeftTemp {
                            path: path.clone(),
                            line: format!(
                                "the interrupted save's temporary file {host}:{path} could not be \
                                 removed ({e:#}) — remove it?"
                            ),
                            seen: fs.as_deref_mut().and_then(|fs| looked_at(fs, path)),
                            proven: true,
                        })
                    }
                    None => {}
                }
            }
            _ => {}
        }
        match r.outcome {
            journal::Outcome::Landed => match orphan.landed() {
                Ok(None) => {
                    rows.push(Row::Note(r.line));
                    return rows;
                }
                Ok(Some(later)) => {
                    orphan = later;
                    said.push(r.line);
                }
                Err(e) => {
                    rows.push(Row::Note(format!("{} — its journal stays ({e:#})", r.line)));
                    return rows;
                }
            },
            journal::Outcome::NotLanded => {
                // The record ends here — unless it holds the question about a temp sopen
                // cannot prove its own, which stays until it is answered.
                if left.is_none() {
                    if let Err(e) = orphan.clear_save() {
                        said.push(format!("{e:#}"));
                    }
                }
                said.push(r.line);
            }
            journal::Outcome::InPart | journal::Outcome::Unknown => said.push(r.line),
        }
    } else if !matches!(orphan.save, journal::SaveFile::None) {
        said.push("a save was cut off, and the file could not be read to tell how".to_string());
    }
    let tail = |line: String, said: &[String]| -> String {
        said.iter()
            .fold(line, |line, note| format!("{line}; {note}"))
    };
    match orphan.found.clone() {
        journal::Found::Edits(gen) if gen.head.kind == Kind::Hex => {
            let edits = match parse_edits(&gen.edits) {
                Ok(edits) => edits,
                Err(e) => {
                    let line = format!("edits kept at {dir} cannot be read ({e:#})");
                    rows.push(Row::Kept {
                        line: tail(line, &said),
                        orphan,
                        temp: left,
                    });
                    return rows;
                }
            };
            let n: usize = edits.iter().map(|(_, b)| b.len()).sum();
            if n == 0 && matches!(orphan.save, journal::SaveFile::None) {
                if let Err(e) = orphan.remove() {
                    crate::debug!("journal: an empty one stays: {e:#}");
                }
                return rows;
            }
            // No edits, only the question about an interrupted save's file: that is the row.
            if n == 0 && left.is_some() {
                let line = format!("an interrupted save's record is kept at {dir}");
                rows.push(Row::Kept {
                    line: tail(line, &said),
                    orphan,
                    temp: left,
                });
                return rows;
            }
            let check = match fs {
                Some(fs) => journal::restore_check(&gen, Kind::Hex, &mut patch::Fresh::new(fs)),
                None => journal::Restore::Unchecked {
                    why: "the file could not be read".to_string(),
                },
            };
            let line = format!(
                "unsaved edits from {} ({} changed) kept at {dir}",
                when(gen.head.created),
                bytes_word(n)
            );
            let line = match &check {
                journal::Restore::Apply { notes } => tail(line, &[&said[..], notes].concat()),
                journal::Restore::Refuse { why } | journal::Restore::Unchecked { why } => {
                    tail(format!("{line} — not restored: {why}"), &said)
                }
                journal::Restore::Ask => tail(format!("{line}: {}", journal::ASK_PENDING), &said),
                journal::Restore::OtherKind(_) => tail(line, &said),
            };
            rows.push(Row::Edits {
                orphan,
                line,
                check,
                edits,
                temp: left,
            });
        }
        journal::Found::Edits(gen) => {
            let line = format!(
                "edits made in the {} window are kept at {dir}; the hex window does not apply them",
                gen.head.kind.word()
            );
            rows.push(Row::Kept {
                line: tail(line, &said),
                orphan,
                temp: left,
            });
        }
        journal::Found::Unusable { why, .. } => {
            let line = format!("edits kept at {dir} cannot be restored: {why}");
            rows.push(Row::Kept {
                line: tail(line, &said),
                orphan,
                temp: left,
            });
        }
    }
    rows
}
