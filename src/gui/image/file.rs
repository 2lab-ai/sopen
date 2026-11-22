//! The image window's write side (spec §5): the opened file, its bytes as opened, the local
//! backup written before the first overwrite, Save's conflict-checked overwrite and Save copy's
//! new sibling. Window-free, so the tests and the `SOPEN_AUTOSAVE` hook run the exact code the
//! window's buttons run.
//!
//! And where the window's unsaved edits live besides memory (07 §3, §8): [`Keeper`] writes them
//! to the journal — the operations and the pasted pictures, against the file's [`Basis`]; a
//! Save records each step of its overwrite there before the request that follows it (§3.1);
//! [`recover`] lists the journals earlier sessions left, reconciles a Save one of them was cut
//! off in, and checks each against the file as now read. [`Output`] is Save As's producer
//! (07 §7): the encoder's output.

use super::{Edit, Encoded};
use crate::gui::Origin;
use crate::journal::{
    self, FileStat, Journal, Kind, Orphan, Plan, PostEvidence, Restore, Route, Schedule, Seen, Sha,
    Snapshot,
};
use crate::remote::{patch, Fingerprint, Stat, Step, WriteOutcome, HASH_CHECK_MAX};
use anyhow::{anyhow, bail, Context as _, Result};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

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
    /// The SHA-256 of `original`, once taken (the stream's read gives it at once).
    opened: Arc<OnceLock<Sha>>,
    /// The whole SHA-256 of what the last Save wrote, once one landed.
    saved: Option<Sha>,
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
            opened: Arc::default(),
            saved: None,
        }
    }

    /// The file as read had this whole SHA-256 (the stream hashed it on the way, 06 §7.3): it
    /// need not be taken again.
    pub(super) fn knowing(self, sha: Sha) -> Self {
        let _ = self.opened.set(sha);
        self
    }

    /// What the journal's edits are against ([`Basis`]).
    pub(super) fn basis(&self) -> Basis {
        Basis {
            stat: self.origin.at,
            saved: self.saved,
            original: self.original.clone(),
            opened: self.opened.clone(),
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

    /// Save (spec §5), as [`ImageFile::save_kept`] with no journal: what the `SOPEN_AUTOSAVE`
    /// tests and the window-free callers run.
    pub fn save(&mut self, encode: impl FnOnce() -> Result<Encoded>) -> Result<Saved> {
        self.save_kept(encode, &mut None, None, &Saving::default())
    }

    /// Save (spec §5, 07 §3.1, §8), in this order: can this server replace a file in one step
    /// (refused before anything is written if not) → a Save this session left cut off is
    /// reconciled first, never repeated blindly → the local backup of the original (printed) →
    /// `encode` → stage the edit locally → the conflict-checked overwrite
    /// ([`RemoteFs::write_atomic_with`](crate::remote::RemoteFs::write_atomic_with)). With a
    /// `journal` and the edits it holds (`take`), each step of the overwrite is recorded in its
    /// `save` file before the request that follows it: the temp's name before its create, the
    /// temp once written, the staged temp with the output's whole SHA-256 right before the
    /// rename. Landed: the journal is retired (nothing is left unsaved — no edit happens while
    /// a Save runs). Not landed: the record goes and the edits stay. Unknown, or a temp of its
    /// own left behind: the record stays for the next Save or open to reconcile. `saving`
    /// says where the Save is, and its Cancel stops it before the rename (the temp removed,
    /// the original untouched). An encode failure leaves the backup made; a refusal or a
    /// transport error keeps the edit in a local file whose path the message names.
    pub(super) fn save_kept(
        &mut self,
        encode: impl FnOnce() -> Result<Encoded>,
        journal: &mut Option<Journal>,
        take: Option<&Take>,
        saving: &Saving,
    ) -> Result<Saved> {
        if !self.can_replace() {
            bail!(
                "{} was not touched: {CANNOT_REPLACE}",
                self.origin.display()
            );
        }
        if let Some(found) = self.settle(journal)? {
            return Ok(found);
        }
        let backup = self.ensure_backup()?;
        saving.set(Stage::Encoding);
        let encoded = encode().with_context(|| {
            format!(
                "{} was not touched (the original is backed up at {})",
                self.origin.display(),
                backup.display()
            )
        })?;
        if saving.cancelled() {
            return Ok(Saved::Cancelled);
        }
        let not_kept = encoded.not_kept;
        let bytes = encoded.bytes;
        let staged = self.stage(&bytes, "edited")?;
        // The post-save evidence (07 §3.1): the output's whole SHA-256 at every size, and above
        // 256 MiB its first and last MiB for reconciliation.
        let whole = journal::sha256(&bytes);
        let ends = (bytes.len() as u64 > HASH_CHECK_MAX).then(|| journal::Ends::of(&bytes));
        drop(bytes);
        let snap = match (journal.as_ref(), take) {
            (Some(_), Some(take)) => Some(snapshot(&take.basis, &take.ops)?),
            _ => None,
        };
        let at = self.origin.at;
        let abs = self.origin.abs.clone();
        let mut told = Told::default();
        let written = self.origin.fs().and_then(|fs| {
            fs.write_atomic_with(&staged, &abs, at.fingerprint(), &mut |step| {
                told.step(step, journal, snap.as_ref(), &abs, (whole, ends), saving)
            })
        });
        self.after(journal, &told, &written);
        let result = (|| -> Result<Saved> {
            let landed = match written {
                Ok(WriteOutcome::Written { now, notes }) => (now, notes),
                // Landed, but what followed the rename failed (its `lstat`): the staged temp
                // is what the file became.
                Err(e) if told.renamed => match told.staged {
                    Some(stat) => (
                        Stat {
                            sha256: Some(whole),
                            ..stat
                        },
                        vec![format!("{e:#}")],
                    ),
                    None => return Err(e),
                },
                Err(e) if crate::remote::is_cancelled(&e) => {
                    let _ = std::fs::remove_file(&staged);
                    return Ok(Saved::Cancelled);
                }
                Err(e) => return Err(e),
                Ok(WriteOutcome::Conflict { reason, notes, .. }) => {
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
                    return Ok(Saved::Refused { message, kept });
                }
            };
            let (now, notes) = landed;
            // The next baseline (07 §3.1): the staged stat and the output's whole SHA-256, known
            // at every size — sopen encoded it.
            self.saved = Some(whole);
            self.origin.at = now;
            let _ = std::fs::remove_file(&staged);
            Ok(Saved::Written {
                remote: self.origin.display(),
                backup,
                notes,
                not_kept,
            })
        })();
        result.map_err(|e| failed(&e, &staged))
    }

    /// The journal after the overwrite (07 §3.1), from what it told: landed → retired;
    /// unknown, or a temp of its own left behind → the record stays for a reconciliation; any
    /// other end → the record goes and the edits stay.
    fn after(&self, journal: &mut Option<Journal>, told: &Told, written: &Result<WriteOutcome>) {
        let Some(j) = journal.as_mut() else {
            return;
        };
        if told.unrecorded {
            // Its record may be on disk although nothing was sent: cleared.
            if let Err(e) = j.clear_save() {
                eprintln!("sopen: {e:#}");
            }
            return;
        }
        if !told.begun {
            return;
        }
        let landed = told.renamed || matches!(written, Ok(WriteOutcome::Written { .. }));
        let unknown = matches!(written, Err(e) if crate::remote::is_outcome_unknown(e));
        if landed {
            if let Some(j) = journal.take() {
                if let Err(e) = j.save_landed(None) {
                    eprintln!("sopen: {e:#}");
                }
            }
        } else if unknown || told.left {
            j.save_unknown();
        } else if let Err(e) = j.save_not_landed() {
            eprintln!("sopen: {e:#}");
        }
    }

    /// A Save this session left cut off (an unknown outcome): reconciled now, from fresh reads
    /// of the remote (07 §3.1), before anything is written again. Landed: the file is that
    /// Save's output — nothing is written again ([`Saved::Found`]). Not landed: its record goes
    /// (its temp removed when provably its own, else named) and this Save goes on. Unknown:
    /// this Save is refused, saying what to check.
    fn settle(&mut self, journal: &mut Option<Journal>) -> Result<Option<Saved>> {
        let Some(j) = journal.as_mut() else {
            return Ok(None);
        };
        let Some(intent) = j.pending_save().cloned() else {
            return Ok(None);
        };
        let host = self.origin.host().to_string();
        let fs = self.origin.fs()?;
        let reconciled = {
            let patcher = fs.patcher().context(
                "this connection cannot read the file back to tell what an earlier save did",
            )?;
            journal::reconcile(&intent, &host, &mut patch::Fresh::new(patcher))
        };
        let mut notes = Vec::new();
        match &reconciled.temp {
            journal::Temp::Remove(path) => {
                if let Err(e) = fs.remove(path) {
                    notes.push(format!("{host}:{path} could not be removed ({e:#})"));
                }
            }
            journal::Temp::Keep { path, why } => notes.push(format!("{host}:{path}: {why}")),
            journal::Temp::None | journal::Temp::Gone(_) => {}
        }
        let line = reconciled.line.replace("(found on reopen)", "(found now)");
        match reconciled.outcome {
            journal::Outcome::Landed => {
                let now = fs.lstat_opt(&intent.target)?;
                let sha = reconciled.next.and_then(|(base, _)| base.sha256);
                if let (Some(now), Some(sha)) = (now, sha) {
                    self.saved = Some(sha);
                    self.origin.at = Stat {
                        sha256: Some(sha),
                        ..now
                    };
                }
                j.clear_save()?;
                notes.insert(0, format!("{line}: nothing was written again"));
                Ok(Some(Saved::Found { notes }))
            }
            journal::Outcome::NotLanded => {
                j.clear_save()?;
                for note in notes {
                    eprintln!("sopen: {note}");
                }
                Ok(None)
            }
            journal::Outcome::InPart | journal::Outcome::Unknown => {
                bail!("{line}; nothing was written this time")
            }
        }
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

// ---------------------------------------------------------------------------------------------
// A Save under way (07 §10): where it is, and its Cancel.

/// Where a Save is, for the status row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Stage {
    Encoding = 1,
    Uploading = 2,
    Checking = 3,
    Replacing = 4,
}

/// A Save's progress and its Cancel, shared by the window and the Save's worker. A cancel
/// stops the Save between its steps, up to the rename; from the rename on it is refused, and
/// the Save is reported as it ends.
#[derive(Debug, Default)]
pub(super) struct Saving {
    stage: AtomicU8,
    /// 0: open; 1: cancelled; 2: the rename is next (a cancel is refused).
    decision: AtomicU8,
}

impl Saving {
    pub(super) fn new() -> Arc<Saving> {
        Arc::new(Saving::default())
    }

    fn set(&self, stage: Stage) {
        self.stage.store(stage as u8, Ordering::SeqCst);
    }

    /// The status row's word for where the Save is.
    pub(super) fn word(&self) -> &'static str {
        match self.stage.load(Ordering::SeqCst) {
            1 => "encoding",
            2 => "uploading",
            3 => "checking the file",
            4 => "replacing the file",
            _ => "starting",
        }
    }

    /// Esc: stops the Save, unless its rename is next — whether the cancel won.
    pub(super) fn cancel(&self) -> bool {
        match self
            .decision
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => true,
            Err(now) => now == 1,
        }
    }

    pub(super) fn cancelled(&self) -> bool {
        self.decision.load(Ordering::SeqCst) == 1
    }

    /// The rename is next, unless the Save was cancelled: whether it goes ahead.
    fn replacing(&self) -> bool {
        match self
            .decision
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => true,
            Err(now) => now == 2,
        }
    }
}

