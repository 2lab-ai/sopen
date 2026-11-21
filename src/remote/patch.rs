//! Saving a hex edit to a remote file without moving the file (`.prd/07-streaming-edit.md` §4,
//! §5.1): the file is copied on the server (`copy-data`) into a temp, the changed runs are
//! written into the temp, and the temp is renamed over the target ([`save`]). Without
//! `copy-data` a file of at most 256 MiB is rewritten through this machine in one fresh pass;
//! where no atomic route fits, the Save is refused with the cost ([`Refusal`]) and the runs can
//! be written into the file itself, from an explicit dialog only ([`write_in_place`]).
//!
//! - [`PatchFs`] is what those need of a connection beyond [`RemoteFs`]: files opened by handle,
//!   positional reads and writes, `fstat`, `fsync`, and the server's copy. `sftp/patch.rs`
//!   implements it over SFTP, `local/patch.rs` on the local disk (a plain copy and `pwrite`: the
//!   half the tests run beside the real `sftp-server`).
//! - **Every read a Save makes is a fresh read of the remote**, through a handle this Save
//!   opened. Nothing here takes a cache, so a block the view fetched earlier cannot stand in for
//!   the file as it is now (§4.2).
//! - [`Pace`] sizes the copy requests: a file copy keeps its fixed steps, a Save's requests take
//!   about a second each. The loop that sends them is the one in `sftp/ops.rs`.
//! - [`UndoFile`] keeps the bytes a session's Saves replaced (§5.1), durable before anything of
//!   the file is patched. [`BaselinePass`] is the whole-file hash a Save of at most 256 MiB
//!   compares, with a short hash of every page so the baseline can be held to what the window
//!   showed (§4.5).
//! - Each phase goes into the journal's `save` record before the request that needs it
//!   (`crate::journal`, §3.1). Every step can be stopped, or its reply lost, by [`Ctl::hook`].
//!
//! Nothing here knows a window. The hex window (ED4) gives the patches, the evidence and the
//! journal, and shows what comes back.

use super::{
    check_target, describe_change, describe_stat, join, random_hex, split_parent, unknown,
    unknown_at, Cancelled, Check, CreateError, Created, Fingerprint, RemoteFs, Stat,
    HASH_CHECK_MAX,
};
use crate::journal::{self, FileStat, Journal, Plan, PostEvidence, Route, Seen, Sha, Snapshot};
use anyhow::{anyhow, bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::any::Any;
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

/// The pages evidence is kept by (the journal's `seen`).
pub const PAGE: u64 = journal::PAGE;

/// Free space a Save wants in the target's folder beyond the file's size (§4.1).
pub const ROOM_MARGIN: u64 = 1 << 30;

/// The status row when the baseline pass disagrees with a page the window showed (§4.5).
pub const CHANGED_UNDER_EDITS: &str = "the file changed under your edits — Cmd/Ctrl+R reloads";

/// What every SFTP server takes in one write request (the protocol's 32 KiB packet floor): a
/// longer write may be split, so a refusal of it does not prove nothing of it was written.
const ONE_REQUEST: usize = 32 * 1024;

const SAME_STAT_OTHER_CONTENT: &str = "its content changed (same size and modification time)";

// ---------------------------------------------------------------------------------------------
// The size of each copy request.

/// How large each request of a server copy is, and when the temp is flushed on the way.
///
/// - [`Pace::steps`]: fixed steps and no flush — a file copy (04 §5.1).
/// - [`Pace::timed`]: a Save (07 §4.2 step 5). The first request is 64 MiB; each next one is
///   sized to take about 1 s at the rate measured, within 16 MiB–1 GiB, so a slow server disk
///   never turns one request into a request timeout (an unknown outcome). The temp is flushed
///   after every 2 GiB copied or 10 s of copying, whichever comes first, and a flush's time
///   counts in the rate: a server that answers at memory speed and pays at the flush is then
///   sized by what its disk does.
///
/// It holds no clock: the loop tells it how long each request and each flush took.
#[derive(Debug, Clone)]
pub struct Pace {
    next: u64,
    min: u64,
    max: u64,
    /// How long a request should take; `None`: every request is `next` bytes.
    target: Option<Duration>,
    sync_bytes: u64,
    sync_after: Duration,
    /// Copied, and the time it took, since the last flush.
    window_bytes: u64,
    window_time: Duration,
}

impl Pace {
    pub const FIRST: u64 = 64 << 20;
    pub const MIN: u64 = 16 << 20;
    pub const MAX: u64 = 1 << 30;
    pub const REQUEST: Duration = Duration::from_secs(1);
    pub const SYNC_BYTES: u64 = 2 << 30;
    pub const SYNC_AFTER: Duration = Duration::from_secs(10);

    /// Fixed steps of `step` bytes, never flushed on the way.
    pub fn steps(step: u64) -> Pace {
        Pace {
            next: step.max(1),
            min: step.max(1),
            max: step.max(1),
            target: None,
            sync_bytes: u64::MAX,
            sync_after: Duration::MAX,
            window_bytes: 0,
            window_time: Duration::ZERO,
        }
    }

    /// A Save's pace (07 §4.2 step 5).
    pub fn timed() -> Pace {
        Pace::sized(Self::FIRST, Self::MIN, Self::MAX, Self::SYNC_BYTES)
    }

    /// [`Pace::timed`]'s rule at other sizes (tests run it on small files).
    pub fn sized(first: u64, min: u64, max: u64, sync_bytes: u64) -> Pace {
        let min = min.max(1);
        let max = max.max(min);
        Pace {
            next: first.clamp(min, max),
            min,
            max,
            target: Some(Self::REQUEST),
            sync_bytes,
            sync_after: Self::SYNC_AFTER,
            window_bytes: 0,
            window_time: Duration::ZERO,
        }
    }

    /// The size of the next request, with `left` bytes to copy.
    pub fn next(&self, left: u64) -> u64 {
        self.next.min(left)
    }

    /// A request of `bytes` was answered after `took`.
    pub fn copied(&mut self, bytes: u64, took: Duration) {
        self.window_bytes = self.window_bytes.saturating_add(bytes);
        self.window_time = self.window_time.saturating_add(took);
        self.size_from(bytes, took);
    }

    /// Whether the temp is to be flushed before the next request.
    pub fn sync_due(&self) -> bool {
        self.target.is_some()
            && (self.window_bytes >= self.sync_bytes || self.window_time >= self.sync_after)
    }

    /// The flush took `took`: the rate is what was copied since the last flush over the time
    /// its requests and this flush took together.
    pub fn synced(&mut self, took: Duration) {
        let time = self.window_time.saturating_add(took);
        self.size_from(self.window_bytes, time);
        self.window_bytes = 0;
        self.window_time = Duration::ZERO;
    }

    fn size_from(&mut self, bytes: u64, took: Duration) {
        let Some(target) = self.target else {
            return;
        };
        let per = match took.as_nanos() {
            // Answered within the clock's grain: as fast as the rule allows.
            0 => u128::from(self.max),
            nanos => u128::from(bytes) * target.as_nanos() / nanos,
        };
        self.next = u64::try_from(per)
            .unwrap_or(u64::MAX)
            .clamp(self.min, self.max);
    }
}

/// What a copy loop tells its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyEvent {
    /// A request was answered: the bytes copied so far.
    Copied(u64),
    /// The temp was flushed on the way, with this much copied.
    Synced(u64),
}

/// What a copy loop runs by: its pace, the clock its requests are timed with (monotonic, from
/// any start; tests give a scripted one), when to stop, and who hears of it.
pub struct CopyCtl<'a> {
    pub pace: Pace,
    pub clock: &'a mut dyn FnMut() -> Duration,
    /// Asked before each request: `true` ends the copy as [`Cancelled`]. A request already sent
    /// finishes on the server first (SFTP has no cancel).
    pub cancel: &'a dyn Fn() -> bool,
    pub progress: &'a mut dyn FnMut(CopyEvent),
}

/// A clock for [`CopyCtl`] and [`Ctl`]: the time since it was made.
pub fn stopwatch() -> impl FnMut() -> Duration {
    let started = std::time::Instant::now();
    move || started.elapsed()
}

// ---------------------------------------------------------------------------------------------
// What a Save needs of a connection.

/// A file open on a [`PatchFs`]: the backend's own handle, and the path it was opened at (for
/// messages only — every request goes by the handle).
pub struct Handle {
    path: String,
    inner: Box<dyn Any>,
}

impl Handle {
    pub fn new(path: &str, inner: impl Any) -> Handle {
        Handle {
            path: path.to_string(),
            inner: Box::new(inner),
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// The backend's handle; an error for a handle of another backend.
    pub fn get<T: Any>(&mut self) -> Result<&mut T> {
        let path = &self.path;
        self.inner
            .downcast_mut::<T>()
            .with_context(|| format!("{path} is not open on this connection"))
    }

    pub fn into_inner<T: Any>(self) -> Option<T> {
        self.inner.downcast::<T>().ok().map(|b| *b)
    }
}

/// How [`PatchFs::open`] opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFor {
    Read,
    /// Reading and writing, never creating and never truncating (write in place).
    Patch,
}

/// One chunk of a whole read: its offset and bytes; what comes back is written to the
/// destination at that offset (`None`: nothing is).
pub type Each<'a> = dyn FnMut(u64, &[u8]) -> Result<Option<Vec<u8>>> + 'a;

/// A connection a hex Save can run on: [`RemoteFs`] plus files by handle. A request that changes
/// the remote and gets no reply fails as [`OutcomeUnknown`](super::OutcomeUnknown); every
/// request has a timeout.
pub trait PatchFs: RemoteFs {
    /// Whether the server copies a file itself (SFTP: `copy-data`, OpenSSH 9.0 and later).
    fn can_copy(&self) -> bool;

    /// Opens the entry `path` itself.
    fn open(&mut self, path: &str, how: OpenFor) -> Result<Handle>;

    /// Creates `path` exclusively, private (0600), open for reading and writing. A failure says
    /// whether the path was created; only a reply proves it was.
    fn create_temp(&mut self, path: &str) -> std::result::Result<Handle, CreateError>;

    /// `fstat`: what the handle holds now, whatever its name has become.
    fn fstat(&mut self, file: &mut Handle) -> Result<Stat>;

