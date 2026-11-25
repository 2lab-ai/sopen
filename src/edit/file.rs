//! The editor's file (`.prd/12-editor.md` §7.4, §9; ED-save): the whole read and its baseline,
//! Save by §9.1's steps, its conflicts and Overwrite, Save copy and Save As, and the journal that
//! keeps the edits until they are saved (§9.5). Screen-free: what a Save did comes back as
//! [`Saved`], which ED-layout turns into §9.3, and the prompts are ED-dialogs'.
//!
//! - **The read** ([`read_local`], [`read_stream`]) holds the file whole (at most
//!   [`HASH_CHECK_MAX`], E13) as the buffer's [`Original`], hashing it on the way. Its
//!   [`Baseline`] is what every Save checks the file against: the size, mtime and SHA-256, and
//!   for a local file its device and inode. A local path is read as the opening names it — its
//!   real path, through any link (`realpath`, as every opening's `abs` is).
//! - **Save** ([`Session::save`]): refused where the server cannot replace a file in one step;
//!   a Save of this session that was cut off reconciled first (07 §3.1); the session's first Save
//!   backs the original up — a hard link of the journal's `original`, else a copy; the pieces
//!   staged in `.edits`, hashed; then [`RemoteFs::replace_guarded`] (E32), each of its steps
//!   recorded in the journal's `save` intent before the request that follows it. Landed, the
//!   staged bytes are the next baseline and the undo stack is marked saved; the journal stays,
//!   its next generation against the new baseline (§9.5: one journal per editing session).
//! - **A change** found before anything is replaced is [`Saved::Conflict`]: nothing written
//!   over the file, the staged copy kept and named. [`Session::overwrite`] (E23) reads the file
//!   again into the backup folder and replaces it as a second guarded replace against that read.
//! - **The journal** ([`Session::keep`]) is created at the first edit. Its `original` is written
//!   once on a worker, and no generation is published before it is durable (`keeping…`); with no
//!   room for it nothing is kept (`NOT kept on disk`). The add buffer goes into `add-<n>` chunks,
//!   each written once. A re-read ([`Session::reread`]) writes the new original, generation `k`.
//! - **Restore** ([`restore`]) rebuilds the buffer from the journal alone — the original it was
//!   made on, the chunks, the pieces — and never applies a piece to the file; a file changed since
//!   makes the next Save ask.

use super::buffer::{Buffer, Original, Span};
use super::ops::{Doubt, Editor};
use crate::encoding::Encoding;
use crate::gui::binary::{SaveError, SavedTo};
use crate::journal::{
    self, FileStat, Guarded, Journal, Keep, Kind, OriginalRef, Orphan, Other, Plan, PostEvidence,
    Reconciled, Route, Seen, Sha, Snapshot, Temp, TextEdits, TextPiece,
};
use crate::remote::{
    self, FileId, Fingerprint, Guard, Hashing, RemoteFs, Replaced, Stat, Step, Unguarded,
    WriteOutcome, HASH_CHECK_MAX,
};
use anyhow::{anyhow, bail, Context as _, Result};
use std::collections::BTreeSet;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::SystemTime;

/// Why Save cannot replace the file on this server (§9.1 step 1): Save copy still works.
pub const CANNOT_REPLACE: &str = "this server cannot replace a file in one step (no \
     posix-rename@openssh.com) — Save copy writes the edit next to it instead";

/// What a Save checks the file against (12 §3): the size, mtime and SHA-256 of the bytes read
/// (or staged by the last Save), and for a local file its device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Baseline {
    pub size: u64,
    pub mtime: SystemTime,
    pub sha256: Sha,
    pub id: Option<FileId>,
}

impl Baseline {
    fn of(stat: &Stat, sha256: Sha, id: Option<FileId>) -> Baseline {
        Baseline {
            size: stat.size,
            mtime: stat.mtime,
            sha256,
            id,
        }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            size: self.size,
            mtime: self.mtime,
            sha256: Some(self.sha256),
        }
    }
}

/// The whole read (§7.4): the file's path, its bytes as the buffer's original, its baseline.
pub struct Read {
    pub path: String,
    pub original: Original,
    pub baseline: Baseline,
}

fn too_big(path: &str) -> anyhow::Error {
    anyhow!("{path} is over 256 MiB: the editor reads files of at most 256 MiB")
}

/// Bytes read whole, hashed on the way, `progress` told `(done, total)`; [`remote::Cancelled`]
/// once `cancel` is set (Esc goes back to viewing).
fn read_whole(
    src: &mut dyn std::io::Read,
    total: u64,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, Sha)> {
    let mut bytes = Vec::with_capacity(total.min(HASH_CHECK_MAX) as usize);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!(remote::Cancelled);
        }
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        bytes.extend_from_slice(&buf[..n]);
        if bytes.len() as u64 > HASH_CHECK_MAX {
            bail!("over 256 MiB");
        }
        progress(bytes.len() as u64, total);
    }
    let sha = journal::sha256(&bytes);
    Ok((bytes, sha))
}