/// What an overwrite told while it ran ([`Step`]), as [`ImageFile::save_kept`] records it.
#[derive(Default)]
struct Told {
    /// The `save` record was begun (phase `naming`).
    begun: bool,
    /// Beginning the record failed: it may be on disk, and nothing was sent.
    unrecorded: bool,
    staged: Option<Stat>,
    renamed: bool,
    left: bool,
}

impl Told {
    /// One step: recorded in the journal's `save` file before the request that follows it
    /// (07 §3.1), and the Cancel looked at.
    fn step(
        &mut self,
        step: Step<'_>,
        journal: &mut Option<Journal>,
        snap: Option<&Snapshot>,
        target: &str,
        (whole, ends): (Sha, Option<journal::Ends>),
        saving: &Saving,
    ) -> Result<()> {
        let recording = journal.as_mut().filter(|_| snap.is_some());
        match step {
            Step::Named { temp } => {
                if saving.cancelled() {
                    bail!(crate::remote::Cancelled);
                }
                if let (Some(j), Some(snap)) = (recording, snap) {
                    let plan = Plan::Temp {
                        route: Route::Upload,
                        target: target.to_string(),
                        temp: temp.to_string(),
                    };
                    if let Err(e) = j.begin_save(snap, plan) {
                        self.unrecorded = true;
                        return Err(e.context("the save could not be recorded; nothing was sent"));
                    }
                    self.begun = true;
                }
                saving.set(Stage::Uploading);
            }
            Step::Written { stat, .. } => {
                if let (true, Some(j)) = (self.begun, journal.as_mut()) {
                    j.save_created(FileStat::of(stat))?;
                }
                saving.set(Stage::Checking);
                if saving.cancelled() {
                    bail!(crate::remote::Cancelled);
                }
            }
            Step::Staged { stat, .. } => {
                if !saving.replacing() {
                    bail!(crate::remote::Cancelled);
                }
                if let (true, Some(j)) = (self.begun, journal.as_mut()) {
                    let staged = FileStat::of(stat);
                    j.save_staged(staged, PostEvidence::image(&staged, whole, ends)?)?;
                    j.save_renaming()?;
                }
                self.staged = Some(*stat);
                saving.set(Stage::Replacing);
            }
            Step::Renamed { .. } => self.renamed = true,
            Step::Removed { .. } => {}
            Step::Left { .. } => self.left = true,
        }
        Ok(())
    }
}