    /// Up to `len` bytes at `offset`, read from the remote now; fewer only at the end of the file.
    fn read_at(&mut self, file: &mut Handle, offset: u64, len: usize) -> Result<Vec<u8>>;

    /// Reads the first `size` bytes of `src` from the remote, in file order (SFTP: pipelined, up
    /// to 4 MiB in flight, 06 §2), handing each chunk to `each`; what `each` returns is written
    /// to `dst` at the chunk's offset. Returns the bytes read: fewer than `size` when the file
    /// ends early. An error from `each` ends it.
    fn read_through(
        &mut self,
        src: &mut Handle,
        size: u64,
        dst: Option<&mut Handle>,
        each: &mut Each<'_>,
    ) -> Result<u64>;

    /// Writes `bytes` at `offset`.
    fn write_at(&mut self, file: &mut Handle, offset: u64, bytes: &[u8]) -> Result<()>;

    /// Gives the open file an owner (`fsetstat`), never by path.
    fn set_owner(&mut self, file: &mut Handle, uid: u32, gid: u32) -> Result<()>;

    /// Sets the open file's permission bits (`fsetstat`), never by path.
    fn set_handle_mode(&mut self, file: &mut Handle, mode: u32) -> Result<()>;

    /// Flushes the open file to disk (SFTP: `fsync@openssh.com`; nothing where the server does
    /// not offer it).
    fn fsync(&mut self, file: &mut Handle) -> Result<()>;

    /// Copies the first `size` bytes of `src` into `dst` on the server, both from their start,
    /// in requests sized by `ctl.pace`.
    fn copy(
        &mut self,
        src: &mut Handle,
        dst: &mut Handle,
        size: u64,
        ctl: &mut CopyCtl<'_>,
    ) -> Result<()>;

    /// Closes the handle. Never fails: a close that got no reply is the session's to drop.
    fn close(&mut self, file: Handle);
}

// ---------------------------------------------------------------------------------------------
// The edits.

/// One run of changed bytes: `bytes` wanted at `offset`. (In an undo file: the bytes that were
/// there.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

impl Patch {
    fn end(&self) -> u64 {
        self.offset + self.bytes.len() as u64
    }
}

/// The hex window's patches (offset → byte) as runs of adjacent bytes, in file order.
pub fn runs_of(patches: &BTreeMap<u64, u8>) -> Vec<Patch> {
    let mut runs: Vec<Patch> = Vec::new();
    for (&offset, &byte) in patches {
        match runs.last_mut() {
            Some(run) if run.end() == offset => run.bytes.push(byte),
            _ => runs.push(Patch {
                offset,
                bytes: vec![byte],
            }),
        }
    }
    runs
}

/// The 4 KiB pages `runs` touch.
pub fn pages_of(runs: &[Patch]) -> BTreeSet<u64> {
    let mut pages = BTreeSet::new();
    for run in runs {
        let mut page = run.offset / PAGE * PAGE;
        while page < run.end() {
            pages.insert(page);
            page += PAGE;
        }
    }
    pages
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

// ---------------------------------------------------------------------------------------------
// The undo file.

/// The bytes a session's Saves replaced (§5.1): `<cache>/.backup/<host>/<abs path>.<UTC
/// stamp>.undo.txt`, a new file for the owner only. A header names the source and its size and
/// modification time as opened; then one `0xOFF: OLD` line per run. One per editing session:
/// each Save adds the offsets it changes for the first time, and an offset already there keeps
/// the byte recorded first, so the file always holds the file as opened. It is rewritten
/// through a temp and a rename, fsynced, never in place.
///
/// Writing it back needs sopen ([`UndoFile::parse`], "Load as edits"): it is not a file `cp`
/// restores.
#[derive(Debug)]
pub struct UndoFile {
    path: PathBuf,
    /// The name is this session's: reserved by an exclusive create.
    reserved: bool,
    source: String,
    opened: Fingerprint,
    bytes: BTreeMap<u64, u8>,
}

/// An undo file read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undo {
    /// `host:/abs` as the header names it.
    pub source: String,
    /// The file's size when the session opened it.
    pub size: u64,
    /// The bytes replaced, as runs: loaded as edits, they put those bytes back.
    pub runs: Vec<Patch>,
}

impl UndoFile {
    /// The undo file of a session editing `abs` on `host`, opened as `opened`. Nothing is
    /// written until the first [`UndoFile::add`].
    pub fn new(cache: &Path, host: &str, abs: &str, opened: Fingerprint, at: SystemTime) -> Self {
        let mut path = crate::cache::backup_path(cache, host, abs, at).into_os_string();
        path.push(".undo.txt");
        UndoFile {
            path: PathBuf::from(path),
            reserved: false,
            source: format!("{host}:{abs}"),
            opened,
            bytes: BTreeMap::new(),
        }
    }

    /// Where it is (or will be, once the first Save reserves the name).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many bytes it holds.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Adds the bytes `replaced` (each run: the bytes at its offset before this Save) and makes
    /// the file durable: written to a temp, fsynced, renamed into place, the folder fsynced. An
    /// offset already recorded keeps its first byte. Returns only once that is done — the Save
    /// patches nothing before.
    pub fn add(&mut self, replaced: &[Patch]) -> Result<()> {
        let mut fresh = false;
        for run in replaced {
            for (i, &byte) in run.bytes.iter().enumerate() {
                self.bytes.entry(run.offset + i as u64).or_insert_with(|| {
                    fresh = true;
                    byte
                });
            }
        }
        if !fresh && self.reserved {
            return Ok(());
        }
        if !self.reserved {
            let wanted = self.path.clone();
            let (path, _) = crate::cache::create_unique(|attempt| match attempt {
                0 => wanted.clone(),
                n => {
                    let name = wanted.to_string_lossy();
                    let stem = name.strip_suffix(".undo.txt").unwrap_or(&name);
                    PathBuf::from(format!("{stem}-{n}.undo.txt"))
                }
            })?;
            self.path = path;
            self.reserved = true;
        }
        let dir = self.path.parent().context("the undo file has no folder")?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("cannot remove {}", tmp.display())),
        }
        let written = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(self.text().as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, &self.path)?;
            journal::Fs::sync_dir(&journal::Disk, dir)
        })();
        written.with_context(|| format!("cannot write the undo file {}", self.path.display()))
    }

    /// The file's text.
    pub fn text(&self) -> String {
        let mut text = format!(
            "# sopen undo file: the bytes its saves replaced in {}\n\
             # source: {}\n\
             # as opened: {} bytes, modified {}\n\
             # one line per run of replaced bytes — offset: the bytes before the save (hex)\n",
            self.source,
            self.source,
            self.opened.size,
            crate::cache::rfc3339(self.opened.mtime)
        );
        for run in runs_of(&self.bytes) {
            text.push_str(&format!("0x{:X}: {}\n", run.offset, hex(&run.bytes)));
        }
        text
    }

    /// Reads an undo file's text back.
    pub fn parse(text: &str) -> Result<Undo> {
        let mut source = None;
        let mut size = None;
        let mut bytes = BTreeMap::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("# source: ") {
                source = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("# as opened: ") {
                size = rest.split(' ').next().and_then(|n| n.parse::<u64>().ok());
            }
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let parsed = (|| -> Result<(u64, Vec<u8>)> {
                let (off, old) = line.split_once(": ").context("no `: `")?;
                let off = off.strip_prefix("0x").context("an offset is 0x…")?;
                let off = u64::from_str_radix(off, 16)?;
                ensure!(
                    !old.is_empty() && old.len().is_multiple_of(2) && old.is_ascii(),
                    "bytes are pairs of hex digits"
                );
                let old = (0..old.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&old[i..i + 2], 16))
                    .collect::<std::result::Result<Vec<u8>, _>>()?;
                Ok((off, old))
            })();
            let (off, old) = parsed.with_context(|| format!("`{line}` is not an undo line"))?;
            for (i, byte) in old.into_iter().enumerate() {
                let at = off
                    .checked_add(i as u64)
                    .context("past the end of any file")?;
                bytes.insert(at, byte);
            }
        }
        Ok(Undo {
            source: source.context("the undo file names no source")?,
            size: size.context("the undo file does not say the size as opened")?,
            runs: runs_of(&bytes),
        })
    }

    /// The undo files kept for `abs` on `host`, newest first (the hex window's recovery rows).
    pub fn list(cache: &Path, host: &str, abs: &str) -> Vec<PathBuf> {
        let probe = crate::cache::backup_path(cache, host, abs, SystemTime::UNIX_EPOCH);
        let (Some(dir), Some(name)) = (probe.parent(), probe.file_name()) else {
            return Vec::new();
        };
        // `<file>.<stamp>` without the stamp: the stamp has no dot in it.
        let name = name.to_string_lossy();
        let Some((file, _)) = name.rsplit_once('.') else {
            return Vec::new();
        };
        let prefix = format!("{file}.");
        // `<stamp>` or `<stamp>-<n>` (a later session of the same second).
        let mut found: Vec<((String, u32), PathBuf)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let id = name.strip_prefix(&prefix)?.strip_suffix(".undo.txt")?;
                let (stamp, n) = match id.split_once('-') {
                    Some((stamp, n)) => (stamp, n.parse().ok()?),
                    None => (id, 0),
                };
                (!stamp.contains('.')).then(|| ((stamp.to_string(), n), e.path()))
            })
            .collect();
        found.sort();
        found.reverse();
        found.into_iter().map(|(_, path)| path).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// The baseline pass.

/// The whole-file hash a Save of at most 256 MiB compares (§4.5), taken at the first edit: fed
/// the file's bytes in order, it keeps the SHA-256 of the whole and an 8-byte hash (truncated
/// SHA-256) of every 4 KiB page — at most 512 KiB for 256 MiB.
pub struct BaselinePass {
    whole: Sha256,
    page: Sha256,
    in_page: u64,
    pages: Vec<[u8; 8]>,
    len: u64,
}

/// A finished [`BaselinePass`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Baseline {
    pub size: u64,
    pub sha256: Sha,
    pages: Vec<[u8; 8]>,
}

fn short(sha: &Sha) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&sha[..8]);
    out
}

impl Default for BaselinePass {
    fn default() -> Self {
        Self::new()
    }
}

