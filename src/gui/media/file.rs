//! The media window's writes (07 §5.3, §9): Save renders the edit list and writes it back over
//! the original by the same rule as image and hex Save (01 §5: a backup of the original first,
//! then the conflict-checked overwrite, never silent), Save As renders it into a new local file
//! ([`MediaProducer`], 07 §7), and Export publishes a new sibling.
//!
//! - **Backup.** A local file keeps PR #15's local copy of the original. A remote one — read
//!   through a URL or from a fetched copy — keeps its original on its own host: the first Save of
//!   the session, once its render is staged, links the original to
//!   `.<name>.sopen-backup-<UTC stamp>` beside it (`RemoteFs::link_new`, never over anything) and
//!   only then calls `write_atomic`, unchanged. A server without hard links has no remote media
//!   Save; Save As and Export are offered.
//! - **Baseline.** What a Save compares the file with is the file as opened ([`Evidence`]): its
//!   whole SHA-256 at most 256 MiB, above that the SHA-256 of its first and last MiB. A worker
//!   makes it ([`Pass`]) — a local file from the start, a remote one from the first edit: a
//!   stream through a Bulk reader on its Source, a fetched copy from the fetch's hash.
//! - **Journal.** A Save keeps the edits in the window's journal first, reconciles a Save of this
//!   session that got no reply, and records every phase of the upload before the request that
//!   needs it (07 §3.1) — watching `write_atomic`'s own requests through [`Upload`].

use super::{Media, CANNOT_REPLACE};
use crate::cut;
use crate::ffmpeg::EditSpec;
use crate::gui::Origin;
use crate::journal::{self, Ends, FileStat, Journal, PostEvidence, Seen, Snapshot};
use crate::remote::{self, RemoteFs, Stat, Step, WriteOutcome, HASH_CHECK_MAX};
use crate::stream::{ReadError, ReaderKind, Source};
use anyhow::{anyhow, bail, Context as _, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

/// What a Save checks the file against (07 §3 `seen`, §4.5): the whole SHA-256 of the file as
/// opened at most 256 MiB, above that the SHA-256 of its first and last MiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    Whole([u8; 32]),
    Ends(Ends),
}

impl Evidence {
    /// The journal's `seen` for it.
    pub fn seen(self) -> Seen {
        match self {
            Evidence::Whole(sha) => Seen::whole(sha),
            Evidence::Ends(ends) => Seen::ends(ends),
        }
    }
}

/// The first and last MiB of a file of `size` bytes from `read(offset, len)`, as
/// `journal::Inspect::ends_sha256` reads them.
fn ends_by(
    size: u64,
    read: &mut dyn FnMut(u64, usize) -> Result<Vec<u8>, String>,
) -> Result<Ends, String> {
    let n = journal::ENDS.min(size);
    let first = read(0, n as usize)?;
    let last = read(size - n, n as usize)?;
    if first.len() as u64 != n || last.len() as u64 != n {
        return Err("the file ended early".to_string());
    }
    Ok(Ends {
        first: journal::sha256(&first),
        last: journal::sha256(&last),
    })
}

/// The baseline of the file as opened, made on a worker ([`MediaFile::start_pass`]): the window
/// reads its progress (`checking the file for Save — 37 %`), and a Save waits for it on its own
/// worker. A failure stays: every later Save is refused the same way until the file is opened
/// again — none falls back to comparing size and time alone.
#[derive(Default)]
pub struct Pass {
    result: Mutex<Option<std::result::Result<Evidence, String>>>,
    ready: Condvar,
    /// Bytes read so far, of `total` (a whole-file hash of a stream: the status row's percent).
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// The window went: a pass still reading stops between its reads.
    stop: AtomicBool,
}

impl Pass {
    fn ended_with(result: std::result::Result<Evidence, String>) -> Arc<Pass> {
        let pass = Pass::default();
        *pass.result.lock().unwrap() = Some(result);
        Arc::new(pass)
    }

    fn end(&self, result: std::result::Result<Evidence, String>) {
        *self.result.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
        self.ready.notify_all();
    }

    /// How it ended, if it has.
    pub fn ended(&self) -> Option<std::result::Result<Evidence, String>> {
        self.result
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Waits for the pass — `None` once `stop` says so (Esc): looked at every 50 ms, so a Save
    /// waiting here stops as soon.
    fn wait(&self, stop: &dyn Fn() -> bool) -> Option<std::result::Result<Evidence, String>> {
        let mut slot = self.result.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(result) = slot.as_ref() {
                return Some(result.clone());
            }
            if stop() {
                return None;
            }
            slot = self
                .ready
                .wait_timeout(slot, Duration::from_millis(50))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

/// The baseline of a local file read through one handle: in place (`at`: its size and time as
/// opened, checked through the handle before and after), or sopen's fetched copy of a remote
/// one (`sha`: the fetch's hash of those bytes).
fn local_evidence(
    path: &Path,
    at: Option<&Stat>,
    sha: Option<[u8; 32]>,
    pass: &Pass,
) -> std::result::Result<Evidence, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::os::unix::fs::FileExt;
    let cannot = |e: std::io::Error| format!("cannot read it ({e})");
    let same = |m: &std::fs::Metadata| {
        at.is_none_or(|at| m.len() == at.size && m.modified().ok() == Some(at.mtime))
    };
    let mut file = std::fs::File::open(path).map_err(cannot)?;
    let before = file.metadata().map_err(cannot)?;
    if !same(&before) {
        return Err("changed since it was opened (before its content was read)".to_string());
    }
    let size = before.len();
    pass.total.store(size, SeqCst);
    let evidence = if size > HASH_CHECK_MAX {
        let ends = ends_by(size, &mut |off, len| {
            let mut buf = vec![0u8; len];
            file.read_exact_at(&mut buf, off).map_err(cannot)?;
            Ok(buf)
        })?;
        Evidence::Ends(ends)
    } else if let Some(sha) = sha {
        Evidence::Whole(sha)
    } else {
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut done = 0u64;
        loop {
            if pass.stop.load(SeqCst) {
                return Err("the window closed during the check".to_string());
            }
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    hash.update(&buf[..n]);
                    done += n as u64;
                    pass.done.store(done, SeqCst);
                }
                Err(e) => return Err(cannot(e)),
            }
        }
        Evidence::Whole(hash.finalize().into())
    };
    let after = file.metadata().map_err(cannot)?;
    if !same(&after) {
        return Err("changed since it was opened (while its content was read)".to_string());
    }
    Ok(evidence)
}

/// The baseline pass of a streamed file (07 §4.5): a Bulk reader on its Source — the reader the
/// `bulk` URL serves — hashes the whole version open now, at most 256 MiB; above that it reads
/// its first and last MiB.
fn stream_evidence(source: &Source, pass: &Pass) -> std::result::Result<Evidence, String> {
    use sha2::{Digest, Sha256};
    const CHUNK: usize = 1 << 20;
    let opened = source.opened();
    let size = opened.size;
    pass.total.store(size, SeqCst);
    let mut reader = source.reader(ReaderKind::Bulk);
    let mut read = |off: u64, len: usize| -> std::result::Result<Vec<u8>, String> {
        let mut buf = vec![0u8; len];
        let mut got = 0;
        while got < len {
            match reader.read_at(off + got as u64, &mut buf[got..], &pass.stop) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(ReadError::Changed) => {
                    return Err("the file changed since it was opened".to_string())
                }
                Err(ReadError::Cancelled) => {
                    return Err("the window closed during the check".to_string())
                }
                Err(e) => return Err(format!("the file could not be read for the check ({e})")),
            }
        }
        buf.truncate(got);
        Ok(buf)
    };
    let evidence = if size > HASH_CHECK_MAX {
        Evidence::Ends(ends_by(size, &mut read)?)
    } else {
        let mut hash = Sha256::new();
        let mut at = 0u64;
        while at < size {
            let want = (size - at).min(CHUNK as u64) as usize;
            let bytes = read(at, want)?;
            if bytes.is_empty() {
                return Err(format!("the file ended at {at} of {size} bytes"));
            }
            hash.update(&bytes);
            at += bytes.len() as u64;
            pass.done.store(at, SeqCst);
        }
        Evidence::Whole(hash.finalize().into())
    };
    let now = source.opened();
    if (now.size, now.mtime) != (opened.size, opened.mtime) {
        return Err("the file was reloaded during the check".to_string());
    }
    Ok(evidence)
}