/// Reads a local file whole for the editor (§7.4): through any link to the file it names, with
/// its SHA-256 taken on the way and its device and inode recorded for E32. Refused above 256 MiB,
/// and when the file changed while it was read.
pub fn read_local(
    path: &str,
    encoding: Encoding,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<Read> {
    use std::os::unix::fs::MetadataExt as _;
    let real = std::fs::canonicalize(path).with_context(|| format!("cannot open {path}"))?;
    let real = real
        .into_os_string()
        .into_string()
        .map_err(|p| anyhow!("{}: path is not valid UTF-8", p.to_string_lossy()))?;
    let mut file = std::fs::File::open(&real).with_context(|| format!("cannot open {real}"))?;
    let before = file.metadata()?;
    if !before.is_file() {
        bail!("{real} is not a regular file");
    }
    if before.len() > HASH_CHECK_MAX {
        return Err(too_big(&real));
    }
    let (bytes, sha) = read_whole(&mut file, before.len(), progress, cancel)
        .with_context(|| format!("cannot read {real}"))?;
    // The baseline is the bytes read: they, and the file after, are the size it had before.
    let after = std::fs::symlink_metadata(&real)?;
    let same = |m: &std::fs::Metadata| {
        (m.dev(), m.ino(), m.len(), m.modified().ok())
            == (
                before.dev(),
                before.ino(),
                before.len(),
                before.modified().ok(),
            )
    };
    if bytes.len() as u64 != before.len() || !same(&after) || !same(&file.metadata()?) {
        bail!("{real} changed while it was read; try again when it is quiet");
    }
    let stat = remote::stat_of(&before);
    let id = FileId {
        dev: before.dev(),
        ino: before.ino(),
    };
    Ok(Read {
        baseline: Baseline::of(&stat, sha, Some(id)),
        original: Original::new(bytes, encoding),
        path: real,
    })
}

/// Reads a file whole for the editor from a byte reader (§7.4): the opening's stream's `Bulk`
/// reader in the product, any reader in the tests. `at` is the opening's stat, the baseline with
/// the SHA-256 of the bytes read; a read of another size than `at`'s is refused.
pub fn read_stream(
    reader: &mut dyn std::io::Read,
    path: &str,
    at: Stat,
    encoding: Encoding,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<Read> {
    if at.size > HASH_CHECK_MAX {
        return Err(too_big(path));
    }
    let (bytes, sha) = read_whole(reader, at.size, progress, cancel)
        .with_context(|| format!("cannot read {path}"))?;
    if bytes.len() as u64 != at.size {
        bail!(
            "{path} changed while it was read ({} bytes, {} read); try again when it is quiet",
            at.size,
            bytes.len()
        );
    }
    Ok(Read {
        path: path.to_string(),
        original: Original::new(bytes, encoding),
        baseline: Baseline::of(&at, sha, None),
    })
}

/// What a Save did (§9.1, §9.2), for ED-layout's §9.3 and ED-dialogs' conflict dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    /// The file holds the document: `bytes` in `encoding`. `backup`: made by this Save (the
    /// session's first, or Overwrite's). `kept`: where another writer's version from the moment
    /// before is kept. `notes`: what the owner must hear, in order. `unguarded`: the guard this
    /// Save ran without; its line is in `notes` the first time for its filesystem or server.
    Written {
        file: String,
        bytes: u64,
        encoding: Encoding,
        backup: Option<PathBuf>,
        kept: Option<String>,
        unguarded: Option<Unguarded>,
        notes: Vec<String>,
    },
    /// The file changed since it was read: nothing was written over it. `line` names the change;
    /// `overwrite`: the file is still a regular file, so Overwrite is offered; the staged copy is
    /// kept at `staged`.
    Conflict {
        line: String,
        overwrite: bool,
        staged: PathBuf,
        notes: Vec<String>,
    },
    /// Nothing was written: this server cannot replace a file in one step. Save copy remains.
    CannotReplace(String),
    /// A Save of this session that was cut off had landed: found now, nothing written again.
    Found { notes: Vec<String> },
}

impl Saved {
    /// The result line (07 §10): `saved host:/abs · backup <path>`, then the notes.
    pub fn line(&self) -> String {
        match self {
            Saved::Written {
                file,
                backup,
                notes,
                ..
            } => {
                let mut line = format!("saved {file}");
                if let Some(backup) = backup {
                    line.push_str(&format!(" · backup {}", backup.display()));
                }
                for note in notes {
                    line.push_str(&format!("; {note}"));
                }
                line
            }
            Saved::Conflict { line, notes, .. } => {
                let mut out = line.clone();
                for note in notes {
                    out.push_str(&format!("; {note}"));
                }
                out
            }
            Saved::CannotReplace(line) => line.clone(),
            Saved::Found { notes } => notes.join("; "),
        }
    }
}

/// The guard's lines already said, by filesystem or server: each is said once (§9.1).
static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Whether `key`'s line is said now: the first time only.
fn first_time(key: String) -> bool {
    SAID.lock().unwrap_or_else(|p| p.into_inner()).insert(key)
}

/// The one line for a write that failed, keeping the edit: a request with no reply is "outcome
/// unknown — check <path>", never "not saved" (as the image and hex windows say it).
fn failed(e: &anyhow::Error, staged: &Path) -> anyhow::Error {
    match remote::outcome_unknown(e) {
        Some(unknown) => anyhow!(
            "{unknown}; your edit is at {} (cause: {})",
            staged.display(),
            e.root_cause()
        ),
        None => anyhow!("{e:#}; not saved — your edit is at {}", staged.display()),
    }
}

/// Fresh reads of the file through its connection, for the journal's checks (07 §3.1, §9.5).
struct Look<'a>(&'a mut dyn RemoteFs);

/// Copies `[offset, offset + len)` of what passes through.
struct Window<'a> {
    at: u64,
    from: u64,
    out: &'a mut Vec<u8>,
    len: usize,
}

impl std::io::Write for Window<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let (start, end) = (self.at, self.at + buf.len() as u64);
        self.at = end;
        let want_end = self.from + self.len as u64;
        let (a, b) = (start.max(self.from), end.min(want_end));
        if a < b {
            self.out
                .extend_from_slice(&buf[(a - start) as usize..(b - start) as usize]);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl journal::Inspect for Look<'_> {
    fn lstat(&mut self, path: &str) -> Result<Option<Stat>> {
        self.0.lstat_opt(path)
    }

    fn read_at(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut w = Window {
            at: 0,
            from: offset,
            out: &mut out,
            len,
        };
        self.0.read_to(path, &mut w, &mut |_| {})?;
        Ok(out)
    }

    fn whole_sha256(&mut self, path: &str, size: u64) -> Result<Option<Sha>> {
        let mut sink = Hashing::new(std::io::sink());
        self.0.read_to(path, &mut sink, &mut |_| {})?;
        let read = sink.finish();
        Ok((read.size == size).then_some(read.sha256))
    }

    fn file_id(&mut self, path: &str) -> Result<Option<FileId>> {
        Ok(self.0.file_id(path)?.map(|(id, _)| id))
    }
}