impl BaselinePass {
    pub fn new() -> Self {
        BaselinePass {
            whole: Sha256::new(),
            page: Sha256::new(),
            in_page: 0,
            pages: Vec::new(),
            len: 0,
        }
    }

    /// The next bytes of the file.
    pub fn update(&mut self, mut bytes: &[u8]) {
        self.whole.update(bytes);
        self.len += bytes.len() as u64;
        while !bytes.is_empty() {
            let take = ((PAGE - self.in_page) as usize).min(bytes.len());
            self.page.update(&bytes[..take]);
            self.in_page += take as u64;
            bytes = &bytes[take..];
            if self.in_page == PAGE {
                self.end_page();
            }
        }
    }

    fn end_page(&mut self) {
        let sha: Sha = std::mem::take(&mut self.page).finalize().into();
        self.pages.push(short(&sha));
        self.in_page = 0;
    }

    pub fn finish(mut self) -> Baseline {
        if self.in_page > 0 {
            self.end_page();
        }
        Baseline {
            size: self.len,
            sha256: self.whole.finalize().into(),
            pages: self.pages,
        }
    }
}

impl Baseline {
    /// The pass's short hash of the page at `offset`.
    pub fn page(&self, offset: u64) -> Option<[u8; 8]> {
        self.pages.get((offset / PAGE) as usize).copied()
    }

    /// Whether the pass read, under every edited page, the bytes the window showed (`seen`: page
    /// offset → the SHA-256 of the page as shown). `Err`: the first page that differs — the file
    /// changed under a page the window showed, with size and mtime unchanged, and this baseline
    /// is not accepted ([`CHANGED_UNDER_EDITS`]). It holds whether the page was first edited
    /// before the pass or after it.
    pub fn agrees(&self, seen: &BTreeMap<u64, Sha>) -> std::result::Result<(), u64> {
        match seen
            .iter()
            .find(|(&off, sha)| self.page(off) != Some(short(sha)))
        {
            Some((&off, _)) => Err(off),
            None => Ok(()),
        }
    }
}

/// Runs the baseline pass over `path` on a fresh handle: the whole file, at most 256 MiB, with
/// `progress(done, size)` for `checking the file for Save — 37 %`. The handle's `fstat` must
/// equal `opened` before and after the read; anything else is an error naming the change.
pub fn baseline_pass(
    fs: &mut dyn PatchFs,
    path: &str,
    opened: &Fingerprint,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Baseline> {
    ensure!(
        opened.size <= HASH_CHECK_MAX,
        "{path} is over 256 MiB: its saves check the pages under the edits, not a whole hash"
    );
    let mut file = fs.open(path, OpenFor::Read)?;
    let run = (|| -> Result<Baseline> {
        let before = fs.fstat(&mut file)?;
        if let Some(how) = differs(&before, opened) {
            bail!("{path} changed since it was opened ({how})");
        }
        let mut pass = BaselinePass::new();
        let size = opened.size;
        let read = fs.read_through(&mut file, size, None, &mut |_, chunk| {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!(Cancelled));
            }
            pass.update(chunk);
            progress(pass.len, size);
            Ok(None)
        })?;
        let after = fs.fstat(&mut file)?;
        if let Some(how) = differs(&after, opened) {
            bail!("{path} changed while it was checked ({how})");
        }
        ensure!(
            read == size,
            "{path} changed while it was checked (it ended after {read} of {size} bytes)"
        );
        Ok(pass.finish())
    })();
    fs.close(file);
    run
}

/// The status row while the baseline pass runs.
pub fn checking_line(done: u64, size: u64) -> String {
    format!("checking the file for Save — {} %", percent(done, size))
}

// ---------------------------------------------------------------------------------------------
// Steps, hooks and what a Save says.

/// A step of a Save, as [`Ctl::hook`] hears of it right before the step's request is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// §4.2 step 1: the target's `lstat`.
    CheckTarget,
    /// Step 2: `statvfs` of the folder.
    Room,
    /// Step 3: the source is opened; then its handle's `fstat`.
    OpenSource,
    StatSource,
    /// Step 4: the exclusive create (the journal already says `naming`); then the temp's
    /// `fstat` (after it the journal says `created`), its owner and its mode.
    Create,
    StatCreated,
    SetOwner,
    SetMode,
    /// Step 5: the copy request of this number, from 0.
    Copy(u32),
    /// §4.3: the one pass through this machine.
    Rewrite,
    /// Step 6: the temp read back (above 256 MiB: once for every edited page).
    ReadBack,
    /// Step 7: the undo file.
    Undo,
    /// Step 8: the write of this run, from 0; then the full mode, the fsync, the `fstat`.
    Patch(usize),
    FullMode,
    Fsync,
    StatTemp,
    /// Step 9: the source's `fstat` and the target's `lstat` (after them the journal says
    /// `staged`, then `renaming`).
    Recheck,
    /// Step 10: the rename; then the target's `lstat`.
    Rename,
    StatAfter,
    /// A refused or failed Save removing its own temp.
    RemoveTemp,
    /// A Save copy publishing its temp under the new name (a hard link).
    Link,
    /// §4.4: the pages read from the target itself; the write of this run, from 0 (the journal
    /// already says `writing`); the fsync is [`Step::Fsync`].
    ReadPages,
    Write(usize),
}

/// What a hook makes of a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    Go,
    /// The Save ends here as a kill would end it: the step's request is not sent, nothing is
    /// cleaned up and the journal's `save` record stays as it is. The error is [`Stopped`].
    Stop,
    /// The step's request is sent and takes effect, and its reply never comes.
    LoseReply,
}

/// A Save a hook stopped ([`At::Stop`]).
#[derive(Debug)]
pub struct Stopped(pub Step);

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped before {:?}", self.0)
    }
}

impl std::error::Error for Stopped {}

pub fn is_stopped(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Stopped>())
}

/// A step hook.
pub type Hook<'a> = Option<&'a mut dyn FnMut(Step) -> At>;

/// Where a Save is, for the status row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// The server copies: bytes copied of the file's size.
    Copying { done: u64, total: u64 },
    /// The streamed rewrite: bytes through this machine.
    Rewriting { done: u64, total: u64 },
    /// The content check reads the temp back.
    Checking { done: u64, total: u64 },
    /// The runs are written.
    Patching,
}

fn percent(done: u64, total: u64) -> u64 {
    if total == 0 {
        100
    } else {
        (u128::from(done) * 100 / u128::from(total)) as u64
    }
}

/// `8.9 of 24.0 GB`, in the unit of the whole.
fn part_of(done: u64, total: u64) -> String {
    const UNITS: [(&str, f64); 4] = [("GB", 1e9), ("MB", 1e6), ("KB", 1e3), ("B", 1.0)];
    let (unit, per) = UNITS
        .into_iter()
        .find(|(_, per)| total as f64 >= *per)
        .unwrap_or(("B", 1.0));
    if per == 1.0 {
        format!("{done} of {total} B")
    } else {
        format!(
            "{:.1} of {:.1} {unit}",
            done as f64 / per,
            total as f64 / per
        )
    }
}

impl Progress {
    /// The status row: `saving — copying on <host> 37 % (8.9 of 24.0 GB)`.
    pub fn line(&self, host: &str) -> String {
        match *self {
            Progress::Copying { done, total } => format!(
                "saving — copying on {host} {} % ({})",
                percent(done, total),
                part_of(done, total)
            ),
            Progress::Rewriting { done, total } => format!(
                "saving — rewriting {host}'s file through this machine {} % ({})",
                percent(done, total),
                part_of(done, total)
            ),
            Progress::Checking { done, total } => format!(
                "saving — checking the copy on {host} {} % ({})",
                percent(done, total),
                part_of(done, total)
            ),
            Progress::Patching => format!("saving — writing your changes on {host}"),
        }
    }
}

/// What a Save runs by.
pub struct Ctl<'a> {
    /// Checked between copy requests and between chunks; never once the rename is due.
    pub cancel: &'a AtomicBool,
    pub progress: &'a mut dyn FnMut(Progress),
    /// The copy's pace: [`Pace::timed`].
    pub pace: Pace,
    /// Times the copy requests ([`stopwatch`]).
    pub clock: &'a mut dyn FnMut() -> Duration,
    /// Stops the Save at a step, or loses the step's reply (tests, E18).
    pub hook: Hook<'a>,
}

impl<'a> Ctl<'a> {
    pub fn new(
        cancel: &'a AtomicBool,
        progress: &'a mut dyn FnMut(Progress),
        clock: &'a mut dyn FnMut() -> Duration,
    ) -> Ctl<'a> {
        Ctl {
            cancel,
            progress,
            pace: Pace::timed(),
            clock,
            hook: None,
        }
    }
}

/// What a Save writes: the runs, where, and what the window knows of the file. The baseline and
/// the evidence (`seen`: each edited page as shown) are the journal snapshot's.
pub struct Job<'a> {
    /// The host as shown (`host` of `host:/abs`).
    pub host: &'a str,
    /// The file's absolute path on it.
    pub target: &'a str,
    /// The changed runs, in file order, none overlapping.
    pub runs: &'a [Patch],
    /// The baseline pass, at most 256 MiB: the Save is refused when it disagrees with a page
    /// the window showed (§4.5).
    pub pass: Option<&'a Baseline>,
    /// The link's measured rate in bytes per second, for the cost a refusal states.
    pub rate: Option<f64>,
}

/// Why a Save wrote nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// The server cannot replace a file in one step (no `posix-rename@openssh.com`).
    CannotReplace,
    /// The target is no longer the file that was opened: deleted, no longer a regular file,
    /// another size or modification time, or — by the whole-content check — other content.
    Changed { now: Option<Stat>, reason: String },
    /// The page check: an edited page no longer holds what the window showed.
    Page {
        offset: u64,
        shown: Sha,
        now: Option<Sha>,
    },
    /// The baseline pass read other bytes under an edited page than the window showed.
    UnderEdits { offset: u64 },
    /// No room for the temp in the target's folder.
    NoRoom { dir: String, need: u64, free: u64 },
    /// No `copy-data`, and the file is over 256 MiB.
    NoCopyData { size: u64, rate: Option<f64> },
}

impl Refusal {
    /// Whether the refusal's row offers Write in place… and Save As… (§4.1).
    pub fn offers_other_ways(&self) -> bool {
        matches!(self, Refusal::NoRoom { .. } | Refusal::NoCopyData { .. })
    }
}