/// The first remote Save's backup (07 §5.3): the original, linked to
/// `.<name>.sopen-backup-<UTC stamp>` in its own folder, and the link's attributes as they were
/// right after it was made — what any removal of it is checked against first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub path: String,
    pub stat: FileStat,
}

/// The status line while the first remote Save renders (07 §5.3, "before").
pub fn link_ahead(host: &str) -> String {
    format!(
        "the original will be kept on {host} as a hard link — still changeable through other \
         links or programs that have it open"
    )
}

/// The result line's words for the backup link (07 §5.3, "after").
pub fn link_after(host: &str, link: &Link) -> String {
    format!(
        "the original is kept on {host} as {} ({}, a hard link: other links or open handles can \
         still change it); delete it when no longer needed",
        link.path,
        crate::human(link.stat.size)
    )
}

/// Why a remote media Save is off on a server without hard links (07 §5.3).
pub const NO_LINKS: &str = "this server has no hard links (no hardlink@openssh.com), so sopen \
     cannot keep the original beside it — remote media Save is off; Save As… or Export keeps \
     your edit";

/// The backup link could not be made: why, and what is left.
enum Unlinked {
    /// The file changed since it was opened (found when the link was looked at): the link is
    /// gone again, or named in the words.
    Changed(String),
    /// Nothing was made (the server refused, or every name was taken).
    Refused(String),
    /// The link request got no reply: it may be there. Named, never removed; nothing was
    /// uploaded.
    Unknown(String),
}

/// Links `target` (the original as opened, `at`) to a new hidden name beside it. Only the link
/// request's own success proves the name is sopen's: a name already there is never touched,
/// and one whose request got no reply is named and kept.
fn make_link(
    fs: &mut dyn RemoteFs,
    target: &str,
    at: &Stat,
) -> std::result::Result<Link, Unlinked> {
    let (dir, name) = remote::split_parent(target);
    let stamp = crate::cache::utc_stamp(SystemTime::now());
    for attempt in 0..3 {
        let mut link = format!(".{name}.sopen-backup-{stamp}");
        if attempt > 0 {
            link.push_str(&format!("-{}", &remote::random_hex()[..8]));
        }
        let path = remote::join(dir, &link);
        match fs.lstat_opt(&path) {
            Ok(None) => {}
            Ok(Some(_)) => continue,
            Err(e) => return Err(Unlinked::Refused(format!("cannot look at {path} ({e:#})"))),
        }
        match fs.link_new(target, &path) {
            Ok(()) => {}
            Err(e) if remote::is_outcome_unknown(&e) => {
                return Err(Unlinked::Unknown(format!(
                    "making the backup link {path} got no reply: check it; nothing was uploaded"
                )))
            }
            Err(e) => match fs.lstat_opt(&path) {
                // Taken since the look: someone else's, never touched; another name.
                Ok(Some(_)) => continue,
                Ok(None) => {
                    return Err(Unlinked::Refused(format!(
                        "the original could not be kept as a hard link ({e:#})"
                    )))
                }
                Err(_) => {
                    return Err(Unlinked::Unknown(format!(
                        "making the backup link {path} failed ({e:#}) and the name cannot be \
                         looked at: check it; nothing was uploaded"
                    )))
                }
            },
        }
        // Made: what it is now is what a removal must still find.
        return match fs.lstat_opt(&path) {
            Ok(Some(stat)) if stat.is_file && !stat.is_symlink && stat.missing.is_none() => {
                let link = Link {
                    path,
                    stat: FileStat::of(&stat),
                };
                if (stat.size, stat.mtime) != (at.size, at.mtime) {
                    // The target changed before the link: it is not the file as opened.
                    let why = describe_now(&stat, at);
                    return Err(match remove_link(fs, &link) {
                        Ok(()) => Unlinked::Changed(why),
                        Err(left) => Unlinked::Changed(format!("{why}; {left}")),
                    });
                }
                Ok(link)
            }
            _ => Err(Unlinked::Unknown(format!(
                "the backup link {path} was made but cannot be looked at: check it; nothing was \
                 uploaded"
            ))),
        };
    }
    Err(Unlinked::Refused(format!(
        "no free name for the backup link of {name} in {dir}"
    )))
}

fn describe_now(now: &Stat, at: &Stat) -> String {
    remote::describe_change(&at.fingerprint(), now)
}

/// A refused Save's render moved to a name of its own beside the edits (never over anyone's
/// file), and the words naming it: `; not saved — your edit is at <path>`.
fn keep_render(edits: &Path, staged: &Path) -> Result<String> {
    let (kept, _) = crate::gui::create_keep(edits, "conflict")?;
    if let Err(e) = std::fs::rename(staged, &kept) {
        let _ = std::fs::remove_file(&kept);
        return Err(e)
            .with_context(|| format!("cannot keep the rendered edit at {}", kept.display()));
    }
    Ok(format!("; not saved — your edit is at {}", kept.display()))
}

/// Removes the backup link this Save made — only while the name is still exactly the link it
/// made (a regular file with the attributes it had right after the link), looked at again right
/// before. Anything else is kept, and the words say why.
fn remove_link(fs: &mut dyn RemoteFs, link: &Link) -> std::result::Result<(), String> {
    let path = &link.path;
    match fs.lstat_opt(path) {
        Ok(None) => Ok(()),
        Ok(Some(now)) if now.missing.is_none() && link.stat.matches(&now) => fs
            .remove(path)
            .map_err(|e| format!("the backup link {path} could not be removed ({e:#})")),
        Ok(Some(now)) => Err(format!(
            "the backup link {path} was not removed: it is not as sopen made it ({} bytes, \
             modified {}, it was {} bytes, modified {})",
            now.size,
            crate::cache::rfc3339(now.mtime),
            link.stat.size,
            crate::cache::rfc3339(link.stat.mtime)
        )),
        Err(e) => Err(format!(
            "the backup link {path} was not removed: it cannot be looked at ({e:#})"
        )),
    }
}

/// What a Save does now, for the window's status row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    /// Waiting for the window's journal (a generation on its way).
    Keeping,
    /// The baseline pass, 0..=1.
    Checking(f64),
    /// The render, 0..=1.
    Rendering(f64),
    /// The original being linked beside it.
    Linking,
    Uploading,
}

/// The window's Esc for a Save: looked at between its steps, by the render (ffmpeg is killed)
/// and between the upload's chunks — up to the rename; from then on the Save is past stopping.
pub type Stop = AtomicBool;

/// What the window hands a Save.
pub struct Job<'a> {
    pub media: &'a Media,
    pub spec: &'a EditSpec,
    /// The edits as the journal keeps them (07 §3 `edits`).
    pub edits: String,
    pub work_root: &'a Path,
    /// The window's journal; made here when it has none.
    pub journal: &'a Mutex<Option<Journal>>,
    pub stop: &'a Stop,
    pub report: &'a (dyn Fn(Stage) + Sync),
}

/// What became of the backup of the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backup {
    /// A local file: PR #15's copy.
    Local(PathBuf),
    /// The first remote Save of the session: its link, made now.
    Linked(Link),
    /// A later remote Save: the link the first one made stays the backup.
    Kept(Link),
}

/// A file a cut-off Save left that the next Save could not remove by itself: the question the
/// window asks about it (Remove it / Keep it), bound to the file as it was when asked (`seen`)
/// and, when the record proved the file the Save's, to that proof (`proven`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftTemp {
    pub path: String,
    pub line: String,
    pub seen: Option<FileStat>,
    pub proven: bool,
}

/// Result of one Save.
pub enum Saved {
    /// `notes`: what the owner must still hear (the remote changed right after); `not_kept`:
    /// what the saved file does not carry; `media`: the saved file as probed (the window plays
    /// it from now on), or why it could not be read back.
    Written {
        remote: String,
        backup: Backup,
        notes: Vec<String>,
        not_kept: Vec<String>,
        media: Box<Result<Media, String>>,
    },
    /// Nothing was written: the file changed since it was opened, or the Save could not go on.
    /// `message` names where the render was kept, if one was.
    Refused { message: String },
    /// Esc stopped it before its rename: `message` says what was left (nothing, or what could
    /// not be removed).
    Stopped { message: String },
    /// A Save of this session that got no reply had landed (found now): nothing was written
    /// again. `media`: the file it wrote, as the window plays it from now on, when its render is
    /// still kept.
    FoundLanded {
        line: String,
        media: Option<Box<Result<Media, String>>>,
    },
    /// A Save of this session cut off left a file the record cannot prove its own: nothing was
    /// sent, and the window asks about it.
    Ask(LeftTemp),
    /// A Save of this session that got no reply cannot be told landed or not from what the file
    /// is now: nothing was sent; `message` names what to check. Its record stays.
    Unresolved { message: String },
}