/// The result of one Save.
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
    /// Esc before the rename: nothing was written (the temp, if any, removed).
    Cancelled,
    /// An earlier Save of this session, cut off, had landed: found now, and nothing was written
    /// again. `notes` say so first.
    Found { notes: Vec<String> },
}

// ---------------------------------------------------------------------------------------------
// The journal (07 §3, §8).

/// What the window's edits are against (07 §3 `head`'s baseline and image `seen`): the file as
/// opened — its stat and the whole SHA-256 of the bytes read — until a Save lands; from then on
/// the staged output's stat and its whole SHA-256 (§3.1's next baseline, at every size). Cheap to
/// clone: the hash of the bytes read is taken once, where it is first asked for (off the
/// window's thread).
#[derive(Clone)]
pub(super) struct Basis {
    stat: Stat,
    saved: Option<Sha>,
    original: Arc<Vec<u8>>,
    opened: Arc<OnceLock<Sha>>,
}

impl Basis {
    /// The file's whole SHA-256 as the window knows it.
    pub(super) fn whole(&self) -> Sha {
        self.saved
            .unwrap_or_else(|| *self.opened.get_or_init(|| journal::sha256(&self.original)))
    }

    pub(super) fn fingerprint(&self) -> Fingerprint {
        Fingerprint {
            size: self.stat.size,
            mtime: self.stat.mtime,
            sha256: Some(self.whole()),
        }
    }
}