/// `about 3 h 40 min`, `about 12 min`, `about 40 s`.
fn about(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs >= 3600.0 {
        let tens = (secs / 600.0).round() as u64;
        match (tens / 6, tens % 6 * 10) {
            (h, 0) => format!("about {h} h"),
            (h, m) => format!("about {h} h {m} min"),
        }
    } else if secs >= 60.0 {
        format!("about {} min", (secs / 60.0).round() as u64)
    } else {
        format!("about {} s", secs.round().max(1.0) as u64)
    }
}

fn short_hex(sha: &Sha) -> String {
    hex(&sha[..8]).to_lowercase()
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::CannotReplace => f.write_str(
                "this server cannot replace a file in one step (it does not offer \
                 posix-rename@openssh.com)",
            ),
            Refusal::Changed { reason, .. } => {
                write!(f, "it changed since it was opened ({reason})")
            }
            Refusal::Page { offset, shown, now } => write!(
                f,
                "the bytes under your edits changed at 0x{offset:X}: shown {}… now {}",
                short_hex(shown),
                now.as_ref().map_or(
                    "nothing (the file ends before it)".to_string(),
                    |n| format!("{}…", short_hex(n))
                )
            ),
            Refusal::UnderEdits { .. } => f.write_str(CHANGED_UNDER_EDITS),
            Refusal::NoRoom { dir, need, free } => write!(
                f,
                "needs {} free in {dir}, has {}",
                crate::human(*need),
                crate::human(*free)
            ),
            Refusal::NoCopyData { size, rate } => {
                let moved = size.saturating_mul(2);
                write!(
                    f,
                    "rewriting {} through this machine moves {}",
                    crate::human(*size),
                    crate::human(moved)
                )?;
                if let Some(rate) = rate.filter(|r| *r > 0.0) {
                    write!(
                        f,
                        " ({} at the current {}/s)",
                        about(moved as f64 / rate),
                        crate::human(rate as u64)
                    )?;
                }
                f.write_str("; this server has no copy-data (OpenSSH before 9.0)")
            }
        }
    }
}

/// A Save that landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub route: Route,
    /// The next baseline: the staged temp's `(size, mtime)` (write in place: the target's after
    /// the fsync), with the new whole SHA-256 at most 256 MiB.
    pub next: Fingerprint,
    /// The next `seen`: each edited page's SHA-256 as saved.
    pub seen: BTreeMap<u64, Sha>,
    /// The undo file holding the bytes replaced.
    pub undo: PathBuf,
    /// Above 256 MiB, what was checked: `checked size, modification time and the 8 KiB under
    /// your edits, not the whole content`.
    pub checked: Option<String>,
    /// What the user must hear besides (the remote changed right after the save).
    pub notes: Vec<String>,
}

impl Saved {
    /// `saved host:/abs · backup <undo file>`.
    pub fn line(&self, host: &str, target: &str) -> String {
        format!("saved {host}:{target} · backup {}", self.undo.display())
    }
}

/// How a Save ended when it did not fail. A failure is an `Err`: [`Cancelled`] when it was
/// stopped by `cancel` (the temp removed, the target untouched); an
/// [`OutcomeUnknown`](super::OutcomeUnknown) when a request that may have changed the target
/// got no reply ("check <path>", never "nothing was written"); any other error wrote nothing to
/// the target, and names a temp it could not remove.
#[derive(Debug, Clone, PartialEq)]
pub enum SaveOutcome {
    Saved(Saved),
    /// Nothing was written to the target. `notes` names anything left behind.
    Refused {
        why: Refusal,
        notes: Vec<String>,
    },
}

fn refused(why: Refusal) -> Result<SaveOutcome> {
    Ok(SaveOutcome::Refused {
        why,
        notes: Vec::new(),
    })
}

/// Fires the hook for `step`: `Ok(true)` when the step's reply is to be lost.
fn fire(hook: &mut Hook<'_>, step: Step) -> Result<bool> {
    match hook.as_mut().map_or(At::Go, |h| h(step)) {
        At::Go => Ok(false),
        At::LoseReply => Ok(true),
        At::Stop => Err(anyhow!(Stopped(step))),
    }
}

/// One request of a step. `changes`: it changes the remote, so a lost reply is an unknown
/// outcome; a read that got no reply is only a failure.
fn ask<T>(
    hook: &mut Hook<'_>,
    step: Step,
    changes: bool,
    what: &str,
    request: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lose = fire(hook, step)?;
    let got = request();
    if !lose {
        return got;
    }
    drop(got);
    Err(match changes {
        true => unknown(format!(
            "{what}: request sent, no reply — it may or may not have happened"
        )),
        false => anyhow!("{what}: no reply from the server"),
    })
}

/// How `now` (an `fstat` of an open file) differs from the baseline; `None`: a regular file of
/// that size and modification time.
fn differs(now: &Stat, at: &Fingerprint) -> Option<String> {
    if !now.is_file {
        return Some("it is no longer a regular file".to_string());
    }
    (now.size != at.size || now.mtime != at.mtime).then(|| describe_change(at, now))
}

/// `e` as the failure of a request on a temp, once the temp's fate is settled: a lost reply
/// there leaves nothing unknown about the target, which no request of it touched. The words
/// stay; a cancel stays a cancel.
fn on_the_temp(e: anyhow::Error) -> anyhow::Error {
    match super::is_outcome_unknown(&e) && !super::is_cancelled(&e) {
        true => anyhow!("{e:#}"),
        false => e,
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    match cancel.load(Ordering::Relaxed) {
        true => Err(anyhow!(Cancelled)),
        false => Ok(()),
    }
}

// ---------------------------------------------------------------------------------------------
// The content check, over bytes read fresh.

/// What the fresh reads gave: the bytes each run replaces, the edited pages' hashes with the
/// patches applied, and (a whole read) the SHA-256 of the result.
struct Scanned {
    replaced: Vec<Patch>,
    new_pages: BTreeMap<u64, Sha>,
    new_whole: Option<Sha>,
}

/// Takes the file's bytes as they are read — the whole file in order, or only the edited pages —
/// and compares them with what the window showed.
struct Scan<'a> {
    runs: &'a [Patch],
    /// The whole file is fed: its hash before and after the patches is taken.
    whole: bool,
    old: Sha256,
    new: Sha256,
    pages: BTreeMap<u64, (Sha256, Sha256)>,
    replaced: Vec<Vec<u8>>,
    got: Vec<usize>,
}

impl<'a> Scan<'a> {
    fn new(runs: &'a [Patch], whole: bool) -> Self {
        Scan {
            runs,
            whole,
            old: Sha256::new(),
            new: Sha256::new(),
            pages: pages_of(runs)
                .into_iter()
                .map(|page| (page, (Sha256::new(), Sha256::new())))
                .collect(),
            replaced: runs.iter().map(|r| vec![0u8; r.bytes.len()]).collect(),
            got: vec![0; runs.len()],
        }
    }

    /// `chunk` is what the file holds at `at`. Returns it with the patches applied.
    fn feed<'c>(&mut self, at: u64, chunk: &'c [u8]) -> Cow<'c, [u8]> {
        let end = at + chunk.len() as u64;
        let mut out = Cow::Borrowed(chunk);
        let first = self.runs.partition_point(|r| r.end() <= at);
        for (i, run) in self.runs.iter().enumerate().skip(first) {
            if run.offset >= end {
                break;
            }
            let (from, to) = (run.offset.max(at), run.end().min(end));
            let (a, b) = ((from - at) as usize, (to - at) as usize);
            let (ra, rb) = ((from - run.offset) as usize, (to - run.offset) as usize);
            self.replaced[i][ra..rb].copy_from_slice(&chunk[a..b]);
            self.got[i] += rb - ra;
            out.to_mut()[a..b].copy_from_slice(&run.bytes[ra..rb]);
        }
        if self.whole {
            self.old.update(chunk);
            self.new.update(&out);
        }
        for (&page, (old, new)) in self.pages.range_mut(at / PAGE * PAGE..end) {
            let (from, to) = (page.max(at), (page + PAGE).min(end));
            if from < to {
                let (a, b) = ((from - at) as usize, (to - at) as usize);
                old.update(&chunk[a..b]);
                new.update(&out[a..b]);
            }
        }
        out
    }

    /// The comparison: every edited page against `seen`, and (a whole read) the whole against
    /// `want`.
    fn finish(
        self,
        seen: &BTreeMap<u64, Sha>,
        want: Option<Sha>,
    ) -> Result<std::result::Result<Scanned, Refusal>> {
        let mut new_pages = BTreeMap::new();
        for (page, (old, new)) in self.pages {
            let shown = *seen
                .get(&page)
                .with_context(|| format!("the edited page at 0x{page:x} has no evidence"))?;
            let old: Sha = old.finalize().into();
            if old != shown {
                return Ok(Err(Refusal::Page {
                    offset: page,
                    shown,
                    now: Some(old),
                }));
            }
            new_pages.insert(page, new.finalize().into());
        }
        let mut new_whole = None;
        if self.whole {
            let want = want.context("the file's whole SHA-256 is not known")?;
            let old: Sha = self.old.finalize().into();
            if old != want {
                return Ok(Err(Refusal::Changed {
                    now: None,
                    reason: SAME_STAT_OTHER_CONTENT.to_string(),
                }));
            }
            new_whole = Some(self.new.finalize().into());
        }
        for (run, got) in self.runs.iter().zip(&self.got) {
            ensure!(
                *got == run.bytes.len(),
                "the bytes at 0x{:x} could not be read back whole",
                run.offset
            );
        }
        let replaced = self
            .runs
            .iter()
            .zip(self.replaced)
            .map(|(run, bytes)| Patch {
                offset: run.offset,
                bytes,
            })
            .collect();
        Ok(Ok(Scanned {
            replaced,
            new_pages,
            new_whole,
        }))
    }
}