/// What a test runs between a Save's steps.
#[cfg(test)]
pub(crate) type StepHook = Box<dyn FnMut(&str) + Send>;

/// The opened media file: where it lives and the local places sopen writes to.
pub struct MediaFile {
    pub origin: Origin,
    cache_root: PathBuf,
    /// Staged renders and refused ones: `<cache>/.edits/<host>/<abs>` siblings.
    edits: PathBuf,
    /// A local file's backup, set by the first Save; later saves of the session keep it.
    backup: Option<PathBuf>,
    /// A remote file's backup link, set by the first Save that kept it (07 §5.3).
    link: Option<Link>,
    /// What the last Save of a remote file wrote, kept here: the window plays it (the cached
    /// copy, or the stream, still holds what was opened). Removed when the window goes or saves
    /// again.
    rendered: Option<(PathBuf, crate::cache::JobLock)>,
    /// The render of a Save whose upload got no reply, with its lock: what the file holds if it
    /// landed. Removed when the window goes or the next Save tells how that one went.
    unknown: Option<(PathBuf, crate::cache::JobLock)>,
    /// The baseline of the file as opened (07 §4.5).
    pass: Option<Arc<Pass>>,
    /// Where the journals are written (tests: a disk that stops at a step).
    pub(crate) journal_fs: Arc<dyn journal::Fs>,
    /// Called between a Save's steps with their names (tests: a stop, a change of the file).
    #[cfg(test)]
    pub(crate) step_hook: Option<StepHook>,
}

impl MediaFile {
    pub fn new(origin: Origin, cache_root: &Path) -> Self {
        let edits = crate::cache::edits_path(cache_root, origin.host(), &origin.abs);
        Self {
            origin,
            cache_root: cache_root.to_path_buf(),
            edits,
            backup: None,
            link: None,
            rendered: None,
            unknown: None,
            pass: None,
            journal_fs: journal::disk(),
            #[cfg(test)]
            step_hook: None,
        }
    }

    #[cfg(test)]
    fn step(&mut self, name: &str) {
        if let Some(hook) = self.step_hook.as_mut() {
            hook(name);
        }
    }

    #[cfg(not(test))]
    fn step(&mut self, _: &str) {}

    /// For a file opened in place: its content as opened is hashed on a worker from now on —
    /// what Save compares the file with, before rendering and before the overwrite.
    pub fn hash_in_background(&mut self) {
        if !self.origin.is_local() || self.origin.at.sha256.is_some() {
            return;
        }
        let input = crate::ffmpeg::Input::path(Path::new(&self.origin.abs));
        self.start_pass(&input);
    }

    /// The baseline of the file as opened (07 §4.5), made on a worker from now on, once: a file
    /// read in place hashed through one handle, a stream through a Bulk reader on its Source, a
    /// fetched copy from the fetch's hash (or, without one, its own bytes: the copy is what was
    /// fetched).
    pub fn start_pass(&mut self, input: &crate::ffmpeg::Input) {
        if self.pass.is_some() {
            return;
        }
        let at = self.origin.at;
        type Make = Box<dyn FnOnce(&Pass) -> std::result::Result<Evidence, String> + Send>;
        let make: Make = match (input.served(), input.local()) {
            (Some(served), _) => {
                let source = served.source().clone();
                Box::new(move |pass| stream_evidence(&source, pass))
            }
            (None, Some(local)) if self.origin.is_local() => {
                let path = local.to_path_buf();
                Box::new(move |pass| local_evidence(&path, Some(&at), None, pass))
            }
            (None, Some(local)) => {
                let path = local.to_path_buf();
                Box::new(move |pass| local_evidence(&path, None, at.sha256, pass))
            }
            (None, None) => {
                self.pass = Some(Pass::ended_with(Err("it cannot be read".to_string())));
                return;
            }
        };
        let pass = Arc::new(Pass::default());
        let theirs = pass.clone();
        std::thread::spawn(move || {
            let result = make(&theirs);
            theirs.end(result);
        });
        self.pass = Some(pass);
    }

    /// The pass, for the window's status row.
    pub fn pass(&self) -> Option<Arc<Pass>> {
        self.pass.clone()
    }

    /// The hash of the file as opened is made (tests wait for it before changing the file).
    #[cfg(test)]
    pub fn baseline_ready(&self) -> bool {
        self.pass.as_ref().is_none_or(|p| p.ended().is_some())
    }

    /// The file as opened has a hash to check a Save against, or one is being made (tests).
    #[cfg(test)]
    pub fn has_baseline(&self) -> bool {
        self.pass.is_some() || self.origin.at.sha256.is_some()
    }

    /// As if hashing the file as opened had failed for `why` (tests: a failure that leaves the
    /// file's size and time as they were, such as a read error).
    #[cfg(test)]
    pub fn fail_baseline(&mut self, why: &str) {
        self.pass = Some(Pass::ended_with(Err(why.to_string())));
    }

    /// The staged render a saved remote file plays from, if any (tests).
    #[cfg(test)]
    pub fn rendered_path(&self) -> Option<&Path> {
        self.rendered.as_ref().map(|(p, _)| p.as_path())
    }

    /// The session's backup link, once a Save kept one (tests).
    #[cfg(test)]
    pub fn link(&self) -> Option<&Link> {
        self.link.as_ref()
    }

    /// A Save of this session kept the original as a link: later ones make none (07 §5.3).
    pub fn has_link(&self) -> bool {
        self.link.is_some()
    }

    /// `(host, abs, cache)`: where this file's journals are.
    pub fn place(&self) -> (String, String, PathBuf) {
        (
            self.origin.host().to_string(),
            self.origin.abs.clone(),
            self.cache_root.clone(),
        )
    }

    /// The evidence of the file as opened, once the pass has it (the journal's `seen`).
    pub fn seen(&self) -> Seen {
        match self.pass.as_ref().and_then(|p| p.ended()) {
            Some(Ok(evidence)) => evidence.seen(),
            _ => Seen::Pending,
        }
    }

    /// The journal snapshot of `edits` against the file as opened.
    pub fn snapshot(&self, edits: String) -> Snapshot {
        Snapshot {
            baseline: self.origin.at.fingerprint(),
            seen: self.seen(),
            edits,
            pastes: Vec::new(),
        }
    }

    /// Whether a Save of this file is possible at all here, and why not.
    pub fn save_off(&self) -> Option<String> {
        if !self.origin.can_replace() {
            return Some(CANNOT_REPLACE.to_string());
        }
        if !self.origin.is_local() && !self.origin.fs_can_link() {
            return Some(NO_LINKS.to_string());
        }
        None
    }