/// Acts on a reconciliation (§9.1, 07 §3.1): a temp provably the cut-off Save's removed, the
/// other file — the version read — removed, or another writer's version kept beside the file
/// and named. What the owner must hear.
fn act(
    fs: &mut dyn RemoteFs,
    r: &Reconciled,
    target: &str,
    show: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let mut notes = Vec::new();
    match &r.temp {
        Temp::Remove(path) => {
            if let Err(e) = fs.remove(path) {
                notes.push(format!("{} could not be removed: {e:#}", show(path)));
            }
        }
        Temp::Keep { path, why } => notes.push(format!("{} is kept ({why})", show(path))),
        Temp::None | Temp::Gone(_) => {}
    }
    match &r.other {
        Other::Remove(path) => {
            if fs.remove(path).is_err() {
                notes.push(format!("remove {} by hand", show(path)));
            }
        }
        Other::Rename(path) => {
            let at = remote::keep_aside(fs, path, target, &mut |_| Ok(()))
                .unwrap_or_else(|_| path.clone());
            notes.push(format!("the other version is kept at {}", show(&at)));
        }
        Other::Kept(path) => notes.push(format!("the other version is kept at {}", show(path))),
        Other::Keep { path, why } => notes.push(format!("{} is kept ({why})", show(path))),
        Other::None | Other::Gone(_) => {}
    }
    notes
}

/// What a guarded replace told while it ran ([`Guard`]), as the Save records it.
#[derive(Default)]
struct Told {
    /// The `save` record was begun (phase `naming`).
    begun: bool,
    /// Beginning the record failed: it may be on disk, and nothing was sent.
    unrecorded: bool,
    staged: Option<Stat>,
    renamed: bool,
    left: bool,
    ids: Option<(FileId, FileId)>,
}

impl Told {
    /// One step, recorded in the journal's `save` file before the request that follows it.
    fn record(
        &mut self,
        guard: &Guard<'_>,
        journal: Option<&mut Journal>,
        snap: Option<&Snapshot>,
        (target, route, sha): (&str, Route, Sha),
    ) -> Result<()> {
        // What is uploaded must be what was staged, before anything is replaced.
        if let Guard::Step(Step::Staged { uploaded, .. }) = guard {
            if uploaded.sha256 != sha {
                bail!("the upload is not the bytes staged; nothing was replaced");
            }
        }
        let j = journal.filter(|_| self.begun || snap.is_some());
        let Some(j) = j else {
            if let Guard::Step(Step::Staged { stat, .. }) = guard {
                self.staged = Some(**stat);
            }
            self.renamed |= matches!(guard, Guard::Step(Step::Renamed { .. }));
            return Ok(());
        };
        match *guard {
            Guard::Step(Step::Named { temp }) => {
                let plan = Plan::Temp {
                    route,
                    target: target.to_string(),
                    temp: temp.to_string(),
                };
                let Some(snap) = snap else { return Ok(()) };
                // Refused because another Save is recorded, it wrote nothing: that record is
                // not this Save's to clear.
                let earlier = j.pending_save().is_some();
                if let Err(e) = j.begin_save(snap, plan) {
                    self.unrecorded = !earlier;
                    return Err(e.context("the save could not be recorded; nothing was sent"));
                }
                self.begun = true;
            }
            _ if !self.begun => {}
            Guard::Step(Step::Written { stat, .. }) => j.save_created(FileStat::of(stat))?,
            Guard::Step(Step::Staged { stat, .. }) => {
                let staged = FileStat::of(stat);
                j.save_staged(staged, PostEvidence::text(&staged, sha)?)?;
                j.save_renaming()?;
                self.staged = Some(*stat);
            }
            Guard::Step(Step::Renamed { .. }) => self.renamed = true,
            Guard::Step(Step::Removed { .. }) => {}
            Guard::Step(Step::Left { .. }) => self.left = true,
            Guard::Exchanging {
                temp_id, target_id, ..
            } => {
                self.ids = Some((temp_id, target_id));
                j.save_guard(Some(Guarded::Exchanging {
                    temp: temp_id,
                    target: target_id,
                }))?;
            }
            Guard::SwappedBack { .. } => {
                if let Some((temp, target)) = self.ids {
                    j.save_guard(Some(Guarded::SwappedBack { temp, target }))?;
                }
            }
            Guard::Linked { link } => j.save_guard(Some(Guarded::Linked {
                link: link.to_string(),
            }))?,
            Guard::Committed { link } => j.save_guard(Some(Guarded::Committed {
                link: link.to_string(),
            }))?,
            Guard::Verified { link } => j.save_guard(Some(Guarded::Verified {
                link: link.to_string(),
            }))?,
            Guard::Kept { from, kept } => j.save_guard(Some(Guarded::Kept {
                from: from.to_string(),
                kept: kept.to_string(),
            }))?,
            Guard::Unguarded(_) => j.save_guard(None)?,
        }
        Ok(())
    }
}

/// One editing session's file side (§7.4, §9): where the file is, its baseline, the session's
/// backup and the journal.
pub struct Session {
    /// `[user@]host` for a remote file, `None` for a local one.
    label: Option<String>,
    /// The host in cache and backup paths.
    host: String,
    target: String,
    baseline: Baseline,
    cache: PathBuf,
    journal_fs: Arc<dyn journal::Fs>,
    backup: Option<PathBuf>,
    journal: Option<Journal>,
    /// `original`'s write on a worker, until it answers.
    keeping: Option<mpsc::Receiver<Result<OriginalRef>>>,
    /// Why nothing is kept on disk (no room for a copy of the file), once found.
    not_kept: Option<String>,
    /// The add buffer's bytes kept in chunks.
    kept_add: u64,
}

impl Session {
    /// The session of a file just read (§7.4): `label` is `[user@]host` for a remote file; the
    /// journal and the backups go under `cache`.
    pub fn new(
        read: &Read,
        label: Option<&str>,
        cache: &Path,
        journal_fs: Arc<dyn journal::Fs>,
    ) -> Session {
        Session {
            label: label.map(str::to_string),
            host: label.unwrap_or(crate::cache::LOCAL_HOST).to_string(),
            target: read.path.clone(),
            baseline: read.baseline,
            cache: cache.to_path_buf(),
            journal_fs,
            backup: None,
            journal: None,
            keeping: None,
            not_kept: None,
            kept_add: 0,
        }
    }