/// What a Save needs to know before it sends anything: the evidence, and runs that fit it.
fn evidence<'s>(job: &Job<'_>, snap: &'s Snapshot) -> Result<&'s BTreeMap<u64, Sha>> {
    let Seen::Known(ev) = &snap.seen else {
        bail!("a save starts once the file's evidence is known");
    };
    ensure!(!job.runs.is_empty(), "nothing to save");
    let mut end = 0u64;
    for run in job.runs {
        ensure!(!run.bytes.is_empty(), "an empty run at 0x{:x}", run.offset);
        ensure!(
            run.offset >= end,
            "the runs overlap or are out of order at 0x{:x}",
            run.offset
        );
        end = run
            .offset
            .checked_add(run.bytes.len() as u64)
            .context("a run past the end of any file")?;
    }
    ensure!(
        end <= snap.baseline.size,
        "a hex edit never changes the size: a run ends at 0x{end:x}, past the file's {} bytes",
        snap.baseline.size
    );
    if let Some(page) = pages_of(job.runs)
        .into_iter()
        .find(|p| !ev.pages.contains_key(p))
    {
        bail!("the edited page at 0x{page:x} has no evidence");
    }
    if snap.baseline.size <= HASH_CHECK_MAX {
        ensure!(
            snap.baseline.sha256.is_some(),
            "{} has not been checked for Save yet (the baseline pass has not ended)",
            job.target
        );
    }
    Ok(&ev.pages)
}

/// §4.5 "the baseline must agree with what the window showed".
fn under_edits(job: &Job<'_>, seen: &BTreeMap<u64, Sha>) -> Option<Refusal> {
    let edited: BTreeMap<u64, Sha> = pages_of(job.runs)
        .into_iter()
        .filter_map(|p| seen.get(&p).map(|sha| (p, *sha)))
        .collect();
    job.pass?
        .agrees(&edited)
        .err()
        .map(|offset| Refusal::UnderEdits { offset })
}

/// `checked size, modification time and the 8 KiB under your edits, not the whole content`.
fn checked_line(size: u64, pages: usize) -> Option<String> {
    (size > HASH_CHECK_MAX).then(|| {
        format!(
            "checked size, modification time and the {} KiB under your edits, not the whole \
             content",
            pages as u64 * PAGE / 1024
        )
    })
}

// ---------------------------------------------------------------------------------------------
// The route.

/// §4.1: how a hex Save of a file of `size` bytes at `target` can be written — decided before
/// anything is, from what the server offers and the room in the target's folder.
pub fn route(
    fs: &mut dyn PatchFs,
    target: &str,
    size: u64,
    rate: Option<f64>,
) -> std::result::Result<Route, Refusal> {
    if !fs.can_replace() {
        return Err(Refusal::CannotReplace);
    }
    staging_route(fs, target, size, rate)
}

/// How the temp is filled, whatever it is published by (a Save's rename, a Save copy's link).
fn staging_route(
    fs: &mut dyn PatchFs,
    target: &str,
    size: u64,
    rate: Option<f64>,
) -> std::result::Result<Route, Refusal> {
    let dir = split_parent(target).0;
    let need = size.saturating_add(ROOM_MARGIN);
    if let Some(free) = fs.free_space(dir).filter(|free| *free < need) {
        return Err(Refusal::NoRoom {
            dir: dir.to_string(),
            need,
            free,
        });
    }
    if fs.can_copy() {
        Ok(Route::ServerCopy)
    } else if size <= HASH_CHECK_MAX {
        Ok(Route::StreamedRewrite)
    } else {
        Err(Refusal::NoCopyData { size, rate })
    }
}

// ---------------------------------------------------------------------------------------------
// Save: a temp, then a rename.

/// Saves `job.runs` into `job.target` by a temp and a rename (§4.2; §4.3 where the server has no
/// `copy-data` and the file is at most 256 MiB), or refuses having created nothing.
///
/// - `snap` is the generation this Save writes: its baseline and `seen` are what every check
///   compares, and `journal` records it with the `save` intent before the temp is created, then
///   each phase before the request that needs it.
/// - `undo` gets the bytes replaced, durable before the temp is patched.
/// - On [`SaveOutcome::Saved`] the journal's record says `renaming`: the caller ends it with
///   [`Journal::save_landed`] (it knows which edits remain) and reloads its Source.
/// - On a refusal or a failure this function ends the record itself: removed when this Save's
///   temp is gone, kept as an unknown outcome ([`Journal::save_unknown`]) when a temp may be
///   left or the rename got no reply.
pub fn save(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    journal: &mut Journal,
    snap: &Snapshot,
    undo: &mut UndoFile,
    ctl: &mut Ctl<'_>,
) -> Result<SaveOutcome> {
    let seen = evidence(job, snap)?;
    if let Some(why) = under_edits(job, seen) {
        return refused(why);
    }
    if !fs.can_replace() {
        return refused(Refusal::CannotReplace);
    }
    let target = job.target;
    let base = &snap.baseline;
    let checked = ask(&mut ctl.hook, Step::CheckTarget, false, "stat", || {
        check_target(fs, target, base)
    })?;
    let current = match checked {
        Check::Same(now) => now,
        Check::Changed(now, reason) => return refused(Refusal::Changed { now, reason }),
    };
    fire(&mut ctl.hook, Step::Room)?;
    let route = match route(fs, target, base.size, job.rate) {
        Ok(route) => route,
        Err(why) => return refused(why),
    };
    let mut src = ask(&mut ctl.hook, Step::OpenSource, false, "open", || {
        fs.open(target, OpenFor::Read)
    })?;
    let done = with_source(
        fs, job, journal, snap, seen, undo, ctl, &current, route, &mut src,
    );
    if !done.as_ref().is_err_and(is_stopped) {
        fs.close(src);
    }
    done
}

#[allow(clippy::too_many_arguments)]
fn with_source(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    journal: &mut Journal,
    snap: &Snapshot,
    seen: &BTreeMap<u64, Sha>,
    undo: &mut UndoFile,
    ctl: &mut Ctl<'_>,
    current: &Stat,
    route: Route,
    src: &mut Handle,
) -> Result<SaveOutcome> {
    let target = job.target;
    let opened = ask(&mut ctl.hook, Step::StatSource, false, "stat", || {
        fs.fstat(src)
    })?;
    if let Some(reason) = differs(&opened, &snap.baseline) {
        return refused(Refusal::Changed {
            now: Some(opened),
            reason,
        });
    }
    cancelled(ctl.cancel)?;
    // Step 4. The name goes into the journal before the create; only the create's reply makes
    // the temp this Save's.
    let (dir, name) = split_parent(target);
    let temp_path = join(dir, &format!(".{name}.sopen-{}", random_hex()));
    let plan = Plan::Temp {
        route,
        target: target.to_string(),
        temp: temp_path.clone(),
    };
    if let Err(e) = journal.begin_save(snap, plan) {
        // Nothing was sent: a record that reached the disk is cleared.
        if journal.pending_save().is_some() {
            let _ = journal.clear_save();
        }
        return Err(e.context(format!(
            "{target} was not touched: the save could not be recorded in the journal"
        )));
    }
    let lose = fire(&mut ctl.hook, Step::Create)?;
    let made = match (fs.create_temp(&temp_path), lose) {
        (made, false) => made,
        (_, true) => Err(CreateError::new(
            Created::Unknown,
            unknown(format!("create {temp_path}: request sent, no reply")),
        )),
    };
    let mut temp = match made {
        Ok(temp) => temp,
        Err(e) if e.created == Created::No => {
            // Whatever has that name is not this Save's: never removed, never changed.
            let e = e.error.context(format!("{target} was not replaced"));
            return Err(match journal.save_not_landed() {
                Ok(()) => e,
                Err(je) => e.context(format!("{je:#}")),
            });
        }
        Err(e) => {
            journal.save_unknown();
            let what = format!("{target} was not replaced, but creating {temp_path} got no reply");
            return Err(unknown_at(e.error, &what, &temp_path));
        }
    };
    // The temp is this Save's from here: every way out removes it or names it.
    let mut at_rename = false;
    let flow = stage_and_swap(
        fs,
        job,
        journal,
        snap,
        seen,
        undo,
        ctl,
        current,
        route,
        src,
        &mut temp,
        &mut at_rename,
    );
    match flow {
        Ok(Ok(saved)) => {
            fs.close(temp);
            Ok(SaveOutcome::Saved(saved))
        }
        Ok(Err(why)) => {
            let notes = drop_temp(fs, temp, journal, &mut ctl.hook)?;
            Ok(SaveOutcome::Refused { why, notes })
        }
        // As a kill: nothing more is sent.
        Err(e) if is_stopped(&e) => Err(e),
        Err(e) if at_rename && super::is_outcome_unknown(&e) => {
            // It may have landed: the temp is not removed, and both paths are named.
            journal.save_unknown();
            fs.close(temp);
            let what = format!("the rename of {temp_path} over {target} got no reply");
            Err(unknown_at(e, &what, target))
        }
        Err(e) => {
            // Every request so far was on the temp: the target was not written.
            let mut e = on_the_temp(e).context(format!("{target} was not replaced"));
            for note in drop_temp(fs, temp, journal, &mut ctl.hook)? {
                e = e.context(note);
            }
            Err(e)
        }
    }
}

/// Removes this Save's own temp — only while its name is still the file the handle holds
/// (`fstat` against `lstat`, §3.1) — and closes it. `Ok(Err(why))`: it is left behind.
fn remove_own(
    fs: &mut dyn PatchFs,
    mut temp: Handle,
    hook: &mut Hook<'_>,
) -> Result<std::result::Result<(), String>> {
    let path = temp.path().to_string();
    let lose = fire(hook, Step::RemoveTemp)?;
    let removed = (|| -> std::result::Result<(), String> {
        let held = fs
            .fstat(&mut temp)
            .map_err(|e| format!("it could not be checked: {e:#}"))?;
        let named = match fs.lstat_opt(&path) {
            Ok(Some(named)) => named,
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("it could not be checked: {e:#}")),
        };
        let same =
            named.is_file && !named.is_symlink && FileStat::of(&named) == FileStat::of(&held);
        if !same {
            return Err(format!(
                "another file has its name now ({})",
                describe_stat(&named)
            ));
        }
        let gone = fs.remove(&path);
        match (gone, lose) {
            (Ok(()), false) => Ok(()),
            (Err(e), false) => Err(format!("it could not be removed: {e:#}")),
            (_, true) => Err("its removal got no reply".to_string()),
        }
    })();
    fs.close(temp);
    Ok(removed.map_err(|why| format!("the temp file {path} is left behind ({why})")))
}