    /// The local backup of the original — the file the window plays (`local`) as opened —
    /// written once per session before the first overwrite: copied whole (a media file is not
    /// held in memory), into a new owner-only file.
    fn ensure_backup(&mut self, local: &Path) -> Result<PathBuf> {
        if let Some(path) = &self.backup {
            return Ok(path.clone());
        }
        let first = crate::cache::backup_path(
            &self.cache_root,
            self.origin.host(),
            &self.origin.abs,
            SystemTime::now(),
        );
        let (path, mut file) = crate::cache::create_unique(|attempt| {
            if attempt == 0 {
                return first.clone();
            }
            let mut name = first.file_name().unwrap_or_default().to_os_string();
            name.push(format!("-{}", &remote::random_hex()[..8]));
            first.with_file_name(name)
        })?;
        let copied = std::fs::File::open(local)
            .and_then(|mut src| std::io::copy(&mut src, &mut file))
            .and_then(|_| file.sync_all());
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("cannot write the backup {}", path.display()));
        }
        println!("backup of {} at {}", self.origin.display(), path.display());
        self.backup = Some(path.clone());
        Ok(path)
    }

    /// Keeps `snap` in the journal at once (07 §3: before a Save sends anything), the journal
    /// made first when the window has none.
    fn keep(&self, slot: &mut Option<Journal>, snap: &Snapshot) -> Result<()> {
        let (host, abs, cache) = self.place();
        let journal = match slot {
            Some(journal) => journal,
            None => slot.insert(Journal::create(
                self.journal_fs.clone(),
                &cache,
                &host,
                &abs,
                journal::Kind::Media,
            )?),
        };
        // A cut-off Save's record stays beside it (its generation is kept until reconciled).
        journal.publish(snap).map(|_| ())
    }

    /// Save (07 §5.3, §9), in this order:
    ///
    /// 1. the edits kept in the journal (the window's, or one made now);
    /// 2. the `Origin`, connected now if it was not; a Save of this session that got no reply
    ///    reconciled from fresh reads before anything is sent again — not landed: its provable
    ///    temp removed and the record ended; landed: nothing is written again; anything else
    ///    refused, the line naming what to check;
    /// 3. the baseline of the file as opened, waited for, and PR #15's check of the file against
    ///    it — content too — before anything is rendered;
    /// 4. the render into a staged local file, after the room for it is checked; a stream cut
    ///    short during it fails it;
    /// 5. the first remote Save's backup link, then `write_atomic`, its phases recorded in the
    ///    journal before the requests that need them.
    ///
    /// Esc (`job.stop`) stops it up to the rename. A refusal or a failure keeps the edits in the
    /// journal and the render, named; the backup link it made is removed — or kept and named
    /// when the outcome is unknown.
    pub fn save(&mut self, job: &Job<'_>) -> Result<Saved> {
        let shown = self.origin.display();
        let media = job.media;
        if let Some(why) = self.save_off() {
            bail!("{shown} was not touched: {why}");
        }
        if let Some(why) = media.save_block() {
            bail!("{shown} was not touched: {why}");
        }
        let Some((kind, _, plan)) = media.plan.clone() else {
            bail!("{shown} was not touched: not a file sopen can render");
        };
        let stopped = || job.stop.load(SeqCst);
        let nothing_sent = || Saved::Stopped {
            message: "save cancelled: nothing was sent".to_string(),
        };
        // 1. The edits are kept at once, whatever becomes of this Save.
        (job.report)(Stage::Keeping);
        let mut slot = job.journal.lock().unwrap_or_else(|p| p.into_inner());
        if stopped() {
            return Ok(nothing_sent());
        }
        if self.pass.is_none() {
            self.start_pass(&media.input);
        }
        if let Err(e) = self.keep(&mut slot, &self.snapshot(job.edits.clone())) {
            bail!("{shown} was not touched: the edits could not be kept in a journal ({e:#})");
        }
        self.step("kept");
        // 2. Connected, and a cut-off Save of this session reconciled first.
        let host = self.origin.host().to_string();
        let abs = self.origin.abs.clone();
        if let Err(e) = self.origin.fs() {
            bail!("{shown} was not touched: {}", crate::stream::explain(&e));
        }
        if !self.origin.is_local() && !self.origin.fs_can_link() {
            bail!("{shown} was not touched: {NO_LINKS}");
        }
        let journal = slot.as_mut().expect("kept above");
        if let Some(intent) = journal.pending_save().cloned() {
            let fs = self.origin.fs()?;
            let Some(patcher) = fs.patcher() else {
                bail!(
                    "not saved: an earlier save was cut off, and this connection cannot read \
                     {shown} to tell whether it landed: check it"
                );
            };
            let found = journal::reconcile(&intent, &host, &mut remote::patch::Fresh::new(patcher));
            match found.outcome {
                journal::Outcome::NotLanded => match &found.temp {
                    journal::Temp::Keep { path, why } => {
                        let seen = looked_at(fs, path);
                        return Ok(Saved::Ask(LeftTemp {
                            path: path.clone(),
                            line: ask_line(&host, path, why),
                            seen,
                            proven: false,
                        }));
                    }
                    temp => {
                        if let journal::Temp::Remove(temp) = temp {
                            if let Err(e) = fs.remove(temp) {
                                let seen = looked_at(fs, temp);
                                return Ok(Saved::Ask(LeftTemp {
                                    path: temp.clone(),
                                    line: format!(
                                        "the interrupted save's temporary file {host}:{temp} \
                                         could not be removed ({e:#}) — remove it?"
                                    ),
                                    seen,
                                    proven: true,
                                }));
                            }
                        }
                        journal.clear_save()?;
                        self.forget_unknown();
                    }
                },
                journal::Outcome::Landed => {
                    journal.clear_save()?;
                    let line = found.line.replace("(found on reopen)", "(found now)");
                    let line = format!("{line}: nothing was written again");
                    // The file is what that Save wrote: its staged stat and evidence are the next
                    // baseline (07 §3.1's "next baseline" column), as after a Save that replied.
                    if let Some((at, seen)) = found.next {
                        self.origin.at = Stat {
                            size: at.size,
                            mtime: at.mtime,
                            sha256: at.sha256,
                            ..self.origin.at
                        };
                        let next = match seen {
                            Seen::Known(ev) => match (ev.whole, ev.ends) {
                                (Some(sha), _) => Ok(Evidence::Whole(sha)),
                                (None, Some(ends)) => Ok(Evidence::Ends(ends)),
                                (None, None) => Err("the save's record holds no evidence".into()),
                            },
                            Seen::Pending => Err("the save's record holds no evidence".into()),
                        };
                        self.pass = Some(Pass::ended_with(next));
                    }
                    let media = self.unknown.take().map(|(kept, job)| {
                        let (_, name) = self.origin.parent();
                        let input = crate::ffmpeg::Input::path(&kept);
                        let probed = Media::probe_as(&input, &name).map_err(|e| format!("{e:#}"));
                        if probed.is_ok() {
                            if let Some((old, _)) = self.rendered.replace((kept, job)) {
                                let _ = std::fs::remove_file(old);
                            }
                        } else {
                            let _ = std::fs::remove_file(&kept);
                        }
                        Box::new(probed)
                    });
                    // Nothing is unsaved: the journal goes.
                    if let Some(journal) = slot.take() {
                        let _ = journal.remove();
                    }
                    return Ok(Saved::FoundLanded { line, media });
                }
                journal::Outcome::InPart | journal::Outcome::Unknown => {
                    return Ok(Saved::Unresolved {
                        message: format!("not saved: {}", found.line),
                    });
                }
            }
        }
        // 3. The baseline, and the file checked against it before anything is rendered.
        let Some(pass) = self.pass.clone() else {
            bail!("{shown} was not touched: its baseline was never taken");
        };
        let progress = |pass: &Pass| {
            let total = pass.total.load(SeqCst).max(1);
            pass.done.load(SeqCst) as f64 / total as f64
        };
        (job.report)(Stage::Checking(progress(&pass)));
        let ended = pass.wait(&|| {
            (job.report)(Stage::Checking(progress(&pass)));
            stopped()
        });
        let evidence = match ended {
            None => return Ok(nothing_sent()),
            Some(Ok(evidence)) => evidence,
            Some(Err(why)) => {
                return Ok(Saved::Refused {
                    message: format!(
                        "{shown} {why}; not saved — nothing was rendered, and your edits are \
                         still in the window"
                    ),
                })
            }
        };
        if let Evidence::Whole(sha) = evidence {
            self.origin.at.sha256 = Some(sha);
        }
        self.step("checked");
        if stopped() {
            return Ok(nothing_sent());
        }
        let at = self.origin.at;
        // Before a render that can take minutes: a local file is read whole here, as PR #15
        // does (its own disk). A remote one by its stat alone (still a regular file, the same
        // size and modification time): its content is read once per Save, by the overwrite's
        // check right before the rename, against the baseline's hash (07 §4.5) — never twice.
        let expected = if self.origin.is_local() {
            at.fingerprint()
        } else {
            remote::Fingerprint {
                sha256: None,
                ..at.fingerprint()
            }
        };
        let changed = {
            let fs = self.origin.fs()?;
            match remote::changed_since(fs, &abs, &expected)? {
                Some(reason) => Some(reason),
                None => match evidence {
                    Evidence::Ends(ends) => ends_changed(fs, &abs, at.size, &ends)?,
                    Evidence::Whole(_) => None,
                },
            }
        };
        if let Some(reason) = changed {
            return Ok(Saved::Refused {
                message: format!(
                    "{shown} changed since it was opened ({reason}); not saved — nothing was \
                     rendered, and your edits are still in the window"
                ),
            });
        }
        // A local file's backup is its copy, made before anything is rendered (PR #15).
        let local_backup = if self.origin.is_local() {
            let Some(local) = media.input.local() else {
                bail!("{shown} was not touched: there is no local copy to back up first");
            };
            Some(self.ensure_backup(local)?)
        } else {
            None
        };
        // 4. The render, into a staged file, once there is room for it.
        let length = job.spec.length();
        let estimate = (at.size as f64 * (length / media.duration.max(1e-9))).ceil() as u64;
        let exports = job.work_root;
        crate::cache::private_dir(exports)?;
        if let Some(free) = remote::LocalFs::free_space_at(exports) {
            if free < estimate {
                bail!(
                    "{shown} was not touched: the render needs about {} free in {}, there is {}",
                    crate::human(estimate),
                    crate::gui::binary::tilde(exports),
                    crate::human(free)
                );
            }
        }
        let work = exports.join(remote::random_hex());
        // Both held while the render runs and is written back: a sopen killed meanwhile leaves
        // them to the next start's sweep ([`crate::cache::sweep_stale_jobs`]), never forever.
        let _work_job = crate::cache::JobLock::take(&work)?;
        crate::cache::private_dir(&work)?;
        let staged = match crate::gui::create_keep(&self.edits, "render") {
            Ok((staged, _)) => staged,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                return Err(e);
            }
        };
        let mut render_job = match crate::cache::JobLock::take(&staged) {
            Ok(job) => Some(job),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                let _ = std::fs::remove_file(&staged);
                return Err(e);
            }
        };
        let rendered = cut::render_edit_stoppable(
            &media.tools,
            &media.input,
            (&staged, None),
            job.spec,
            (kind, &plan),
            &work,
            &mut |f| (job.report)(Stage::Rendering(f)),
            &stopped,
        );
        let _ = std::fs::remove_dir_all(&work);
        self.step("rendered");
        if let Err(e) = rendered {
            let _ = std::fs::remove_file(&staged);
            if remote::is_cancelled(&e) {
                return Ok(nothing_sent());
            }
            // A stream cut short during the render says so (06 §7.4): nothing is uploaded.
            let kept = match &local_backup {
                Some(backup) => format!("the original is backed up at {}", backup.display()),
                None => "nothing was uploaded, and your edits are still in the window".to_string(),
            };
            return Err(e.context(format!("{shown} was not touched ({kept})")));
        }
        let not_kept = cut::edit_notes(&plan, &media.probe, job.spec);
        // What the output is, for the journal's post-save evidence and the next baseline.
        let output = match output_evidence(&staged) {
            Ok(output) => output,
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                return Err(e.context(format!("{shown} was not touched")));
            }
        };
        if stopped() {
            let _ = std::fs::remove_file(&staged);
            return Ok(nothing_sent());
        }
        // 5. The first remote Save's backup link, then the overwrite.
        let backup = match (&local_backup, &self.link) {
            (Some(path), _) => Backup::Local(path.clone()),
            (None, Some(link)) => Backup::Kept(link.clone()),
            (None, None) => {
                (job.report)(Stage::Linking);
                let linked = match self.origin.fs() {
                    Ok(fs) => make_link(fs, &abs, &at),
                    Err(e) => Err(Unlinked::Refused(crate::stream::explain(&e))),
                };
                match linked {
                    Ok(link) => Backup::Linked(link),
                    Err(Unlinked::Changed(why)) => {
                        let _ = std::fs::remove_file(&staged);
                        return Ok(Saved::Refused {
                            message: format!(
                                "{shown} changed since it was opened ({why}); not saved — nothing \
                                 was uploaded, and your edits are still in the window"
                            ),
                        });
                    }
                    Err(Unlinked::Refused(why)) => {
                        let _ = std::fs::remove_file(&staged);
                        bail!(
                            "{shown} was not touched: {why}; not saved — Save As… or Export keeps \
                             your edit"
                        );
                    }
                    Err(Unlinked::Unknown(why)) => {
                        let _ = std::fs::remove_file(&staged);
                        bail!("{shown} was not touched: {why}");
                    }
                }
            }
        };
        let link_made = match &backup {
            Backup::Linked(link) => Some(link.clone()),
            _ => None,
        };
        // A link this Save made is never left unnamed: an error from here on says where it is.
        let named = |e: anyhow::Error| match &link_made {
            Some(link) => e.context(format!("the backup link {} is kept: check it", link.path)),
            None => e,
        };
        self.step("linked");
        (job.report)(Stage::Uploading);
        let snap = Snapshot {
            seen: evidence.seen(),
            ..self.snapshot(job.edits.clone())
        };
        let fs = match self.origin.fs() {
            Ok(fs) => fs,
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                return Err(named(e.context(format!("{shown} was not touched"))));
            }
        };
        // Above 256 MiB the first and last MiB are what the baseline holds: read again here,
        // right before the overwrite (its own checks then see size and modification time up to
        // the rename; the observer below has no connection to read with).
        let ends_now = match evidence {
            Evidence::Ends(ends) => ends_changed(fs, &abs, at.size, &ends),
            Evidence::Whole(_) => Ok(None),
        };
        let mut told = Told::default();
        let written = match ends_now {
            Err(e) => Err(e),
            Ok(Some(reason)) => Ok(WriteOutcome::Conflict {
                now: None,
                reason,
                notes: Vec::new(),
            }),
            Ok(None) => {
                let journal = slot.as_mut().expect("kept above");
                fs.write_atomic_with(&staged, &abs, at.fingerprint(), &mut |step| {
                    told.step(step, journal, &snap, &abs, output, &stopped)
                })
            }
        };
        self.step("uploaded");
        // Landed once the rename had its reply (`Step::Renamed`), whatever failed after it (its
        // `lstat`): the staged temp is what the file became. A request with no reply is an
        // unknown outcome — never "not saved": everything is kept and named, and the next Save
        // (or open) reconciles it first.
        let written = match written {
            Err(e) if told.renamed => match told.staged {
                Some(stat) => Ok(WriteOutcome::Written {
                    now: Stat {
                        sha256: output.0,
                        ..stat
                    },
                    notes: vec![format!(
                        "saved, but what followed the rename failed ({e:#}); the file is what \
                         sopen staged"
                    )],
                }),
                None => Err(e),
            },
            other => other,
        };
        let lands = matches!(written, Ok(WriteOutcome::Written { .. }));
        let unknown = !lands && matches!(&written, Err(e) if remote::is_outcome_unknown(e));
        let (created, removed) = (told.created, told.removed);
        // The record of a Save whose temp is not left goes; one whose temp is left (or whose
        // outcome is unknown) stays for the next Save — or open — to reconcile. A Save that
        // landed wrote every edit (none is taken while it runs): the journal is retired.
        let mut retire_failed = None;
        if told.unrecorded {
            // Its record may be on disk although nothing was sent: cleared.
            if let Some(journal) = slot.as_mut() {
                journal.save_unknown();
                if let Err(e) = journal.clear_save() {
                    crate::debug!("journal: {e:#}");
                }
            }
        } else if lands && told.begun {
            if let Some(journal) = slot.take() {
                if let Err(e) = journal.save_landed(None) {
                    retire_failed = Some(format!(
                        "saved, but its journal could not be removed ({e:#})"
                    ));
                }
            }
        } else if told.begun {
            let journal = slot.as_mut().expect("kept above");
            if unknown || told.left {
                journal.save_unknown();
            } else if let Err(e) = journal.save_not_landed() {
                crate::debug!("journal: the save's record stays: {e:#}");
            }
        }
        drop(slot);
        let result = (|| -> Result<Saved> {
            match written? {
                WriteOutcome::Written { now, mut notes } => {
                    // The staged temp's stat is the next baseline; its hash the uploaded bytes'.
                    self.origin.at = now;
                    if let Some(link) = &link_made {
                        self.link = Some(link.clone());
                    }
                    if matches!(evidence, Evidence::Ends(_)) {
                        notes.retain(|n| !n.contains("over 256 MiB"));
                        notes.push(journal::NOTE_ENDS.to_string());
                    }
                    notes.extend(retire_failed.clone());
                    let next = match output {
                        (Some(sha), _) => Evidence::Whole(sha),
                        (None, Some(ends)) => Evidence::Ends(ends),
                        (None, None) => unreachable!("output_evidence gives one"),
                    };
                    self.pass = Some(Pass::ended_with(Ok(next)));
                    self.forget_unknown();
                    let playing = if self.origin.is_local() {
                        let _ = std::fs::remove_file(&staged);
                        PathBuf::from(&abs)
                    } else {
                        // Kept as the playing copy, with its lock: a sopen killed later still
                        // leaves it to the next start's sweep.
                        let job = render_job.take().expect("held until here");
                        if let Some((old, _)) = self.rendered.replace((staged.clone(), job)) {
                            let _ = std::fs::remove_file(old);
                        }
                        staged.clone()
                    };
                    let (_, name) = self.origin.parent();
                    let input = crate::ffmpeg::Input::path(&playing);
                    let media =
                        Box::new(Media::probe_as(&input, &name).map_err(|e| format!("{e:#}")));
                    Ok(Saved::Written {
                        remote: shown.clone(),
                        backup: backup.clone(),
                        notes,
                        not_kept: not_kept.clone(),
                        media,
                    })
                }
                WriteOutcome::Conflict { reason, notes, .. } => {
                    let mut message = format!("{shown} changed since it was opened ({reason})");
                    message.push_str(&keep_render(&self.edits, &staged)?);
                    for note in notes {
                        message.push_str(&format!("; {note}"));
                    }
                    Ok(Saved::Refused { message })
                }
            }
        })();
        // The backup link of a Save that did not land goes — the exact link it made, checked
        // again right before — or, when the outcome is unknown, stays as the session's backup,
        // named.
        let landed = matches!(result, Ok(Saved::Written { .. }));
        let mut said = Vec::new();
        if let Some(link) = &link_made {
            if unknown {
                self.link = Some(link.clone());
                said.push(format!(
                    "the original is kept on {host} as {} (a hard link)",
                    link.path
                ));
            } else if !landed {
                match self.origin.fs() {
                    Ok(fs) => {
                        if let Err(left) = remove_link(fs, link) {
                            said.push(left);
                        }
                    }
                    Err(e) => said.push(format!(
                        "the backup link {} could not be removed ({}): check it",
                        link.path,
                        crate::stream::explain(&e)
                    )),
                }
            }
        }
        if unknown {
            // What the file holds if the upload landed: the next Save, finding it landed, plays it.
            if let Some(job) = render_job.take() {
                self.forget_unknown();
                self.unknown = Some((staged.clone(), job));
            }
        }
        let stopped_now = matches!(&result, Err(e) if remote::is_cancelled(e));
        match result {
            Ok(Saved::Refused { mut message }) => {
                for s in said {
                    message.push_str(&format!("; {s}"));
                }
                Ok(Saved::Refused { message })
            }
            Err(_) if stopped_now => {
                let _ = std::fs::remove_file(&staged);
                let mut message = match (created, removed) {
                    (false, _) => {
                        format!("save cancelled: nothing was uploaded; {shown} was not replaced")
                    }
                    (true, true) => format!(
                        "save cancelled before the rename: the uploaded temporary file was \
                         removed; {shown} was not replaced"
                    ),
                    (true, false) => format!(
                        "save cancelled: {shown} was not replaced, but the upload's temporary \
                         file could not be removed"
                    ),
                };
                for s in said {
                    message.push_str(&format!("; {s}"));
                }
                Ok(Saved::Stopped { message })
            }
            Err(e) => {
                let mut e = crate::gui::image::failed(&e, &staged);
                if unknown {
                    // Said as it was, and still an unknown outcome (07 §3.1): never "not saved".
                    e = remote::unknown(format!("{e:#}"));
                }
                for s in said {
                    e = e.context(s);
                }
                Err(e)
            }
            other => other,
        }
    }

    /// The kept render of a Save whose upload got no reply goes (the file first, then its lock).
    fn forget_unknown(&mut self) {
        if let Some((path, _job)) = self.unknown.take() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Removes the file a question about an interrupted Save's temp was about, after the owner
    /// chose to — that file only (07 §3.1, the hex window's rule). With the question's record,
    /// the record is reconciled again first, from fresh reads: a temp it proves the Save's is
    /// removed as a reconcile removes one; one it proved before and no longer does is left.
    /// Otherwise the file at the path now must be a regular file with every attribute the
    /// question saw. Anything else is left, and said.
    pub fn remove_left(
        &mut self,
        left: &LeftTemp,
        record: Option<&journal::SaveIntent>,
    ) -> Result<()> {
        let shown = self.origin.show(&left.path);
        let host = self.origin.host().to_string();
        let fs = self
            .origin
            .fs()
            .map_err(|e| anyhow!("{shown} was not removed: {}", crate::stream::explain(&e)))?;
        if let Some(intent) = record {
            let patcher = fs
                .patcher()
                .with_context(|| format!("{shown} cannot be checked — not removed"))?;
            let found = journal::reconcile(intent, &host, &mut remote::patch::Fresh::new(patcher));
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
            None => Ok(()),
            Some(now) if now.missing.is_none() && seen.matches(&now) => fs
                .remove(&left.path)
                .with_context(|| format!("{shown} was not removed")),
            Some(now) => bail!(
                "{shown} is not the file the question was about (it is {} bytes now, modified \
                 {}) — not removed",
                now.size,
                crate::cache::rfc3339(now.mtime)
            ),
        }
    }

    /// The owner's answer to a live session's question about a temp ([`Saved::Ask`]): the record
    /// must still be the one that asked it (naming that temp); `remove` then removes the file it
    /// was about ([`MediaFile::remove_left`] — a failure keeps the question and the record).
    /// Then the record ends, and the next Save goes ahead.
    pub fn answer_left(
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
        self.forget_unknown();
        Ok(if remove {
            format!("removed {shown}")
        } else {
            format!("kept {shown}; it will not be asked about again")
        })
    }
}