/// The picture's edits as one generation (07 §3): each operation a line, each pasted picture
/// once, against `basis`.
fn snapshot(basis: &Basis, ops: &[Edit]) -> Result<Snapshot> {
    let mut lines = Vec::with_capacity(ops.len());
    let mut pastes: Vec<journal::Paste> = Vec::new();
    for op in ops {
        lines.push(op.line()?);
        if let Edit::Paste { picture, .. } = op {
            let paste = picture.paste()?;
            if !pastes.iter().any(|p| p.sha256 == paste.sha256) {
                pastes.push(paste.clone());
            }
        }
    }
    Ok(Snapshot {
        baseline: basis.fingerprint(),
        seen: Seen::whole(basis.whole()),
        edits: lines.join("\n"),
        pastes,
    })
}

/// A generation to write: the window's revision it holds, and what it holds.
pub(super) struct Take {
    pub rev: u64,
    pub basis: Basis,
    pub ops: Vec<Edit>,
}

/// The window's journal, shared with the worker that writes a generation and with a Save's
/// worker, which records its intent in it (07 §3.1) and retires it once the Save landed.
pub(super) type Slot = Arc<Mutex<Option<Journal>>>;

/// Where the journal of a file goes, and how a generation is written there.
#[derive(Clone)]
struct Place {
    fs: Arc<dyn journal::Fs>,
    cache: PathBuf,
    host: String,
    abs: String,
}