    pub fn baseline(&self) -> &Baseline {
        &self.baseline
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    /// The session's backup, once its first Save made it.
    pub fn backup(&self) -> Option<&Path> {
        self.backup.as_deref()
    }

    /// The journal's folder, once there is one (`kept at <journal>`).
    pub fn journal_dir(&self) -> Option<&Path> {
        self.journal.as_ref().map(Journal::dir)
    }

    /// `host:/abs`, or the local path.
    pub fn show(&self, path: &str) -> String {
        match &self.label {
            Some(label) => format!("{label}:{path}"),
            None => path.to_string(),
        }
    }

    fn name(&self) -> String {
        remote::split_parent(&self.target).1.to_string()
    }

    /// Starts writing `original` (re-read generation `k`) on a worker — after the one still
    /// writing an earlier generation, if any, so two never write `original.tmp` at once.
    fn keep_original(&mut self, original: Original, k: u32) {
        let Some(j) = &self.journal else { return };
        let keeping = j.keeping();
        let earlier = self.keeping.take();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            if let Some(earlier) = earlier {
                let _ = earlier.recv();
            }
            let _ = tx.send(keeping.write(k, original.bytes()));
        });
        self.keeping = Some(rx);
    }

    /// The journal is given up: removed, and nothing kept until the next edit (or ever, with no
    /// room).
    fn drop_journal(&mut self) {
        if let Some(j) = self.journal.take() {
            if let Err(e) = j.remove() {
                crate::debug!("journal: {e:#}");
            }
        }
        self.keeping = None;
        self.kept_add = 0;
    }

    /// Keeps the edits on disk (§9.5): called at most 250 ms after a change, at focus loss, at
    /// close, and by a Save before it starts. The first edit makes the journal and starts writing
    /// its `original`; until that is durable no generation is published (`keeping…`). Then the
    /// add buffer's new bytes become one `add-<n>` and the pieces one generation. The status.
    pub fn keep(&mut self, editor: &Editor) -> Keep {
        let b = editor.buffer();
        if let Some(why) = &self.not_kept {
            return match b.is_saved() && self.journal.is_none() {
                true => Keep::Clean,
                false => Keep::NotKept(why.clone()),
            };
        }
        if self.journal.is_none() {
            if b.is_saved() {
                return Keep::Clean;
            }
            match Journal::create(
                self.journal_fs.clone(),
                &self.cache,
                &self.host,
                &self.target,
                Kind::Text,
            ) {
                Ok(j) => self.journal = Some(j),
                Err(e) => return Keep::NotKept(format!("{e:#}")),
            }
            self.keep_original(b.original().clone(), b.generation());
        }
        if let Some(status) = self.original_answer(false) {
            return status;
        }
        match self.publish(editor) {
            Ok(Some(_)) if b.is_saved() => Keep::Clean,
            Ok(Some(_)) => Keep::Kept,
            Ok(None) => Keep::Keeping,
            Err(e) => Keep::NotKept(format!("{e:#}")),
        }
    }

    /// The answer of the worker writing `original` — waited for with `block` — taken: the original
    /// kept, or the journal given up (no room). `Some(status)` where [`Session::keep`] stops.
    fn original_answer(&mut self, block: bool) -> Option<Keep> {
        let rx = self.keeping.as_ref()?;
        let lost = || Err(anyhow!("the copy of the file was not written"));
        let answer = match block {
            true => rx.recv().unwrap_or_else(|_| lost()),
            false => match rx.try_recv() {
                Ok(answer) => answer,
                Err(mpsc::TryRecvError::Empty) => return Some(Keep::Keeping),
                Err(mpsc::TryRecvError::Disconnected) => lost(),
            },
        };
        self.keeping = None;
        let kept = answer.and_then(|kept| match &mut self.journal {
            Some(j) => j.original_kept(kept),
            None => Ok(()),
        });
        let e = kept.err()?;
        let why = match journal::is_no_room(&e) {
            true => journal::NoRoom.to_string(),
            false => format!("{e:#}"),
        };
        self.drop_journal();
        self.not_kept = Some(why.clone());
        Some(Keep::NotKept(why))
    }

    /// The add buffer's new bytes kept, then the generation ([`Session::snapshot`]); `None`
    /// while the original the pieces were made on is not durable yet.
    fn publish(&mut self, editor: &Editor) -> Result<Option<journal::GenRef>> {
        let b = editor.buffer();
        let Some(j) = &mut self.journal else {
            return Ok(None);
        };
        if j.original().is_none_or(|o| o.k != b.generation()) {
            return Ok(None);
        }
        if b.add_len() > self.kept_add {
            let parts: Vec<&[u8]> = b.add_chunks(self.kept_add..b.add_len()).collect();
            j.keep_add(&parts)?;
            self.kept_add = b.add_len();
        }
        let Some(snap) = self.snapshot(editor, &self.baseline) else {
            return Ok(None);
        };
        let Some(j) = &mut self.journal else {
            return Ok(None);
        };
        j.publish(&snap).map(Some)
    }

    /// [`snapshot`] of this session's journal.
    fn snapshot(&self, editor: &Editor, baseline: &Baseline) -> Option<Snapshot> {
        snapshot(editor, self.journal.as_ref()?, self.kept_add, baseline)
    }

    /// The session's first Save backs up the original as opened (§9.1 step 3): a hard link of the
    /// journal's `original` where it holds these bytes and `<cache>` is one volume, else a copy,
    /// at `<cache>/.backup/<host>/<abs>.<stamp>` (`-<random>` when taken). Printed.
    fn ensure_backup(&mut self, editor: &Editor) -> Result<PathBuf> {
        if let Some(path) = &self.backup {
            return Ok(path.clone());
        }
        let b = editor.buffer();
        let linked = self.journal.as_ref().and_then(|j| {
            j.original()
                .filter(|o| o.k == b.generation())
                .map(|_| j.dir().join("original"))
        });
        let path = self.new_backup(|path| match &linked {
            Some(from) => match std::fs::hard_link(from, path) {
                Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
                    crate::debug!("backup by link: {e}; copying");
                    write_new(path, b.original().bytes())
                }
                done => done,
            },
            None => write_new(path, b.original().bytes()),
        })?;
        println!(
            "backup of {} at {}",
            self.show(&self.target),
            path.display()
        );
        self.backup = Some(path.clone());
        Ok(path)
    }

    /// A new file in the backup folder, made by `make` at the first free name.
    fn new_backup(&self, mut make: impl FnMut(&Path) -> std::io::Result<()>) -> Result<PathBuf> {
        let first =
            crate::cache::backup_path(&self.cache, &self.host, &self.target, SystemTime::now());
        if let Some(dir) = first.parent() {
            crate::cache::private_dir(dir)?;
        }
        for attempt in 0..32 {
            let path = match attempt {
                0 => first.clone(),
                _ => {
                    let mut name = first.file_name().unwrap_or_default().to_os_string();
                    name.push(format!("-{}", &remote::random_hex()[..8]));
                    first.with_file_name(name)
                }
            };
            match make(&path) {
                Ok(()) => {
                    // The backup's name made durable in its folder.
                    let dir = path.parent().unwrap_or(Path::new("/"));
                    std::fs::File::open(dir)
                        .and_then(|d| d.sync_all())
                        .with_context(|| format!("cannot make {} durable", dir.display()))?;
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("cannot write the backup {}", path.display()))
                }
            }
        }
        bail!("no free name for a backup beside {}", first.display())
    }

    /// The pieces in order, into a new owner-only file under `.edits` (§9.1 step 4), hashed on
    /// the way and fsynced.
    fn stage(&self, editor: &Editor) -> Result<(PathBuf, remote::Uploaded)> {
        let edits = crate::cache::edits_path(&self.cache, &self.host, &self.target);
        let (path, file) = crate::gui::create_keep(&edits, "edited")?;
        let written = (|| -> Result<remote::Uploaded> {
            let mut out = Hashing::new(BufWriter::with_capacity(1 << 20, &file));
            editor.buffer().write_to(&mut out)?;
            out.flush()?;
            let up = out.finish();
            file.sync_all()?;
            Ok(up)
        })();
        match written {
            Ok(up) => Ok((path, up)),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e.context(format!("cannot write {}", path.display())))
            }
        }
    }

    /// Save (§9.1): see [`Session::save_with`].
    pub fn save(&mut self, editor: &mut Editor, fs: &mut dyn RemoteFs) -> Result<Saved> {
        self.save_with(editor, fs, &mut |_, _| Ok(()))
    }

    /// Save by §9.1's steps: refused where the server cannot replace a file in one step; a cut-off
    /// Save of this session reconciled first, never repeated blindly; the session's first Save
    /// backs the original up; the pieces staged; [`RemoteFs::replace_guarded`], each step
    /// recorded first. `observe` hears every step after it was recorded, with the record (the
    /// tests' hooks); an error from it stops the Save there, as a failure would.
    pub fn save_with(
        &mut self,
        editor: &mut Editor,
        fs: &mut dyn RemoteFs,
        observe: &mut dyn FnMut(&Guard<'_>, Option<&journal::SaveIntent>) -> Result<()>,
    ) -> Result<Saved> {
        if !fs.can_replace() {
            let line = format!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.show(&self.target)
            );
            return Ok(Saved::CannotReplace(line));
        }
        if let Some(found) = self.settle(editor, fs)? {
            return Ok(found);
        }
        let made = self.backup.is_none();
        let backup = self.ensure_backup(editor)?;
        self.replace(editor, fs, observe, made.then_some(backup))
    }

    /// A Save of this session cut off with an unknown outcome (07 §3.1): reconciled from fresh
    /// reads before anything is written again. Landed: nothing is written again. Not landed: the
    /// record goes and this Save goes on. Unknown: refused, saying what to check.
    fn settle(&mut self, editor: &mut Editor, fs: &mut dyn RemoteFs) -> Result<Option<Saved>> {
        let Some(intent) = self
            .journal
            .as_ref()
            .and_then(|j| j.pending_save())
            .cloned()
        else {
            return Ok(None);
        };
        let r = journal::reconcile(&intent, &self.host, &mut Look(fs));
        let show = |p: &str| self.show(p);
        let mut notes = act(fs, &r, &intent.target, &show);
        let line = r.line.replace("(found on reopen)", "(found now)");
        let j = self.journal.as_mut().context("the journal is gone")?;
        match r.outcome {
            journal::Outcome::Landed => {
                j.clear_save()?;
                if let (Some((base, _)), Some(now)) = (r.next, fs.lstat_opt(&intent.target)?) {
                    let id = fs.file_id(&intent.target)?.map(|(id, _)| id);
                    let sha = base.sha256.context("a text save's baseline has its hash")?;
                    self.baseline =
                        Baseline::of(&now, sha, id.filter(|_| self.baseline.id.is_some()));
                    if document_sha(editor) == sha {
                        editor.mark_saved();
                    }
                }
                notes.insert(0, format!("{line}: nothing was written again"));
                Ok(Some(Saved::Found { notes }))
            }
            journal::Outcome::NotLanded => {
                j.clear_save()?;
                Ok(None)
            }
            journal::Outcome::InPart | journal::Outcome::Unknown => {
                bail!("{line}; nothing was written this time")
            }
        }
    }

    /// §9.1 steps 4–6: stage, the guarded replace recorded in the journal, and what landed.
    fn replace(
        &mut self,
        editor: &mut Editor,
        fs: &mut dyn RemoteFs,
        observe: &mut dyn FnMut(&Guard<'_>, Option<&journal::SaveIntent>) -> Result<()>,
        backup: Option<PathBuf>,
    ) -> Result<Saved> {
        let (staged, up) = self.stage(editor)?;
        // The generation before the Save (07 §3): the edits as they are, kept first — once the
        // original they are made on is durable, waited for, so every phase is recorded (E32).
        let _ = self.keep(editor);
        if self.keeping.is_some() {
            let _ = self.original_answer(true);
            let _ = self.keep(editor);
        }
        let snap = self.snapshot(editor, &self.baseline);
        let route = match (self.baseline.id, fs.can_link()) {
            (Some(_), _) => Route::Exchange,
            (None, true) => Route::Link,
            (None, false) => Route::Upload,
        };
        let target = self.target.clone();
        let mut told = Told::default();
        let journal = &mut self.journal;
        let result = fs.replace_guarded(
            &staged,
            &target,
            self.baseline.fingerprint(),
            self.baseline.id,
            &mut |guard| {
                told.record(
                    &guard,
                    journal.as_mut(),
                    snap.as_ref(),
                    (&target, route, up.sha256),
                )?;
                observe(&guard, journal.as_ref().and_then(|j| j.pending_save()))
            },
        );
        let landed = match result {
            Ok(Replaced {
                outcome: WriteOutcome::Written { now, notes },
                kept,
                unguarded,
            }) => (now, notes, kept, unguarded),
            // Landed, but what followed the rename failed: the staged temp is what the file became.
            Err(e) if told.renamed && told.staged.is_some() => {
                let stat = told.staged.unwrap_or_else(|| unreachable!("checked above"));
                (stat, vec![format!("{e:#}")], None, None)
            }
            Ok(Replaced {
                outcome: WriteOutcome::Conflict { now, reason, notes },
                ..
            }) => {
                self.not_landed(&told, false);
                return self.conflict(staged, now, reason, notes);
            }
            Err(e) => {
                // Once the exchange is recorded, which file is where is the record's to tell:
                // it stays for the next Save to reconcile (by device and inode).
                let unknown = remote::is_outcome_unknown(&e) || told.left || told.ids.is_some();
                self.not_landed(&told, unknown);
                return Err(failed(&e, &staged));
            }
        };
        let (now, mut notes, kept, unguarded) = landed;
        let id = match self.baseline.id {
            Some(_) => fs.file_id(&target).ok().flatten().map(|(id, _)| id),
            None => None,
        };
        self.baseline = Baseline::of(&now, up.sha256, id);
        editor.mark_saved();
        // The link check never finished (still `committed`), or the version it found was not
        // moved (still `kept` at the link): the record stays, and the next Save runs it first.
        let unfinished = self
            .journal
            .as_ref()
            .and_then(|j| j.pending_save())
            .and_then(|i| i.guard.as_ref())
            .is_some_and(|g| match g {
                Guarded::Committed { .. } => true,
                Guarded::Kept { from, .. } => kept.as_deref() == Some(from.as_str()),
                _ => false,
            });
        match (unfinished && told.begun, self.journal.as_mut()) {
            (true, Some(j)) => j.save_unknown(),
            _ => self.landed(editor, &told),
        }
        if let Some(u) = unguarded {
            let key = match (u, self.baseline.id) {
                (Unguarded::NoExchange, Some(id)) => format!("dev {}", id.dev),
                _ => format!("host {}", self.host),
            };
            if first_time(key) {
                notes.insert(0, u.line().to_string());
            }
        }
        let _ = std::fs::remove_file(&staged);
        Ok(Saved::Written {
            file: self.show(&target),
            bytes: up.size,
            encoding: editor.buffer().encoding(),
            backup,
            kept,
            unguarded,
            notes,
        })
    }

    /// A Save landed: its record ends, and the journal stays with one generation of the saved
    /// document against the new baseline (§9.5: undo past a Save stays kept).
    fn landed(&mut self, editor: &Editor, told: &Told) {
        let Some(j) = self.journal.take() else { return };
        if !told.begun {
            self.journal = Some(j);
            let _ = self.keep(editor);
            return;
        }
        let snap = snapshot(editor, &j, self.kept_add, &self.baseline);
        match j.save_landed(snap.as_ref()) {
            Ok(j) => self.journal = j,
            Err(e) => {
                eprintln!("sopen: {e:#}");
                self.kept_add = 0;
            }
        }
        if self.journal.is_none() {
            self.kept_add = 0;
        }
    }

    /// A Save that did not land: its record goes and the edits stay — or, with an unknown
    /// outcome, the record stays for the next Save to reconcile.
    fn not_landed(&mut self, told: &Told, unknown: bool) {
        let Some(j) = self.journal.as_mut() else {
            return;
        };
        let ended = if told.unrecorded {
            j.clear_save()
        } else if !told.begun {
            Ok(())
        } else if unknown {
            j.save_unknown();
            Ok(())
        } else {
            j.save_not_landed()
        };
        if let Err(e) = ended {
            eprintln!("sopen: {e:#}");
        }
    }

    /// §9.2: nothing was written over the file; the staged copy is kept under `.edits`.
    fn conflict(
        &mut self,
        staged: PathBuf,
        now: Option<Stat>,
        reason: String,
        notes: Vec<String>,
    ) -> Result<Saved> {
        let edits = crate::cache::edits_path(&self.cache, &self.host, &self.target);
        let (kept, _) = crate::gui::create_keep(&edits, "conflict")?;
        if let Err(e) = std::fs::rename(&staged, &kept) {
            let _ = std::fs::remove_file(&kept);
            return Err(e)
                .with_context(|| format!("cannot keep the edited copy at {}", kept.display()));
        }
        let overwrite = now.is_some_and(|s| s.is_file && !s.is_symlink);
        let line = format!("{} changed since it was read ({reason})", self.name());
        Ok(Saved::Conflict {
            line,
            overwrite,
            staged: kept,
            notes,
        })
    }

    /// Overwrite (§9.2, E23): see [`Session::overwrite_with`].
    pub fn overwrite(&mut self, editor: &mut Editor, fs: &mut dyn RemoteFs) -> Result<Saved> {
        self.overwrite_with(editor, fs, &mut |_, _| Ok(()))
    }

    /// Overwrite, a second guarded replace and never a way around the checks: the file is read
    /// again as it is now into the backup folder (its path printed), that read — its stat,
    /// SHA-256 and device and inode — becomes the baseline, and the Save's steps 4–6 run against
    /// it, so a change after that read is caught again. Refused for a file that is gone, a link
    /// or not a regular file: there is no version to back up.
    pub fn overwrite_with(
        &mut self,
        editor: &mut Editor,
        fs: &mut dyn RemoteFs,
        observe: &mut dyn FnMut(&Guard<'_>, Option<&journal::SaveIntent>) -> Result<()>,
    ) -> Result<Saved> {
        if !fs.can_replace() {
            let line = format!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.show(&self.target)
            );
            return Ok(Saved::CannotReplace(line));
        }
        if let Some(found) = self.settle(editor, fs)? {
            return Ok(found);
        }
        let target = self.target.clone();
        match fs.lstat_opt(&target)? {
            Some(s) if s.is_file && !s.is_symlink => {}
            _ => bail!(
                "{} is gone, a link or not a regular file: there is no version to back up, so \
                 there is no Overwrite — Save As… or Save copy keeps your text",
                self.name()
            ),
        }
        let local = self.baseline.id.is_some();
        let id = |fs: &mut dyn RemoteFs| -> Result<Option<FileId>> {
            Ok(match local {
                true => fs.file_id(&target)?.map(|(id, _)| id),
                false => None,
            })
        };
        let before = id(fs)?;
        let mut at = None;
        let backup = self.new_backup(|path| {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            let fetched = fs
                .fetch(&target, path, &mut |_, _| {})
                .map_err(|e| std::io::Error::other(format!("{e:#}")))?;
            at = Some(fetched);
            Ok(())
        })?;
        let at = at.context("the file was not read")?;
        if id(fs)? != before {
            bail!("{} kept changing while it was read; try again", self.name());
        }
        println!("backup of {} at {}", self.show(&target), backup.display());
        let sha = at.sha256.context("a fetch hashes what it read")?;
        self.baseline = Baseline::of(&at, sha, before);
        self.replace(editor, fs, observe, Some(backup))
    }

    /// Save copy (07 §6): `<stem>.edited<ext>` beside the file (`-N` when taken), never over
    /// anything. The printed path of the new file.
    pub fn save_copy(&mut self, editor: &Editor, fs: &mut dyn RemoteFs) -> Result<String> {
        let (staged, _) = self.stage(editor)?;
        let (dir, name) = remote::split_parent(&self.target);
        let wanted = crate::gui::edited_name(name);
        match fs.write_new_sibling(&staged, dir, &wanted) {
            Ok(made) => {
                let _ = std::fs::remove_file(&staged);
                Ok(self.show(&remote::join(dir, &made)))
            }
            Err(e) => Err(failed(&e, &staged)),
        }
    }

    /// Save As… (07 §7): the document to a local path, never over anything (`binary::save_new`:
    /// a name taken is [`SaveError::Exists`], untouched).
    pub fn save_as(&self, editor: &Editor, dest: &Path) -> std::result::Result<SavedTo, SaveError> {
        crate::gui::binary::save_new(dest, |file| {
            let mut out = BufWriter::with_capacity(1 << 20, file);
            let n = editor.buffer().write_to(&mut out)?;
            out.flush()?;
            Ok(n)
        })
    }

    /// Reload (§9.2): the edits discarded and the file as it is now taken (`read`, at the
    /// caret's line — the screen's); the journal goes.
    pub fn reload(&mut self, editor: &mut Editor, read: Read, doubt: Doubt) {
        self.drop_journal();
        self.not_kept = None;
        self.target = read.path;
        self.baseline = read.baseline;
        editor.reread(read.original, doubt);
    }

    /// E30's re-read of a clean editor (§9.4): the file as it is now is the new original,
    /// generation `k` + 1 (§9.5). The journal, if any, writes it as its `original` before any
    /// generation names it.
    pub fn reread(&mut self, editor: &mut Editor, read: Read, doubt: Doubt) {
        self.target = read.path;
        self.baseline = read.baseline;
        editor.reread(read.original.clone(), doubt);
        self.keep_original(read.original, editor.buffer().generation());
    }

    /// The editor closes: with nothing unsaved the journal goes (§9.5); else it stays for the
    /// next F4's Restore.
    pub fn close(mut self, editor: &Editor) {
        if editor.buffer().is_saved() {
            self.drop_journal();
        }
    }

    /// Discard (§9.6): the journal goes.
    pub fn discard(mut self) {
        self.drop_journal();
    }
}