impl Drop for MediaFile {
    fn drop(&mut self) {
        if let Some(pass) = &self.pass {
            pass.stop.store(true, SeqCst);
        }
        // The file first, then its lock (dropped with it).
        if let Some((path, _job)) = self.rendered.take() {
            let _ = std::fs::remove_file(path);
        }
        self.forget_unknown();
    }
}

impl Origin {
    /// Whether this server can make hard links (`hardlink@openssh.com`), as its connection
    /// answers — a placeholder's answer until the first use connects.
    fn fs_can_link(&self) -> bool {
        self.fs.can_link()
    }
}

/// What a render staged at `path` is, for the post-save evidence (07 §3.1): its whole SHA-256 at
/// most 256 MiB, above that its first and last MiB.
fn output_evidence(path: &Path) -> Result<(Option<[u8; 32]>, Option<Ends>)> {
    use std::os::unix::fs::FileExt;
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot read the render {}", path.display()))?;
    let size = file.metadata()?.len();
    if size > HASH_CHECK_MAX {
        let ends = ends_by(size, &mut |off, len| {
            let mut buf = vec![0u8; len];
            file.read_exact_at(&mut buf, off)
                .map_err(|e| format!("cannot read the render ({e})"))?;
            Ok(buf)
        })
        .map_err(|why| anyhow!(why))?;
        return Ok((None, Some(ends)));
    }
    let mut sink = remote::Hashing::new(std::io::sink());
    std::io::copy(&mut std::io::BufReader::new(file), &mut sink)
        .with_context(|| format!("cannot read the render {}", path.display()))?;
    Ok((Some(sink.finish().sha256), None))
}