impl Place {
    /// Writes `take` as the current generation, making the journal first if there is none yet
    /// (07 §3: at the first edit, not at open). Returns the journal's directory.
    fn write(&self, slot: &Slot, take: &Take) -> Result<PathBuf> {
        let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(Journal::create(
                self.fs.clone(),
                &self.cache,
                &self.host,
                &self.abs,
                Kind::Image,
            )?);
        }
        let journal = slot.as_mut().expect("made above");
        journal.publish(&snapshot(&take.basis, &take.ops)?)?;
        Ok(journal.dir().to_path_buf())
    }
}

type Wrote = (u64, Result<PathBuf>);

/// The window's journal (07 §3): made at the first edit, a generation at most [`journal::DELAY`]
/// after a change ([`Schedule`]) on a worker — pasted pictures are encoded there — or at once,
/// waited for, at focus loss, at close and before a Save starts ([`Keeper::write_now`]); removed
/// once nothing is unsaved, or by Discard. A window that goes without either leaves it for the
/// next open.
pub(super) struct Keeper {
    place: Place,
    slot: Slot,
    /// A generation being written on a worker.
    writing: Option<Receiver<Wrote>>,
    schedule: Schedule,
    /// The journal's directory, once made (`kept at <journal>`, Discard's words).
    dir: Option<PathBuf>,
}

impl Keeper {
    pub(super) fn new(file: &ImageFile) -> Self {
        Self::at(journal::disk(), file)
    }

    /// The journal on `fs` (tests: a disk that stops between steps).
    pub(super) fn at(fs: Arc<dyn journal::Fs>, file: &ImageFile) -> Self {
        Self {
            place: Place {
                fs,
                cache: file.cache_root.clone(),
                host: file.origin.host().to_string(),
                abs: file.origin.abs.clone(),
            },
            slot: Slot::default(),
            writing: None,
            schedule: Schedule::default(),
            dir: None,
        }
    }

    /// The journal, for a Save's worker.
    pub(super) fn slot(&self) -> Slot {
        self.slot.clone()
    }

    /// The journal's directory, once there is one.
    pub(super) fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The status bar's words for it (07 §3).
    pub(super) fn status(&self) -> journal::Keep {
        self.schedule.status()
    }

    /// No generation is being written, and none is due.
    pub(super) fn settled(&self) -> bool {
        self.writing.is_none() && self.schedule.due().is_none()
    }

    /// The window's edits are now at revision `rev`.
    pub(super) fn changed(&mut self, rev: u64, now: Instant) {
        self.schedule.changed(rev, now);
    }

    /// When the next generation is due, if one is.
    pub(super) fn due(&self) -> Option<Instant> {
        self.schedule.due()
    }

    pub(super) fn is_due(&self, now: Instant) -> bool {
        self.schedule.is_due(now)
    }

    /// The picture's edits cannot be written as operations on the file (07 §3's `NOT kept on
    /// disk (<why>)`): nothing is written for them.
    pub(super) fn cannot(&mut self, why: &str) {
        self.schedule.taking();
        self.schedule.failed(why.to_string());
    }

    /// A Save's outcome was unknown, and its record stays for a reconciliation (07 §3.1).
    /// `None` while the journal is busy (a Save's worker has it).
    pub(super) fn pending_save(&self) -> Option<bool> {
        let slot = self.slot.try_lock().ok()?;
        Some(slot.as_ref().is_some_and(|j| j.pending_save().is_some()))
    }