/// [`remove_own`], and the end of the journal's record: removed when the temp is gone, kept as
/// an unknown outcome when it is left. Returns what the user must hear.
fn drop_temp(
    fs: &mut dyn PatchFs,
    temp: Handle,
    journal: &mut Journal,
    hook: &mut Hook<'_>,
) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    match remove_own(fs, temp, hook)? {
        Ok(()) => {
            if let Err(e) = journal.save_not_landed() {
                notes.push(format!("{e:#}"));
            }
        }
        Err(left) => {
            journal.save_unknown();
            notes.push(left);
        }
    }
    Ok(notes)
}

/// A temp filled, patched and flushed: its `fstat`, and what the fresh reads gave.
struct Staged {
    stat: Stat,
    scanned: Scanned,
}

/// What the temp takes of the target.
struct Like {
    /// The owner to give it and to hold it to (a Save); `None`: this account's (a Save copy).
    owner: Option<(u32, u32)>,
    /// The mode it has once its content is in.
    mode: u32,
}

/// Steps 4 (after the create) to 8. `Ok(Err(_))`: refused, the temp still to be removed.
#[allow(clippy::too_many_arguments)]
fn stage(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    journal: Option<&mut Journal>,
    base: &Fingerprint,
    seen: &BTreeMap<u64, Sha>,
    undo: Option<&mut UndoFile>,
    ctl: &mut Ctl<'_>,
    like: &Like,
    route: Route,
    src: &mut Handle,
    temp: &mut Handle,
) -> Result<std::result::Result<Staged, Refusal>> {
    let target = job.target;
    let temp_path = temp.path().to_string();
    let made = ask(&mut ctl.hook, Step::StatCreated, false, "stat", || {
        fs.fstat(temp)
    })?;
    if let Some(journal) = journal {
        journal.save_created(FileStat::of(&made))?;
    }
    // The target's owner, then its mode without set-id bits, through the handle (`create_from`'s
    // rule). A refused owner change is not fatal here: step 8 checks the result.
    if let Some((uid, gid)) = like.owner {
        let owned = ask(&mut ctl.hook, Step::SetOwner, true, "set the owner", || {
            fs.set_owner(temp, uid, gid)
        });
        match owned {
            Ok(()) => {}
            Err(e) if is_stopped(&e) || super::is_outcome_unknown(&e) => return Err(e),
            Err(e) => crate::debug!("set owner on {temp_path}: {e:#}"),
        }
    }
    ask(&mut ctl.hook, Step::SetMode, true, "set the mode", || {
        fs.set_handle_mode(temp, like.mode & 0o777)
    })?;
    // Steps 5 and 6, or §4.3's one pass.
    let scanned = match route {
        Route::ServerCopy => {
            copy(fs, src, temp, base.size, ctl)?;
            read_back(fs, temp, job, base, seen, ctl)?
        }
        _ => rewrite(fs, src, temp, job, base, seen, ctl)?,
    };
    let scanned = match scanned {
        Ok(scanned) => scanned,
        Err(why) => return Ok(Err(why)),
    };
    // Step 7: the bytes replaced are durable before the temp is patched (§4.3: before the
    // patched temp can replace anything).
    if let Some(undo) = undo {
        fire(&mut ctl.hook, Step::Undo)?;
        undo.add(&scanned.replaced)?;
    }
    // Step 8.
    if route == Route::ServerCopy {
        (ctl.progress)(Progress::Patching);
        for (i, run) in job.runs.iter().enumerate() {
            ask(&mut ctl.hook, Step::Patch(i), true, "write", || {
                fs.write_at(temp, run.offset, &run.bytes)
            })?;
        }
    }
    let mode = like.mode & 0o7777;
    ask(&mut ctl.hook, Step::FullMode, true, "set the mode", || {
        fs.set_handle_mode(temp, mode)
    })?;
    ask(&mut ctl.hook, Step::Fsync, true, "fsync", || fs.fsync(temp))?;
    let stat = ask(&mut ctl.hook, Step::StatTemp, false, "stat", || {
        fs.fstat(temp)
    })?;
    if let Some(attr) = stat.missing {
        bail!("the server does not report the {attr} of {temp_path}");
    }
    ensure!(
        stat.is_file && stat.size == base.size,
        "{temp_path} is not the {} bytes of {target} ({})",
        base.size,
        describe_stat(&stat)
    );
    if let Some((uid, gid)) = like.owner {
        ensure!(
            (stat.uid, stat.gid) == (Some(uid), Some(gid)),
            "{target} is owned by {} and a file this account creates there is owned by {}; the \
             replacement cannot keep the original owner",
            super::owner((Some(uid), Some(gid))),
            super::owner((stat.uid, stat.gid))
        );
    }
    ensure!(
        stat.mode & 0o7777 == mode,
        "{temp_path} did not take mode {mode:o} ({}); the replacement cannot keep the original \
         permissions",
        describe_stat(&stat)
    );
    Ok(Ok(Staged { stat, scanned }))
}

/// Steps 4 (after the create) to 10. `Ok(Err(_))`: refused, the temp still to be removed.
#[allow(clippy::too_many_arguments)]
fn stage_and_swap(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    journal: &mut Journal,
    snap: &Snapshot,
    seen: &BTreeMap<u64, Sha>,
    undo: &mut UndoFile,
    ctl: &mut Ctl<'_>,
    current: &Stat,
    route: Route,
    src: &mut Handle,
    temp: &mut Handle,
    at_rename: &mut bool,
) -> Result<std::result::Result<Saved, Refusal>> {
    let target = job.target;
    let base = &snap.baseline;
    let temp_path = temp.path().to_string();
    let (Some(uid), Some(gid)) = (current.uid, current.gid) else {
        bail!("the server does not report the owner of {target}");
    };
    let like = Like {
        owner: Some((uid, gid)),
        mode: current.mode & 0o7777,
    };
    let staged = stage(
        fs,
        job,
        Some(&mut *journal),
        base,
        seen,
        Some(&mut *undo),
        ctl,
        &like,
        route,
        src,
        temp,
    )?;
    let Staged {
        stat: staged,
        scanned,
    } = match staged {
        Ok(staged) => staged,
        Err(why) => return Ok(Err(why)),
    };
    let mode = like.mode;
    // Step 9: right before the replace.
    cancelled(ctl.cancel)?;
    let again = ask(&mut ctl.hook, Step::Recheck, false, "stat", || {
        fs.fstat(src)
    })?;
    if let Some(reason) = differs(&again, base) {
        return Ok(Err(Refusal::Changed {
            now: Some(again),
            reason,
        }));
    }
    if let Check::Changed(now, reason) = check_target(fs, target, base)? {
        return Ok(Err(Refusal::Changed { now, reason }));
    }
    let stat = FileStat::of(&staged);
    let post = PostEvidence::hex(&stat, scanned.new_pages.clone(), scanned.new_whole)?;
    journal.save_staged(stat, post)?;
    journal.save_renaming()?;
    // Step 10.
    *at_rename = true;
    ask(&mut ctl.hook, Step::Rename, true, "rename", || {
        fs.rename_over(&temp_path, target)
    })?;
    let mut notes = Vec::new();
    let after = ask(&mut ctl.hook, Step::StatAfter, false, "stat", || {
        fs.lstat_opt(target)
    });
    match after {
        Err(e) if is_stopped(&e) => return Err(e),
        Ok(Some(now)) if stat.matches(&now) => {}
        Ok(Some(now)) if now.is_file && !now.is_symlink && super::same_stat(&now, &staged) => {
            notes.push(format!(
                "saved, but {target} now has mode {:o} owner {}, not mode {mode:o} owner {} \
                 (changed right after the save?)",
                now.mode & 0o7777,
                super::owner((now.uid, now.gid)),
                super::owner((staged.uid, staged.gid))
            ));
        }
        Ok(other) => notes.push(format!(
            "saved, but the remote changed right after: {target} is {} — sopen wrote {} (another \
             writer?); the next save from this window will be refused",
            other.map_or("gone".to_string(), |s| describe_stat(&s)),
            describe_stat(&staged)
        )),
        Err(e) => notes.push(format!(
            "saved, but {target} could not be checked right after ({e:#})"
        )),
    }
    Ok(Ok(Saved {
        route,
        next: Fingerprint {
            size: staged.size,
            mtime: staged.mtime,
            sha256: scanned.new_whole,
        },
        checked: checked_line(base.size, scanned.new_pages.len()),
        seen: scanned.new_pages,
        undo: undo.path().to_path_buf(),
        notes,
    }))
}

/// Step 5: the server copies the source into the temp, in requests of about a second.
fn copy(
    fs: &mut dyn PatchFs,
    src: &mut Handle,
    temp: &mut Handle,
    size: u64,
    ctl: &mut Ctl<'_>,
) -> Result<()> {
    #[derive(Clone, Copy)]
    enum Halt {
        Stop(Step),
        Lost,
    }
    let Ctl {
        cancel,
        progress,
        pace,
        clock,
        hook,
    } = ctl;
    let cancel: &AtomicBool = cancel;
    let halt = Cell::new(None::<Halt>);
    let mut request = 0u32;
    let mut lose = fire(hook, Step::Copy(0))?;
    let stop = || cancel.load(Ordering::Relaxed) || halt.get().is_some();
    let mut heard = |event: CopyEvent| {
        let CopyEvent::Copied(done) = event else {
            return;
        };
        progress(Progress::Copying { done, total: size });
        if lose {
            halt.set(Some(Halt::Lost));
            return;
        }
        request += 1;
        if done < size {
            match fire(hook, Step::Copy(request)) {
                Ok(lost) => lose = lost,
                Err(_) => halt.set(Some(Halt::Stop(Step::Copy(request)))),
            }
        }
    };
    let mut copy = CopyCtl {
        pace: pace.clone(),
        clock: &mut **clock,
        cancel: &stop,
        progress: &mut heard,
    };
    let copied = fs.copy(src, temp, size, &mut copy);
    match halt.get() {
        Some(Halt::Stop(step)) => Err(anyhow!(Stopped(step))),
        Some(Halt::Lost) => Err(anyhow!(
            "copy into {}: no reply from the server",
            temp.path()
        )),
        None => copied,
    }
}