/// Above 256 MiB: whether the first or last MiB of `target` (`size` bytes) changed from `ends`,
/// read fresh. `None`: as they were.
fn ends_changed(
    fs: &mut dyn RemoteFs,
    target: &str,
    size: u64,
    ends: &Ends,
) -> Result<Option<String>> {
    let patcher = fs
        .patcher()
        .with_context(|| format!("this connection cannot read part of {target}"))?;
    let now = journal::Inspect::ends_sha256(&mut remote::patch::Fresh::new(patcher), target, size)?;
    Ok((now != *ends)
        .then(|| "its first or last MiB changed (same size and modification time)".to_string()))
}

/// The attributes of the regular file at `path` (its `lstat`, every attribute there), or `None`.
fn looked_at(fs: &mut dyn RemoteFs, path: &str) -> Option<FileStat> {
    match fs.lstat_opt(path) {
        Ok(Some(stat)) if stat.is_file && !stat.is_symlink && stat.missing.is_none() => {
            Some(FileStat::of(&stat))
        }
        _ => None,
    }
}

/// The question about a temp the record cannot prove the Save's: the file named, then why.
fn ask_line(host: &str, path: &str, why: &str) -> String {
    if why.contains(path) {
        format!("{why} — remove it?")
    } else {
        format!("{host}:{path}: {why} — remove it?")
    }
}