    /// Takes in a worker's outcome, if one came back.
    pub(super) fn poll(&mut self) {
        let Some(rx) = &self.writing else {
            return;
        };
        match rx.try_recv() {
            Ok(wrote) => {
                self.writing = None;
                self.landed(wrote);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                self.writing = None;
                self.schedule
                    .failed("the journal's writer stopped".to_string());
            }
        }
    }

    /// Waits for a generation still being written.
    pub(super) fn settle_writer(&mut self) -> &mut Self {
        self.wait();
        self
    }

    /// Waits for a worker still writing.
    fn wait(&mut self) {
        if let Some(rx) = self.writing.take() {
            match rx.recv() {
                Ok(wrote) => self.landed(wrote),
                Err(_) => self
                    .schedule
                    .failed("the journal's writer stopped".to_string()),
            }
        }
    }

    fn landed(&mut self, (rev, result): Wrote) {
        match result {
            Ok(dir) => {
                self.dir = Some(dir);
                self.schedule.kept(rev);
            }
            Err(e) => self.schedule.failed(format!("{e:#}")),
        }
    }

    /// Writes `take` on a worker, unless one is writing already (the next pass looks again).
    pub(super) fn write_later(&mut self, take: Take, wake: impl FnOnce() + Send + 'static) {
        if self.writing.is_some() {
            return;
        }
        self.schedule.taking();
        let (tx, rx) = mpsc::channel();
        let (place, slot) = (self.place.clone(), self.slot.clone());
        let spawned = std::thread::Builder::new()
            .name("image-journal".into())
            .spawn(move || {
                let result = place.write(&slot, &take);
                let _ = tx.send((take.rev, result));
                wake();
            });
        match spawned {
            Ok(_) => self.writing = Some(rx),
            Err(e) => self
                .schedule
                .failed(format!("cannot start the journal's writer: {e}")),
        }
    }

    /// Writes `take` now and waits for it (07 §3: focus loss, close, the start of a Save).
    pub(super) fn write_now(&mut self, take: Take) {
        self.wait();
        self.schedule.taking();
        let result = self.place.write(&self.slot, &take);
        self.landed((take.rev, result));
    }

    /// Nothing is unsaved, or Discard: the journal goes, and the schedule starts over.
    pub(super) fn remove(&mut self) -> Result<()> {
        self.wait();
        self.schedule = Schedule::default();
        self.dir = None;
        let journal = self.slot.lock().unwrap_or_else(|p| p.into_inner()).take();
        match journal {
            Some(journal) => journal.remove(),
            None => Ok(()),
        }
    }

    /// Whether there is anything to remove or settle.
    pub(super) fn holds(&self) -> bool {
        let journal = self.slot.try_lock().map_or(true, |slot| slot.is_some());
        journal || self.writing.is_some() || self.status() != journal::Keep::Clean
    }

    /// The journal went with a Save that landed: the schedule starts over.
    pub(super) fn retired(&mut self) {
        let gone = self.slot.try_lock().is_ok_and(|slot| slot.is_none());
        if gone {
            self.schedule = Schedule::default();
            self.dir = None;
        }
    }

    /// Restore took an earlier session's journal over: it is this session's from now on.
    pub(super) fn adopt(&mut self, journal: Journal) {
        self.wait();
        self.dir = Some(journal.dir().to_path_buf());
        *self.slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(journal);
    }
}

// ---------------------------------------------------------------------------------------------
// Recovery (07 §3, §3.1).

/// How long a journal whose lock is taken is tried again before it counts as another live
/// session's (07 §3 Recovery): a child being started holds a copy of a lock just let go.
const LOCK_RETRY: Duration = Duration::from_millis(500);

/// A file at an interrupted Save's temp path that sopen could not verify is its own (07 §3.1):
/// its removal is offered, never done unasked. While its journal holds the question (its `save`
/// record), Later asks it again on the next open; Restore and Discard wait for the answer.
pub(super) struct LeftTemp {
    pub path: String,
    pub line: String,
}