/// Step 6: what the check needs is read back from the temp — the target as it was at the copy.
/// At most 256 MiB the whole temp, which also gives the hash of the result; above, each edited
/// page and nothing else.
fn read_back(
    fs: &mut dyn PatchFs,
    temp: &mut Handle,
    job: &Job<'_>,
    base: &Fingerprint,
    seen: &BTreeMap<u64, Sha>,
    ctl: &mut Ctl<'_>,
) -> Result<std::result::Result<Scanned, Refusal>> {
    let size = base.size;
    let whole = size <= HASH_CHECK_MAX;
    let mut scan = Scan::new(job.runs, whole);
    if whole {
        let Ctl {
            cancel,
            progress,
            hook,
            ..
        } = ctl;
        let cancel: &AtomicBool = cancel;
        let read = ask(hook, Step::ReadBack, false, "read", || {
            fs.read_through(temp, size, None, &mut |at, chunk| {
                cancelled(cancel)?;
                scan.feed(at, chunk);
                progress(Progress::Checking {
                    done: at + chunk.len() as u64,
                    total: size,
                });
                Ok(None)
            })
        })?;
        if read != size {
            return Ok(Err(Refusal::Changed {
                now: None,
                reason: format!("size {size} → {read}"),
            }));
        }
    } else {
        for page in pages_of(job.runs) {
            cancelled(ctl.cancel)?;
            let bytes = ask(&mut ctl.hook, Step::ReadBack, false, "read", || {
                fs.read_at(temp, page, PAGE as usize)
            })?;
            if bytes.len() as u64 != PAGE.min(size - page) {
                return Ok(Err(Refusal::Page {
                    offset: page,
                    shown: seen.get(&page).copied().unwrap_or([0; 32]),
                    now: None,
                }));
            }
            scan.feed(page, &bytes);
        }
    }
    scan.finish(seen, base.sha256)
}

/// §4.3: steps 3–6 as one pass through this machine. The whole target is read fresh through the
/// handle step 3 opened, the patches are applied on the way into the temp, and what was read is
/// hashed: that hash must equal the baseline before the rename.
fn rewrite(
    fs: &mut dyn PatchFs,
    src: &mut Handle,
    temp: &mut Handle,
    job: &Job<'_>,
    base: &Fingerprint,
    seen: &BTreeMap<u64, Sha>,
    ctl: &mut Ctl<'_>,
) -> Result<std::result::Result<Scanned, Refusal>> {
    let size = base.size;
    let mut scan = Scan::new(job.runs, true);
    let Ctl {
        cancel,
        progress,
        hook,
        ..
    } = ctl;
    let cancel: &AtomicBool = cancel;
    let read = ask(hook, Step::Rewrite, true, "write the temp", || {
        fs.read_through(src, size, Some(temp), &mut |at, chunk| {
            cancelled(cancel)?;
            let out = scan.feed(at, chunk).into_owned();
            progress(Progress::Rewriting {
                done: at + chunk.len() as u64,
                total: size,
            });
            Ok(Some(out))
        })
    })?;
    if read != size {
        return Ok(Err(Refusal::Changed {
            now: None,
            reason: format!("size {size} → {read}"),
        }));
    }
    scan.finish(seen, base.sha256)
}

// ---------------------------------------------------------------------------------------------
// Save copy: a new sibling.

/// How a Save copy ended when it did not fail.
#[derive(Debug, Clone, PartialEq)]
pub enum CopyOutcome {
    /// The edited file is at `path`, a new name next to the original. `checked`: as
    /// [`Saved::checked`].
    Saved {
        path: String,
        checked: Option<String>,
        notes: Vec<String>,
    },
    /// Nothing was published. `notes` names anything left behind.
    Refused { why: Refusal, notes: Vec<String> },
}

/// `model.bin` → `model.edited.bin`; a name without an extension gets `.edited` appended.
pub fn edited_name(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}.edited{}", &name[..i], &name[i..]),
        _ => format!("{name}.edited"),
    }
}

/// Save copy (§6, §4.1's last column): the edited file as a new sibling `<stem>.edited<ext>`
/// (`-N` when that is taken), made as a Save makes its temp — §4.2 steps 1–8, or the streamed
/// rewrite where the server has no `copy-data`, under the same size rule and the same content
/// checks — and published by a hard link, never over anything (`write_new_sibling`'s rule).
///
/// The original is not written, so there is no undo file and no `save` record in the journal;
/// the copy takes the original's permission bits without set-id bits, and this account's
/// ownership. It needs no `posix-rename`, and it needs `hardlink@openssh.com`.
pub fn save_copy(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    snap: &Snapshot,
    ctl: &mut Ctl<'_>,
) -> Result<CopyOutcome> {
    let no = |why| {
        Ok(CopyOutcome::Refused {
            why,
            notes: Vec::new(),
        })
    };
    let seen = evidence(job, snap)?;
    if let Some(why) = under_edits(job, seen) {
        return no(why);
    }
    let target = job.target;
    let base = &snap.baseline;
    ensure!(
        fs.can_link(),
        "no copy saved: this server does not offer hardlink@openssh.com, which sopen needs to \
         publish a new file without the risk of replacing one"
    );
    let checked = ask(&mut ctl.hook, Step::CheckTarget, false, "stat", || {
        check_target(fs, target, base)
    })?;
    let current = match checked {
        Check::Same(now) => now,
        Check::Changed(now, reason) => return no(Refusal::Changed { now, reason }),
    };
    fire(&mut ctl.hook, Step::Room)?;
    let route = match staging_route(fs, target, base.size, job.rate) {
        Ok(route) => route,
        Err(why) => return no(why),
    };
    let mut src = ask(&mut ctl.hook, Step::OpenSource, false, "open", || {
        fs.open(target, OpenFor::Read)
    })?;
    let done = copy_from(fs, job, base, seen, ctl, &current, route, &mut src);
    if !done.as_ref().is_err_and(is_stopped) {
        fs.close(src);
    }
    done
}

#[allow(clippy::too_many_arguments)]
fn copy_from(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    base: &Fingerprint,
    seen: &BTreeMap<u64, Sha>,
    ctl: &mut Ctl<'_>,
    current: &Stat,
    route: Route,
    src: &mut Handle,
) -> Result<CopyOutcome> {
    let target = job.target;
    let opened = ask(&mut ctl.hook, Step::StatSource, false, "stat", || {
        fs.fstat(src)
    })?;
    if let Some(reason) = differs(&opened, base) {
        return Ok(CopyOutcome::Refused {
            why: Refusal::Changed {
                now: Some(opened),
                reason,
            },
            notes: Vec::new(),
        });
    }
    cancelled(ctl.cancel)?;
    let (dir, name) = split_parent(target);
    let wanted = edited_name(name);
    let temp_path = join(dir, &format!(".{wanted}.sopen-{}", random_hex()));
    let lose = fire(&mut ctl.hook, Step::Create)?;
    let made = match (fs.create_temp(&temp_path), lose) {
        (made, false) => made,
        (_, true) => Err(CreateError::new(
            Created::Unknown,
            unknown(format!("create {temp_path}: request sent, no reply")),
        )),
    };
    let mut temp = match made {
        Ok(temp) => temp,
        Err(e) if e.created == Created::No => return Err(e.error.context("no copy saved")),
        Err(e) => {
            let what = format!("no copy saved, but creating {temp_path} got no reply");
            return Err(unknown_at(e.error, &what, &temp_path));
        }
    };
    // The temp is this Save copy's from here: every way out removes it or names it.
    let like = Like {
        owner: None,
        mode: current.mode & 0o777,
    };
    let staged = stage(
        fs, job, None, base, seen, None, ctl, &like, route, src, &mut temp,
    );
    let mut at_link = false;
    let published = match staged {
        Ok(Ok(staged)) => {
            let pages = staged.scanned.new_pages.len();
            let linked = link_copy(fs, base, ctl, src, &temp_path, dir, &wanted, &mut at_link);
            linked.map(|name| name.map(|name| (name, pages)))
        }
        Ok(Err(why)) => Ok(Err(why)),
        Err(e) => Err(e),
    };
    let published = match published {
        // As a kill: nothing more is sent.
        Err(e) if is_stopped(&e) => return Err(e),
        other => other,
    };
    // Published or not, the temp goes: the copy has its own name now.
    let left = remove_own(fs, temp, &mut ctl.hook)?.err();
    match published {
        Ok(Ok((name, pages))) => Ok(CopyOutcome::Saved {
            path: join(dir, &name),
            checked: checked_line(base.size, pages),
            notes: left.into_iter().collect(),
        }),
        Ok(Err(why)) => Ok(CopyOutcome::Refused {
            why,
            notes: left.into_iter().collect(),
        }),
        Err(e) => {
            let e = match at_link && super::is_outcome_unknown(&e) {
                // The link may have happened.
                true => e.context(format!(
                    "the copy may be in place — check {}",
                    join(dir, &wanted)
                )),
                // Every other request was on the temp: no name was published.
                false => on_the_temp(e).context("no copy saved"),
            };
            Err(match left {
                Some(left) => e.context(left),
                None => e,
            })
        }
    }
}

/// A Save copy's last steps: the source's `fstat` once more, then the temp linked to the first
/// free name — a link fails where a name exists. `at_link` is set once the link may have been
/// sent.
#[allow(clippy::too_many_arguments)]
fn link_copy(
    fs: &mut dyn PatchFs,
    base: &Fingerprint,
    ctl: &mut Ctl<'_>,
    src: &mut Handle,
    temp_path: &str,
    dir: &str,
    wanted: &str,
    at_link: &mut bool,
) -> Result<std::result::Result<String, Refusal>> {
    let again = ask(&mut ctl.hook, Step::Recheck, false, "stat", || {
        fs.fstat(src)
    })?;
    if let Some(reason) = differs(&again, base) {
        return Ok(Err(Refusal::Changed {
            now: Some(again),
            reason,
        }));
    }
    let name = ask(&mut ctl.hook, Step::Link, true, "link", || {
        *at_link = true;
        match super::publish_linked(fs, temp_path, dir, wanted)? {
            super::Linked::As(name) => Ok(name),
            super::Linked::Unsupported(_) => bail!("hard links failed in {dir}"),
        }
    })?;
    Ok(Ok(name))
}