/// The document as a text generation of `j` against `baseline`: the original it is made on, the
/// chunks, the pieces. `None` until the original and every byte of the add buffer (`kept_add`
/// of it kept) are durable.
fn snapshot(editor: &Editor, j: &Journal, kept_add: u64, baseline: &Baseline) -> Option<Snapshot> {
    let b = editor.buffer();
    let original = j.original().filter(|o| o.k == b.generation())?;
    if kept_add != b.add_len() {
        return None;
    }
    let pieces = b
        .pieces()
        .iter()
        .map(|p| match b.span(p) {
            Span::Original { range, .. } => TextPiece::Original(range),
            Span::Add(range) => TextPiece::Add(range),
        })
        .collect();
    let edits = TextEdits {
        encoding: b.encoding().label().to_string(),
        id: baseline.id,
        original,
        chunks: j.chunks().to_vec(),
        add: b.add_len(),
        saved: b.is_saved(),
        pieces,
    };
    Some(Snapshot {
        baseline: baseline.fingerprint(),
        seen: Seen::whole(baseline.sha256),
        edits: edits.to_text(),
        pastes: Vec::new(),
    })
}

/// The SHA-256 of the document as a Save would stage it.
fn document_sha(editor: &Editor) -> Sha {
    let mut out = Hashing::new(std::io::sink());
    let _ = editor.buffer().write_to(&mut out);
    out.finish().sha256
}