/// One journal an earlier session left, with what Restore may do with it, what its cut-off
/// Save came to, and a temp whose removal is asked about.
pub(super) struct Found {
    pub orphan: Orphan,
    pub restore: Restore,
    pub said: Vec<String>,
    pub temp: Option<LeftTemp>,
}

/// What a look for earlier sessions' journals found.
#[derive(Default)]
pub(super) struct Recovered {
    pub rows: Vec<Found>,
    /// A file at the temp path of a Save that landed, its ownership uncertain: Remove it or Keep
    /// it (its journal is retired).
    pub temps: Vec<LeftTemp>,
    /// What was settled (a landed Save found, a provable temp removed): said once.
    pub notes: Vec<String>,
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

/// The journals of this file (`abs` on `host`) on disk, by directory: each named as a journal
/// of it, and — where its generation can be read — written for this host and path (a journal
/// of another file whose cache path maps to the same place is not this file's). One whose
/// generation cannot be read counts: the listing names it too, once its lock is free.
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

/// Restore's reads of the file (07 §3: the image is read whole to show it anyway): its stat as
/// opened, and the whole SHA-256 of the bytes read.
struct Opened<'a> {
    basis: &'a Basis,
}

impl journal::Inspect for Opened<'_> {
    fn lstat(&mut self, _: &str) -> Result<Option<Stat>> {
        Ok(Some(self.basis.stat))
    }

    fn read_at(&mut self, path: &str, _: u64, _: usize) -> Result<Vec<u8>> {
        bail!("{path} is not read again here: its whole hash is known")
    }

    fn whole_sha256(&mut self, _: &str, size: u64) -> Result<Option<Sha>> {
        Ok((size == self.basis.stat.size).then(|| self.basis.whole()))
    }
}