/// The temp a Save's record names.
pub fn record_temp(intent: &journal::SaveIntent) -> Option<&str> {
    match &intent.phase {
        journal::Phase::Naming { temp }
        | journal::Phase::Created { temp, .. }
        | journal::Phase::Staged { temp, .. }
        | journal::Phase::Renaming { temp, .. } => Some(temp),
        journal::Phase::Writing { .. } => None,
    }
}

/// What `write_atomic_with` told while it uploaded a media Save's render ([`Step`]), and the
/// journal's record of it (07 §3.1): each step recorded in the `save` file before the request
/// that follows it — `naming` before the temp's exclusive create, `created` with the temp's
/// stat once its create and fill (one request sequence) have replied, `staged` and `renaming`
/// right before the rename, after every check. Esc is looked at before the create, after the
/// upload and right before the rename: a stop there removes the temp, and the file is as it
/// was.
#[derive(Default)]
struct Told {
    /// The record was begun (phase `naming`).
    begun: bool,
    /// Beginning the record failed: it may be on disk, and nothing was sent.
    unrecorded: bool,
    /// The temp was created and filled (`Step::Written`).
    created: bool,
    /// The temp as it was renamed.
    staged: Option<Stat>,
    /// The rename had its reply.
    renamed: bool,
    removed: bool,
    left: bool,
}