/// A new owner-only file at `path`, holding `bytes`; `AlreadyExists` when anything is there.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

/// A journal an earlier session left (§9.5): the recovery row's Restore · Later · Discard, with
/// the journal's size beside its date. Later is dropping it: it stays, and is offered again at
/// the file's next F4.
pub struct Recovery {
    pub orphan: Orphan,
    pub size: u64,
    pub date: Option<SystemTime>,
}

impl Recovery {
    /// Its generation is the file as last saved: nothing unsaved to offer.
    pub fn saved(&self) -> bool {
        match &self.orphan.found {
            journal::Found::Edits(gen) => TextEdits::parse(&gen.edits).is_ok_and(|e| e.saved),
            journal::Found::Unusable { .. } => false,
        }
    }
}

/// The text journals of `target` on `host` that sessions now gone left, newest first.
pub fn recoveries(
    journal_fs: &Arc<dyn journal::Fs>,
    cache: &Path,
    host: &str,
    target: &str,
) -> Result<Vec<Recovery>> {
    let orphans = journal::orphans(journal_fs, cache, host, target)?;
    Ok(orphans
        .into_iter()
        .filter(|o| match &o.found {
            journal::Found::Edits(gen) => gen.head.kind == Kind::Text,
            journal::Found::Unusable { generation, .. } => generation
                .as_ref()
                .is_none_or(|g| g.head.kind == Kind::Text),
        })
        .map(|orphan| {
            let date = match &orphan.found {
                journal::Found::Edits(gen) => Some(gen.head.created),
                journal::Found::Unusable { generation, .. } => {
                    generation.as_ref().map(|g| g.head.created)
                }
            };
            Recovery {
                size: orphan.size(),
                date,
                orphan,
            }
        })
        .collect())
}