/// The journals earlier sessions left of the file `file` is (07 §3 Recovery), newest first. A
/// cut-off Save one records is reconciled first, from fresh reads of the remote (§3.1): landed,
/// its journal retired and said; not landed, its temp removed when provably its own and the
/// record ended — unless the temp's ownership is uncertain, which is asked about; unknown, said.
/// The connection is made only when a journal records a Save. Then each journal's edits are
/// checked against the file as this window read it (`basis`). A journal whose lock is taken is
/// looked at again for up to [`LOCK_RETRY`]. Their locks are held until each is answered.
pub(super) fn recover(file: &Mutex<ImageFile>, basis: &Basis) -> Recovered {
    let (cache, host, abs) = {
        let file = file.lock().unwrap_or_else(|p| p.into_inner());
        (
            file.cache_root.clone(),
            file.origin.host().to_string(),
            file.origin.abs.clone(),
        )
    };
    let fs = journal::disk();
    let started = Instant::now();
    let mut orphans = Vec::new();
    let mut out = Recovered::default();
    loop {
        match journal::orphans(&fs, &cache, &host, &abs) {
            Ok(found) => orphans.extend(found),
            Err(e) => out.notes.push(format!(
                "earlier unsaved edits could not be looked for: {e:#}"
            )),
        }
        // The ones this listing holds are taken now: a list again finds only those it missed.
        let missing = this_files_journals(&cache, &host, &abs)
            .into_iter()
            .any(|dir| orphans.iter().all(|o: &Orphan| o.dir() != dir));
        if !missing || started.elapsed() >= LOCK_RETRY {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for orphan in orphans {
        if let Some(found) = recover_one(orphan, file, &host, basis, &mut out) {
            out.rows.push(found);
        }
    }
    out
}

/// One orphan: its cut-off Save reconciled, then its edits checked.
fn recover_one(
    mut orphan: Orphan,
    file: &Mutex<ImageFile>,
    host: &str,
    basis: &Basis,
    out: &mut Recovered,
) -> Option<Found> {
    let mut said = Vec::new();
    let mut temp = None;
    if !matches!(orphan.save, journal::SaveFile::None) {
        let mut file = file.lock().unwrap_or_else(|p| p.into_inner());
        let reconciled = file.origin.fs().and_then(|fs| {
            let patcher = fs
                .patcher()
                .context("this connection cannot read the file back")?;
            Ok(orphan.reconcile(&mut patch::Fresh::new(patcher)))
        });
        match reconciled {
            Ok(Some(r)) => {
                match &r.temp {
                    journal::Temp::Keep { path, why } => {
                        let left = LeftTemp {
                            path: path.clone(),
                            line: format!("{why} — remove it?"),
                        };
                        if r.outcome == journal::Outcome::Landed {
                            out.temps.push(left);
                        } else {
                            temp = Some(left);
                        }
                    }
                    journal::Temp::Remove(path) if r.outcome == journal::Outcome::NotLanded => {
                        if let Err(e) = file.origin.fs().and_then(|fs| fs.remove(path)) {
                            said.push(format!(
                                "the interrupted save's temporary file {host}:{path} could not \
                                 be removed ({e:#})"
                            ));
                        }
                    }
                    _ => {}
                }
                match r.outcome {
                    journal::Outcome::Landed => match orphan.landed() {
                        Ok(None) => {
                            out.notes
                                .push(format!("{}: nothing was written again", r.line));
                            return None;
                        }
                        Ok(Some(later)) => {
                            orphan = later;
                            said.push(r.line);
                        }
                        Err(e) => {
                            out.notes
                                .push(format!("{} — its journal stays ({e:#})", r.line));
                            return None;
                        }
                    },
                    journal::Outcome::NotLanded => {
                        // The record ends here — unless it holds the question about a temp sopen
                        // cannot prove its own, which stays until it is answered.
                        if temp.is_none() {
                            if let Err(e) = orphan.clear_save() {
                                said.push(format!("{e:#}"));
                            }
                        }
                        said.push(r.line);
                    }
                    journal::Outcome::InPart | journal::Outcome::Unknown => said.push(r.line),
                }
            }
            Ok(None) => {}
            Err(e) => said.push(format!(
                "a save was cut off, and the file could not be read to tell how ({e:#})"
            )),
        }
    }
    let restore = check(basis, &orphan);
    Some(Found {
        orphan,
        restore,
        said,
        temp,
    })
}

/// Restore's check (07 §3): the journal's evidence against the file as `basis` knows it.
fn check(basis: &Basis, orphan: &Orphan) -> Restore {
    match &orphan.found {
        journal::Found::Edits(gen) => {
            journal::restore_check(gen, Kind::Image, &mut Opened { basis })
        }
        journal::Found::Unusable { why, .. } => Restore::Refuse { why: why.clone() },
    }
}

/// Restore's check of `found` again, against the file as the window knows it now: a Save that
/// landed since it was listed made another file of it, which the edits were not made on.
pub(super) fn recheck(basis: &Basis, found: &mut Found) {
    found.restore = check(basis, &found.orphan);
}

// ---------------------------------------------------------------------------------------------
// Save As (07 §7).

/// Save As's producer for an image: the encoder's output, written whole into `part` (again on
/// every try).
pub(super) struct Output(pub Arc<Vec<u8>>);

impl crate::gui::save_as::Producer for Output {
    fn fill(
        &mut self,
        part: &mut std::fs::File,
        ctl: &crate::gui::save_as::Ctl<'_>,
    ) -> Result<u64> {
        let mut done = 0u64;
        for chunk in self.0.chunks(1 << 20) {
            if ctl.cancelled() {
                bail!("stopped: the save was cancelled");
            }
            part.write_all(chunk).context("cannot write the image")?;
            done += chunk.len() as u64;
            ctl.progress(done);
        }
        Ok(done)
    }

    fn check(&mut self, written: u64) -> std::result::Result<(), String> {
        if written == self.0.len() as u64 {
            Ok(())
        } else {
            Err(format!(
                "{written} of the image's {} bytes were written",
                self.0.len()
            ))
        }
    }
}

/// A journal of `take` made now, for a Save that runs without a window (`SOPEN_AUTOSAVE`) to
/// record its steps in (07 §3.1).
pub(super) fn begin_journal(file: &ImageFile, take: &Take) -> Result<Journal> {
    let keeper = Keeper::new(file);
    keeper.place.write(&keeper.slot, take)?;
    let journal = keeper.slot.lock().unwrap_or_else(|p| p.into_inner()).take();
    journal.context("no journal was made")
}