// ---------------------------------------------------------------------------------------------
// Write in place.

/// The words of the dialog that alone starts [`write_in_place`] (§4.4).
pub fn in_place_question(host: &str, target: &str, bytes: usize, undo: &Path) -> String {
    format!(
        "Write the {bytes} changed byte{} into {host}:{target} in place? This is not atomic. If \
         the connection drops while writing, the file may hold only some of the changes. \
         Programs reading it meanwhile see the changes as they land, and every hard link to it \
         changes too. The original bytes are kept at {}.",
        if bytes == 1 { "" } else { "s" },
        undo.display()
    )
}

/// `<file> may hold some of your changes — check it; the original bytes are at <undo file>`.
fn may_hold(job: &Job<'_>, undo: &UndoFile) -> String {
    format!(
        "{}:{} may hold some of your changes — check it; the original bytes are at {}",
        job.host,
        job.target,
        undo.path().display()
    )
}

/// Writes `job.runs` into the target itself (§4.4). **Not atomic**: only ever from the explicit
/// dialog of a refusal's row ([`Refusal::offers_other_ways`], [`in_place_question`]), never as a
/// fallback.
///
/// The target is checked as a Save checks it (its `lstat`, the handle's `fstat`, every edited
/// page read fresh, and at most 256 MiB its whole content), the bytes replaced are made durable
/// in `undo`, the runs go into the journal's `save` record (`writing`), and only then is the
/// first byte written. A write or fsync that fails or gets no reply once writing began is an
/// unknown outcome on the target itself: the record stays, and the error says where the original
/// bytes are. On [`SaveOutcome::Saved`] the caller ends the record with
/// [`Journal::save_landed`].
pub fn write_in_place(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    journal: &mut Journal,
    snap: &Snapshot,
    undo: &mut UndoFile,
    ctl: &mut Ctl<'_>,
) -> Result<SaveOutcome> {
    let seen = evidence(job, snap)?;
    if let Some(why) = under_edits(job, seen) {
        return refused(why);
    }
    let target = job.target;
    let base = &snap.baseline;
    let checked = ask(&mut ctl.hook, Step::CheckTarget, false, "stat", || {
        check_target(fs, target, base)
    })?;
    if let Check::Changed(now, reason) = checked {
        return refused(Refusal::Changed { now, reason });
    }
    let mut file = ask(&mut ctl.hook, Step::OpenSource, false, "open", || {
        fs.open(target, OpenFor::Patch)
    })?;
    let done = in_place(fs, job, journal, snap, seen, undo, ctl, &mut file);
    if !done.as_ref().is_err_and(is_stopped) {
        fs.close(file);
    }
    done
}

#[allow(clippy::too_many_arguments)]
fn in_place(
    fs: &mut dyn PatchFs,
    job: &Job<'_>,
    journal: &mut Journal,
    snap: &Snapshot,
    seen: &BTreeMap<u64, Sha>,
    undo: &mut UndoFile,
    ctl: &mut Ctl<'_>,
    file: &mut Handle,
) -> Result<SaveOutcome> {
    let target = job.target;
    let base = &snap.baseline;
    let size = base.size;
    let opened = ask(&mut ctl.hook, Step::StatSource, false, "stat", || {
        fs.fstat(file)
    })?;
    if let Some(reason) = differs(&opened, base) {
        return refused(Refusal::Changed {
            now: Some(opened),
            reason,
        });
    }
    // The content check, from the target itself.
    let whole = size <= HASH_CHECK_MAX;
    let mut scan = Scan::new(job.runs, whole);
    if whole {
        let cancel: &AtomicBool = ctl.cancel;
        let progress = &mut *ctl.progress;
        let read = ask(&mut ctl.hook, Step::ReadPages, false, "read", || {
            fs.read_through(file, size, None, &mut |at, chunk| {
                cancelled(cancel)?;
                scan.feed(at, chunk);
                progress(Progress::Checking {
                    done: at + chunk.len() as u64,
                    total: size,
                });
                Ok(None)
            })
        })?;
        if read != size {
            return refused(Refusal::Changed {
                now: None,
                reason: format!("size {size} → {read}"),
            });
        }
    } else {
        for page in pages_of(job.runs) {
            cancelled(ctl.cancel)?;
            let bytes = ask(&mut ctl.hook, Step::ReadPages, false, "read", || {
                fs.read_at(file, page, PAGE as usize)
            })?;
            if bytes.len() as u64 != PAGE.min(size - page) {
                return refused(Refusal::Page {
                    offset: page,
                    shown: seen.get(&page).copied().unwrap_or([0; 32]),
                    now: None,
                });
            }
            scan.feed(page, &bytes);
        }
    }
    let scanned = match scan.finish(seen, base.sha256)? {
        Ok(scanned) => scanned,
        Err(why) => return refused(why),
    };
    // The undo file, durable; then the intent with every run's old and new bytes.
    fire(&mut ctl.hook, Step::Undo)?;
    undo.add(&scanned.replaced)?;
    cancelled(ctl.cancel)?;
    let runs = job
        .runs
        .iter()
        .zip(&scanned.replaced)
        .map(|(new, old)| journal::Run {
            offset: new.offset,
            old: old.bytes.clone(),
            new: new.bytes.clone(),
        })
        .collect();
    let plan = Plan::InPlace {
        target: target.to_string(),
        undo: undo.path().to_path_buf(),
        runs,
    };
    if let Err(e) = journal.begin_save(snap, plan) {
        if journal.pending_save().is_some() {
            let _ = journal.clear_save();
        }
        return Err(e.context(format!(
            "{target} was not touched: the save could not be recorded in the journal"
        )));
    }
    // Nothing is written yet: a refusal or a failure here ends the record.
    let ready = (|| -> Result<Option<Refusal>> {
        let again = ask(&mut ctl.hook, Step::Recheck, false, "stat", || {
            fs.fstat(file)
        })?;
        if let Some(reason) = differs(&again, base) {
            return Ok(Some(Refusal::Changed {
                now: Some(again),
                reason,
            }));
        }
        Ok(match check_target(fs, target, base)? {
            Check::Same(_) => None,
            Check::Changed(now, reason) => Some(Refusal::Changed { now, reason }),
        })
    })();
    match ready {
        Ok(None) => {}
        Err(e) if is_stopped(&e) => return Err(e),
        Ok(Some(why)) => {
            journal.save_not_landed()?;
            return refused(why);
        }
        Err(e) => {
            let e = e.context(format!("{target} was not touched"));
            return Err(match journal.save_not_landed() {
                Ok(()) => e,
                Err(je) => e.context(format!("{je:#}")),
            });
        }
    }
    (ctl.progress)(Progress::Patching);
    let mut written = false;
    let wrote = (|| -> Result<()> {
        for (i, run) in job.runs.iter().enumerate() {
            ask(&mut ctl.hook, Step::Write(i), true, "write", || {
                fs.write_at(file, run.offset, &run.bytes)
            })
            .inspect_err(|e| {
                // A run longer than one request's payload may have landed in part.
                let split = run.bytes.len() > ONE_REQUEST;
                written |= i > 0 || split || super::is_outcome_unknown(e)
            })?;
            written = true;
        }
        ask(&mut ctl.hook, Step::Fsync, true, "fsync", || fs.fsync(file))
    })();
    match wrote {
        Ok(()) => {}
        Err(e) if is_stopped(&e) => return Err(e),
        Err(e) if !written => {
            // The first write was answered with a refusal: the target holds nothing of it.
            let e = e.context(format!("{target} was not touched"));
            return Err(match journal.save_not_landed() {
                Ok(()) => e,
                Err(je) => e.context(format!("{je:#}")),
            });
        }
        Err(e) => {
            journal.save_unknown();
            return Err(e.context(super::OutcomeUnknown(may_hold(job, undo))));
        }
    }
    let mut notes = Vec::new();
    let after = ask(&mut ctl.hook, Step::StatAfter, false, "stat", || {
        fs.lstat_opt(target)
    });
    let now = match after {
        Err(e) if is_stopped(&e) => return Err(e),
        Ok(Some(now)) if now.is_file && !now.is_symlink && now.size == size => Some(now),
        Ok(other) => {
            notes.push(format!(
                "saved, but the remote changed right after: {target} is {} (another writer?); \
                 the next save from this window will be refused",
                other.map_or("gone".to_string(), |s| describe_stat(&s))
            ));
            None
        }
        Err(e) => {
            notes.push(format!(
                "saved, but {target} could not be checked right after ({e:#}); the next save \
                 from this window will be refused"
            ));
            None
        }
    };
    Ok(SaveOutcome::Saved(Saved {
        route: Route::InPlace,
        next: Fingerprint {
            size,
            // Without the stat the old time stands: the next save's first check then refuses.
            mtime: now.map_or(base.mtime, |s| s.mtime),
            sha256: scanned.new_whole,
        },
        checked: checked_line(size, scanned.new_pages.len()),
        seen: scanned.new_pages,
        undo: undo.path().to_path_buf(),
        notes,
    }))
}

// ---------------------------------------------------------------------------------------------
// Fresh reads for the journal.

/// The journal's [`Inspect`](journal::Inspect) over a connection: what Restore's checks and the
/// reconciliation of a cut-off Save read (07 §3, §3.1) comes from the remote, through a handle
/// opened for it — never from a view's cache.
pub struct Fresh<'a> {
    fs: &'a mut dyn PatchFs,
    open: Option<Handle>,
}

impl<'a> Fresh<'a> {
    pub fn new(fs: &'a mut dyn PatchFs) -> Self {
        Fresh { fs, open: None }
    }
}

impl journal::Inspect for Fresh<'_> {
    fn lstat(&mut self, path: &str) -> Result<Option<Stat>> {
        self.fs.lstat_opt(path)
    }

    fn read_at(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        if self.open.as_ref().is_some_and(|h| h.path() != path) {
            if let Some(other) = self.open.take() {
                self.fs.close(other);
            }
        }
        let file = match &mut self.open {
            Some(file) => file,
            none => none.insert(self.fs.open(path, OpenFor::Read)?),
        };
        self.fs.read_at(file, offset, len)
    }
}

impl Drop for Fresh<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.open.take() {
            self.fs.close(file);
        }
    }
}

#[cfg(test)]
mod tests;