/// What Restore rebuilt (§9.5): the editor and its session, and the lines to show.
pub struct Restored {
    pub editor: Editor,
    pub session: Session,
    pub lines: Vec<String>,
}

/// What Restore came to.
pub enum Restoring {
    Restored(Box<Restored>),
    /// The cut-off Save the journal recorded had landed and nothing later was kept: nothing to
    /// restore, and the journal is gone.
    Landed(Vec<String>),
}

/// Restore (§9.5, ES-3): a Save the journal records as cut off is reconciled first (its temp and
/// its other file acted on); then the buffer is rebuilt from the journal alone — the original it
/// was made on, the add buffer's chunks, the pieces — and the session takes the journal over.
/// The file as it is now only decides whether the edits are against it (else the line says so,
/// and Save asks).
pub fn restore(
    rec: Recovery,
    fs: &mut dyn RemoteFs,
    label: Option<&str>,
    cache: &Path,
    journal_fs: Arc<dyn journal::Fs>,
    guessed: bool,
) -> Result<Restoring> {
    let mut orphan = rec.orphan;
    let show = |p: &str| match label {
        Some(label) => format!("{label}:{p}"),
        None => p.to_string(),
    };
    let mut lines = Vec::new();
    // A cut-off Save found landed with edits kept after it: the file is that Save's, and its
    // baseline the one the edits are now against (as the next Save's `settle` takes it).
    let mut landed = None;
    if let Some(r) = orphan.reconcile(&mut Look(fs)) {
        let target = match &orphan.save {
            journal::SaveFile::Intent(intent) => intent.target.clone(),
            _ => String::new(),
        };
        let notes = act(fs, &r, &target, &show);
        lines.push(r.line.clone());
        lines.extend(notes);
        match r.outcome {
            journal::Outcome::Landed => match orphan.landed()? {
                Some(later) => {
                    orphan = later;
                    let sha = r.next.as_ref().and_then(|(base, _)| base.sha256);
                    if let (Some(now), Some(sha)) = (fs.lstat_opt(&target)?, sha) {
                        let id = fs.file_id(&target)?.map(|(id, _)| id);
                        landed = Some((now, sha, id));
                    }
                }
                None => return Ok(Restoring::Landed(lines)),
            },
            journal::Outcome::NotLanded => orphan.clear_save()?,
            journal::Outcome::InPart | journal::Outcome::Unknown => {}
        }
    }
    let backing = orphan.text()?;
    let journal::Found::Edits(gen) = &orphan.found else {
        bail!("these edits cannot be restored: Later or Discard");
    };
    let gen = gen.clone();
    let edits = &backing.edits;
    let encoding = Encoding::from_label(&edits.encoding)
        .with_context(|| format!("the journal names an unknown encoding {}", edits.encoding))?;
    let check = match landed {
        // The file is the landed Save's: the edits are against it.
        Some(_) => journal::Restore::Apply { notes: Vec::new() },
        None => journal::restore_check(&gen, Kind::Text, &mut Look(fs)),
    };
    let name = remote::split_parent(&gen.head.path).1.to_string();
    match check {
        journal::Restore::Apply { notes } => {
            lines.extend(notes.iter().map(|n| format!("{name} {n}")));
        }
        // A text check applies in every case (journal.rs); these are other kinds' answers.
        journal::Restore::Refuse { why } | journal::Restore::Unchecked { why } => {
            lines.push(format!("{name}: {why}"))
        }
        journal::Restore::Ask => lines.push(format!("{name}: {}", journal::ASK_PENDING)),
        journal::Restore::OtherKind(kind) => {
            bail!(
                "{name}'s journal is the {} window's, not the editor's",
                kind.word()
            )
        }
    }
    let k = edits.original.k;
    let spans: Vec<Span> = edits
        .pieces
        .iter()
        .map(|p| match p {
            TextPiece::Original(range) => Span::Original {
                k,
                range: range.clone(),
            },
            TextPiece::Add(range) => Span::Add(range.clone()),
        })
        .collect();
    let original = Original::new(backing.original, encoding);
    let doubt = Doubt::of(&original, guessed);
    let buffer = Buffer::restore(original, k, backing.add, &spans)
        .map_err(|_| anyhow!("the journal's pieces do not fit its original and add buffer"))?;
    let editor = Editor::restore(buffer, doubt);
    let journal = orphan.adopt()?;
    let base = gen.head.baseline;
    let sha = base
        .sha256
        .context("a text journal's baseline has its hash")?;
    let session = Session {
        label: label.map(str::to_string),
        host: gen.head.host.clone(),
        target: gen.head.path.clone(),
        baseline: match landed {
            Some((now, sha, id)) => Baseline::of(&now, sha, id.filter(|_| edits.id.is_some())),
            None => Baseline {
                size: base.size,
                mtime: base.mtime,
                sha256: sha,
                id: edits.id,
            },
        },
        cache: cache.to_path_buf(),
        journal_fs,
        backup: None,
        journal: Some(journal),
        keeping: None,
        not_kept: None,
        kept_add: edits.add,
    };
    Ok(Restoring::Restored(Box::new(Restored {
        editor,
        session,
        lines,
    })))
}

#[cfg(test)]
mod tests;