impl Told {
    fn step(
        &mut self,
        step: Step<'_>,
        journal: &mut Journal,
        snap: &Snapshot,
        target: &str,
        output: (Option<[u8; 32]>, Option<Ends>),
        stopped: &dyn Fn() -> bool,
    ) -> Result<()> {
        match step {
            Step::Named { temp } => {
                if stopped() {
                    bail!(remote::Cancelled);
                }
                let plan = journal::Plan::Temp {
                    route: journal::Route::Upload,
                    target: target.to_string(),
                    temp: temp.to_string(),
                };
                if let Err(e) = journal.begin_save(snap, plan) {
                    self.unrecorded = true;
                    return Err(e.context("the save could not be recorded; nothing was sent"));
                }
                self.begun = true;
            }
            Step::Written { stat, .. } => {
                self.created = true;
                journal
                    .save_created(FileStat::of(stat))
                    .context("the save could not be recorded in the journal")?;
                if stopped() {
                    bail!(remote::Cancelled);
                }
            }
            Step::Staged { stat, .. } => {
                if stopped() {
                    bail!(remote::Cancelled);
                }
                let staged = FileStat::of(stat);
                let post = PostEvidence::media(&staged, output.0, output.1)?;
                journal
                    .save_staged(staged, post)
                    .context("the save could not be recorded in the journal")?;
                journal
                    .save_renaming()
                    .context("the save could not be recorded in the journal")?;
                self.staged = Some(*stat);
            }
            Step::Renamed { .. } => self.renamed = true,
            Step::Removed { .. } => self.removed = true,
            Step::Left { .. } => self.left = true,
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The journal's text for media edits (07 §3 `edits`): the clip list in ticks, stereo, the crop,
// In/Out — and the timeline's rate and length, so that edits are applied only to the file they
// were made on.

/// The edits of one media window, as the journal keeps them.
#[derive(Debug, Clone, PartialEq)]
pub struct Edits {
    /// Ticks per second, and the source's length in ticks.
    pub rate: f64,
    pub source: i64,
    pub clips: Vec<(i64, i64)>,
    pub stereo: bool,
    pub crop: Option<[u32; 4]>,
    /// In and Out, in timeline ticks.
    pub marks: (Option<i64>, Option<i64>),
}

impl Edits {
    /// The text: one field per line.
    pub fn text(&self) -> String {
        let mark = |m: Option<i64>| m.map_or("none".to_string(), |t| t.to_string());
        let mut out = format!(
            "rate {:?}\nsource {}\nstereo {}\ncrop {}\nin {}\nout {}\n",
            self.rate,
            self.source,
            u8::from(self.stereo),
            self.crop.map_or("none".to_string(), |[x, y, w, h]| format!(
                "{x},{y},{w},{h}"
            )),
            mark(self.marks.0),
            mark(self.marks.1),
        );
        for (a, b) in &self.clips {
            out.push_str(&format!("clip {a} {b}\n"));
        }
        out
    }

    pub fn parse(text: &str) -> Result<Edits> {
        let mut lines = text.lines();
        let mut field = |name: &str| -> Result<String> {
            let line = lines.next().with_context(|| format!("no {name} line"))?;
            line.strip_prefix(&format!("{name} "))
                .map(str::to_string)
                .with_context(|| format!("'{line}' is not the {name} line"))
        };
        let rate: f64 = field("rate")?.parse().context("rate")?;
        let source: i64 = field("source")?.parse().context("source")?;
        let stereo = match field("stereo")?.as_str() {
            "0" => false,
            "1" => true,
            other => bail!("stereo '{other}'"),
        };
        let crop = match field("crop")?.as_str() {
            "none" => None,
            text => {
                let v: Vec<u32> = text
                    .split(',')
                    .map(|n| n.parse().context("crop"))
                    .collect::<Result<_>>()?;
                let [x, y, w, h] = v[..] else {
                    bail!("crop '{text}'")
                };
                Some([x, y, w, h])
            }
        };
        let mark = |text: String| -> Result<Option<i64>> {
            Ok(match text.as_str() {
                "none" => None,
                t => Some(t.parse().context("mark")?),
            })
        };
        let marks = (mark(field("in")?)?, mark(field("out")?)?);
        let mut clips = Vec::new();
        for line in lines {
            let rest = line
                .strip_prefix("clip ")
                .with_context(|| format!("'{line}' is not a clip"))?;
            let (a, b) = rest
                .split_once(' ')
                .with_context(|| format!("clip '{rest}'"))?;
            let (a, b): (i64, i64) = (a.parse()?, b.parse()?);
            ensure_clip(a, b, source)?;
            clips.push((a, b));
        }
        anyhow::ensure!(!clips.is_empty(), "no clip");
        anyhow::ensure!(rate.is_finite() && rate > 0.0, "rate {rate}");
        Ok(Edits {
            rate,
            source,
            clips,
            stereo,
            crop,
            marks,
        })
    }

    /// Nothing is edited: the whole source, as it is.
    pub fn is_identity(&self) -> bool {
        !self.stereo && self.clips == [(0, self.source)]
    }

    /// `3 pieces`, `1 piece, stereo`.
    pub fn summary(&self) -> String {
        let n = self.clips.len();
        format!(
            "{n} piece{}{}",
            if n == 1 { "" } else { "s" },
            if self.stereo { ", stereo" } else { "" }
        )
    }
}

fn ensure_clip(a: i64, b: i64, source: i64) -> Result<()> {
    anyhow::ensure!(0 <= a && a < b && b <= source, "clip {a}..{b} of {source}");
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Recovery (07 §3, §3.1): what an earlier session left for this file.

/// One recovery row of the media window.
pub enum Row {
    /// A journal of this file's media edits: Restore, Later, Discard — and, under it, the
    /// question about an interrupted save's temp when the record holds one.
    Edits {
        orphan: journal::Orphan,
        line: String,
        check: journal::Restore,
        edits: Edits,
        temp: Option<LeftTemp>,
    },
    /// A journal kept and named that cannot be applied here (another kind, unreadable, or only
    /// an interrupted save's record): Later, Discard.
    Kept {
        orphan: journal::Orphan,
        line: String,
        temp: Option<LeftTemp>,
    },
    /// A temp of uncertain ownership beside a save that landed: asked on this open only.
    Temp {
        path: String,
        line: String,
        seen: Option<FileStat>,
    },
    Note(String),
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
/// and — where its generation can be read — written for this host and path. What a listing
/// should find once no other session holds one.
fn this_files_journals(cache: &Path, host: &str, abs: &str) -> usize {
    let base = crate::cache::edits_path(cache, host, abs);
    let (Some(home), Some(name)) = (base.parent(), base.file_name()) else {
        return 0;
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
        .filter(|e| head_is_ours(&e.path()))
        .count()
}

/// How long a journal whose lock is taken is tried again (§3 Recovery: a child another thread
/// is starting holds a copy of the lock's descriptor from its fork to its exec).
const LOCK_RETRY: Duration = Duration::from_millis(500);

/// `2026-11-20 11:42 UTC`.
fn when(t: SystemTime) -> String {
    let r = crate::cache::rfc3339(t);
    format!("{} {} UTC", &r[..10], &r[11..16])
}

/// The journals and cut-off Saves an earlier session left for this file, as rows (on a worker):
/// a recorded Save reconciled first from fresh reads of the remote (a temp removed only when
/// provably the Save's; one whose ownership is uncertain offered for removal), then each
/// journal's edits checked against the file as it is now (`journal::restore_check`: media at
/// most 256 MiB by its whole content, above that by size, time and the first and last MiB). The
/// `Origin` connects only when there is a journal to check. A journal that holds no edit is
/// removed: there is nothing to offer.
pub fn recover(file: &Mutex<MediaFile>) -> Vec<Row> {
    let (host, abs, cache, fs) = {
        let file = file.lock().unwrap_or_else(|p| p.into_inner());
        let (host, abs, cache) = file.place();
        (host, abs, cache, file.journal_fs.clone())
    };
    let want = this_files_journals(&cache, &host, &abs);
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
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut rows = Vec::new();
    if orphans.is_empty() {
        return rows;
    }
    let mut file = file.lock().unwrap_or_else(|p| p.into_inner());
    let connected = file.origin.fs().map(|_| ());
    for orphan in orphans {
        let fs = match &connected {
            Ok(()) => file.origin.fs().ok(),
            Err(_) => None,
        };
        rows.extend(recover_one(orphan, &host, fs));
    }
    if let Err(e) = &connected {
        rows.push(Row::Note(format!(
            "the kept edits could not be checked against the file: {}",
            crate::stream::explain(e)
        )));
    }
    rows
}

/// One orphan's rows: its cut-off Save reconciled, then its edits checked.
fn recover_one(mut orphan: journal::Orphan, host: &str, fs: Option<&mut dyn RemoteFs>) -> Vec<Row> {
    let mut rows = Vec::new();
    let dir = crate::gui::binary::tilde(orphan.dir());
    let mut said: Vec<String> = orphan.notes.clone();
    let mut left = None;
    let mut fs = fs;
    let reconciled = match fs.as_deref_mut().and_then(|fs| fs.patcher()) {
        Some(patcher) => orphan.reconcile(&mut remote::patch::Fresh::new(patcher)),
        None => None,
    };
    if let Some(r) = reconciled {
        match &r.temp {
            journal::Temp::Keep { path, why } if r.outcome == journal::Outcome::Landed => {
                let seen = fs.as_deref_mut().and_then(|fs| looked_at(fs, path));
                rows.push(Row::Temp {
                    path: path.clone(),
                    line: ask_line(host, path, why),
                    seen,
                })
            }
            journal::Temp::Keep { path, why } => {
                left = Some(LeftTemp {
                    path: path.clone(),
                    line: ask_line(host, path, why),
                    seen: fs.as_deref_mut().and_then(|fs| looked_at(fs, path)),
                    proven: false,
                })
            }
            journal::Temp::Remove(path) if r.outcome == journal::Outcome::NotLanded => {
                // Provably this Save's (its record's stat and evidence, just read): removed now.
                // A removal that fails keeps the record, and the question is asked.
                match fs.as_deref_mut().map(|fs| fs.remove(path)) {
                    Some(Ok(())) | None => {}
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
        journal::Found::Edits(gen) if gen.head.kind == journal::Kind::Media => {
            let edits = match Edits::parse(&gen.edits) {
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
            if edits.is_identity() && matches!(orphan.save, journal::SaveFile::None) {
                if let Err(e) = orphan.remove() {
                    crate::debug!("journal: an empty one stays: {e:#}");
                }
                return rows;
            }
            if edits.is_identity() && left.is_some() {
                let line = format!("an interrupted save's record is kept at {dir}");
                rows.push(Row::Kept {
                    line: tail(line, &said),
                    orphan,
                    temp: left,
                });
                return rows;
            }
            let check = match fs.and_then(|fs| fs.patcher()) {
                Some(patcher) => journal::restore_check(
                    &gen,
                    journal::Kind::Media,
                    &mut remote::patch::Fresh::new(patcher),
                ),
                None => journal::Restore::Unchecked {
                    why: "the file could not be read".to_string(),
                },
            };
            let line = format!(
                "unsaved edits from {} ({}) kept at {dir}",
                when(gen.head.created),
                edits.summary()
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
                "edits made in the {} window are kept at {dir}; the media window does not apply \
                 them",
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

// ---------------------------------------------------------------------------------------------
// Save As (07 §7): the render into the modal's staging `part`.

/// Where `part` is, as the modal's writer says ([`save_as::Ctl::part`]), checked to be the
/// very file being filled (device and inode) before anything writes it by that path.
fn path_of(part: &std::fs::File, ctl: &crate::gui::save_as::Ctl<'_>) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let path = ctl
        .part()
        .context("the writer did not say where its staging file is")?
        .to_path_buf();
    let (at, open) = (std::fs::symlink_metadata(&path)?, part.metadata()?);
    anyhow::ensure!(
        at.file_type().is_file() && (at.dev(), at.ino()) == (open.dev(), open.ino()),
        "{} is not the staging file being written",
        path.display()
    );
    Ok(path)
}

/// Save As's producer for the media window: the edit rendered from the window's input (a stream
/// through its `render` URL — a response cut short fails the render) straight into the modal's
/// staging `part`, by its path and with the output's container said explicitly (`part` has no
/// extension: `ffmpeg::edit_args_as`). Passes of a long edit, and ffmpeg's graph files, go to a
/// private folder of sopen's exports; the output itself is written once, into `part`.
pub struct MediaProducer {
    pub media: Arc<Media>,
    pub spec: EditSpec,
    pub work_root: PathBuf,
    /// Receipt hook (`SOPEN_SAVE_AS_HOLD_MS`): the fill holds at its first progress at or past
    /// 30 % for this long, or until cancelled; `held` is set meanwhile.
    pub hold: Option<Duration>,
    pub held: Arc<AtomicBool>,
}

impl crate::gui::save_as::Producer for MediaProducer {
    fn fill(
        &mut self,
        part: &mut std::fs::File,
        ctl: &crate::gui::save_as::Ctl<'_>,
    ) -> Result<u64> {
        let Some((kind, ext, plan)) = self.media.plan.clone() else {
            bail!("not a file sopen can render");
        };
        let estimate = estimate_size(&self.media, &self.spec);
        let dst = path_of(part, ctl)?;
        let work = self.work_root.join(remote::random_hex());
        let _job = crate::cache::JobLock::take(&work)?;
        crate::cache::private_dir(&work)?;
        let mut hold = self.hold;
        let held = self.held.clone();
        let rendered = cut::render_edit_stoppable(
            &self.media.tools,
            &self.media.input,
            (&dst, Some(ext.as_str())),
            &self.spec,
            (kind, &plan),
            &work,
            &mut |f| {
                ctl.progress((f * estimate as f64) as u64);
                if f >= 0.3 {
                    if let Some(d) = hold.take() {
                        held.store(true, SeqCst);
                        let until = std::time::Instant::now() + d;
                        while std::time::Instant::now() < until && !ctl.cancelled() {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        held.store(false, SeqCst);
                    }
                }
            },
            &|| ctl.cancelled(),
        );
        let _ = std::fs::remove_dir_all(&work);
        rendered?;
        let written = part.metadata()?.len();
        ctl.progress(written);
        Ok(written)
    }
}

/// A render's size as estimated before it runs (07 §9: the file's bytes a second × the output's
/// length).
pub fn estimate_size(media: &Media, spec: &EditSpec) -> u64 {
    let size = media
        .input
        .served()
        .map(|s| s.source().size())
        .or_else(|| {
            media
                .input
                .local()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
        })
        .unwrap_or(0);
    (size as f64 * (spec.length() / media.duration.max(1e-9))).ceil() as u64
}
