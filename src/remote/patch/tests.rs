//! 07 §12's scenarios for the save layer (E1, E3–E9, E6b, E6c, E19), each against the real
//! `sftp-server` ([`SftpRemote::local_server`]) and against [`LocalFs`]; every step stopped and
//! every reply lost; and X2's pacing rule on a scripted clock.

use super::*;
use crate::journal::{reconcile, Kind, Outcome, Phase, Temp};
use crate::remote::sftp::Proxy;
use crate::remote::{is_cancelled, is_outcome_unknown, Entry, Like, LocalFs, SftpRemote, Uploaded};
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::sync::atomic::Ordering::Relaxed;
use std::time::UNIX_EPOCH;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;
/// The modification time every test file is given, and "put back" to.
const OLD: u64 = 1_700_000_000;

/// A connection with its payload bytes counted (07 §12: "at the client's read and write
/// payloads") and its requests that change the remote logged. It can report `copy-data` or
/// `posix-rename` absent, say how much room `statvfs` reports (`free`: `Some(None)` no
/// `statvfs`, the default; `None` the server's own answer), and have the temp's name taken
/// before the create.
struct Probe {
    inner: Box<dyn PatchFs + Send>,
    read: u64,
    written: u64,
    changes: Vec<&'static str>,
    no_copy: bool,
    no_replace: bool,
    free: Option<Option<u64>>,
    squat: bool,
    /// The server has no `fsync@openssh.com`.
    no_fsync: bool,
    /// Where the file's bytes are counted below this wrapper, at the backend's own boundary:
    /// the SFTP session's wire, or the local backend's reads and writes.
    wire: Option<Proxy>,
    local: Option<(u64, u64, u64)>,
}

/// File bytes as the backend itself moved them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Boundary {
    /// Read by the Save's requests (SFTP: `SSH_FXP_DATA` payload on the wire).
    read: u64,
    /// Written by them (SFTP: `SSH_FXP_WRITE` payload on the wire).
    written: u64,
}

impl Probe {
    fn new(inner: impl PatchFs + Send + 'static) -> Self {
        Probe {
            inner: Box::new(inner),
            read: 0,
            written: 0,
            changes: Vec::new(),
            no_copy: false,
            no_replace: false,
            // Not this machine's disk: only E7 says how much room there is.
            free: Some(None),
            squat: false,
            no_fsync: false,
            wire: None,
            local: None,
        }
    }

    /// The local disk, counted in the backend.
    fn local() -> Self {
        let mut probe = Probe::new(LocalFs::new());
        let base = LocalFs::moved();
        probe.local = Some((base.read, base.written, base.copied));
        probe
    }

    /// The real `sftp-server`, its session's bytes counted on the wire by a proxy.
    fn server() -> Option<Self> {
        let proxy = Proxy::default();
        let fs = SftpRemote::local_server_with(Some(proxy.clone()))?;
        let mut probe = Probe::new(fs);
        probe.wire = Some(proxy);
        Some(probe)
    }

    /// What crossed the backend's boundary since this probe was made; `None` where nothing
    /// counts there (a server started without the proxy).
    fn boundary(&self) -> Option<Boundary> {
        if let Some(proxy) = &self.wire {
            return Some(Boundary {
                read: proxy.data.load(Relaxed),
                written: proxy.written.load(Relaxed),
            });
        }
        let (read, written, _) = self.local?;
        let now = LocalFs::moved();
        Some(Boundary {
            read: now.read - read,
            written: now.written - written,
        })
    }

    /// The bytes the local backend's copy moved (the server's part, over SFTP).
    fn copied_locally(&self) -> Option<u64> {
        Some(LocalFs::moved().copied - self.local?.2)
    }

    /// The Save's own requests read `read` and wrote `written` file bytes: by this wrapper's
    /// count and, where it is counted, at the backend's boundary.
    #[track_caller]
    fn moved(&self, read: u64, written: u64, tag: &str) {
        assert_eq!((self.read, self.written), (read, written), "{tag}");
        if let Some(at) = self.boundary() {
            assert_eq!(at, Boundary { read, written }, "{tag}: at the backend");
        }
    }
}

impl RemoteFs for Probe {
    fn stat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.inner.stat_opt(p)
    }
    fn lstat_opt(&mut self, p: &str) -> Result<Option<Stat>> {
        self.inner.lstat_opt(p)
    }
    fn realpath(&mut self, p: &str) -> Result<String> {
        self.inner.realpath(p)
    }
    fn read_head(&mut self, p: &str, max: usize) -> Result<Vec<u8>> {
        self.inner.read_head(p, max)
    }
    fn list_dir(&mut self, p: &str) -> Result<Vec<String>> {
        self.inner.list_dir(p)
    }
    fn list_entries(&mut self, p: &str) -> Result<Vec<Entry>> {
        self.inner.list_entries(p)
    }
    fn free_space(&mut self, p: &str) -> Option<u64> {
        match self.free {
            Some(free) => free,
            None => self.inner.free_space(p),
        }
    }
    fn read_to(
        &mut self,
        p: &str,
        out: &mut dyn Write,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64> {
        let n = self.inner.read_to(p, out, progress)?;
        self.read += n;
        Ok(n)
    }
    fn create_from(
        &mut self,
        local: &Path,
        p: &str,
        like: Option<Like>,
    ) -> std::result::Result<Uploaded, CreateError> {
        self.changes.push("create_from");
        self.inner.create_from(local, p, like)
    }
    fn set_mode(&mut self, p: &str, mode: u32) -> Result<()> {
        self.changes.push("setstat");
        self.inner.set_mode(p, mode)
    }
    fn rename_over(&mut self, from: &str, to: &str) -> Result<()> {
        self.changes.push("rename");
        self.inner.rename_over(from, to)
    }
    fn link_new(&mut self, from: &str, to: &str) -> Result<()> {
        self.changes.push("link");
        self.inner.link_new(from, to)
    }
    fn can_replace(&self) -> bool {
        !self.no_replace && self.inner.can_replace()
    }
    fn remove(&mut self, p: &str) -> Result<()> {
        self.changes.push("remove");
        self.inner.remove(p)
    }
}

impl PatchFs for Probe {
    fn can_copy(&self) -> bool {
        !self.no_copy && self.inner.can_copy()
    }
    fn open(&mut self, p: &str, how: OpenFor) -> Result<Handle> {
        self.inner.open(p, how)
    }
    fn create_temp(&mut self, p: &str) -> std::result::Result<Handle, CreateError> {
        self.changes.push("create");
        if self.squat {
            std::fs::write(p, "someone else's").unwrap();
        }
        self.inner.create_temp(p)
    }
    fn fstat(&mut self, f: &mut Handle) -> Result<Stat> {
        self.inner.fstat(f)
    }
    fn read_at(&mut self, f: &mut Handle, offset: u64, len: usize) -> Result<Vec<u8>> {
        let got = self.inner.read_at(f, offset, len)?;
        self.read += got.len() as u64;
        Ok(got)
    }
    fn read_through(
        &mut self,
        src: &mut Handle,
        size: u64,
        dst: Option<&mut Handle>,
        each: &mut Each<'_>,
    ) -> Result<u64> {
        if dst.is_some() {
            self.changes.push("rewrite");
        }
        let (read, written) = (&mut self.read, &mut self.written);
        self.inner.read_through(src, size, dst, &mut |at, chunk| {
            *read += chunk.len() as u64;
            let out = each(at, chunk)?;
            *written += out.as_ref().map_or(0, |o| o.len() as u64);
            Ok(out)
        })
    }
    fn write_at(&mut self, f: &mut Handle, offset: u64, bytes: &[u8]) -> Result<()> {
        self.changes.push("write");
        self.written += bytes.len() as u64;
        self.inner.write_at(f, offset, bytes)
    }
    fn set_owner(&mut self, f: &mut Handle, uid: u32, gid: u32) -> Result<()> {
        self.changes.push("fsetstat");
        self.inner.set_owner(f, uid, gid)
    }
    fn set_handle_mode(&mut self, f: &mut Handle, mode: u32) -> Result<()> {
        self.changes.push("fsetstat");
        self.inner.set_handle_mode(f, mode)
    }
    fn can_fsync(&self) -> bool {
        !self.no_fsync && self.inner.can_fsync()
    }
    fn fsync(&mut self, f: &mut Handle) -> Result<()> {
        if self.no_fsync {
            return Err(anyhow!(NoFsync));
        }
        // Logged only when one was sent: a backend that cannot flush sends nothing.
        self.inner.fsync(f)?;
        self.changes.push("fsync");
        Ok(())
    }
    fn copy(
        &mut self,
        src: &mut Handle,
        dst: &mut Handle,
        size: u64,
        ctl: &mut CopyCtl<'_>,
    ) -> Result<()> {
        self.changes.push("copy");
        self.inner.copy(src, dst, size, ctl)
    }
    fn close(&mut self, f: Handle) {
        self.inner.close(f)
    }
}

/// The local disk, and the real `sftp-server` where it is installed (CI requires it).
fn sides() -> Vec<(&'static str, Probe)> {
    let mut out = vec![("LocalFs", Probe::local())];
    if let Some(fs) = Probe::server() {
        assert!(fs.can_copy(), "this sftp-server must offer copy-data");
        assert!(fs.can_fsync(), "this sftp-server must offer fsync");
        out.push(("local_server()", fs));
    }
    out
}

/// The same two without `copy-data`: a wrapper reporting it absent, and the real server told to
/// refuse it (`-P copy-data`), which then does not advertise it.
fn sides_without_copy() -> Vec<(&'static str, Probe)> {
    let mut local = Probe::local();
    local.no_copy = true;
    let mut out = vec![("LocalFs, copy-data reported absent", local)];
    if let Some(fs) = SftpRemote::local_server_args(&["-P", "copy-data"], None) {
        assert!(!PatchFs::can_copy(&fs));
        out.push(("local_server() -P copy-data", Probe::new(fs)));
    }
    out
}

fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

/// A write that leaves its own modification time (now).
fn plain_write(path: &Path, offset: u64, bytes: &[u8]) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.write_all_at(bytes, offset).unwrap();
}

/// Another writer: `bytes` at `offset`, with the modification time put back.
fn other_writer(path: &Path, offset: u64, bytes: &[u8]) {
    plain_write(path, offset, bytes);
    set_mtime(path, OLD);
}

fn sha_of(bytes: &[u8]) -> Sha {
    Sha256::digest(bytes).into()
}

/// A whole file's SHA-256. A big file goes to the system's `sha256sum` or `shasum` where there
/// is one (an unoptimized test build hashes about 30 MB a second), else it is hashed here.
fn file_sha(path: &Path) -> Sha {
    let big = std::fs::metadata(path).unwrap().len() > 16 * MIB;
    let tools: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (tool, args) in tools.into_iter().filter(|_| big) {
        let out = std::process::Command::new(tool)
            .args(args)
            .arg(path)
            .output();
        let Ok(out) = out else { continue };
        let text = String::from_utf8_lossy(&out.stdout);
        let digest = text.split_whitespace().next().unwrap_or_default();
        if out.status.success() && digest.len() == 64 {
            let mut sha = [0u8; 32];
            for (i, byte) in sha.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&digest[2 * i..2 * i + 2], 16).unwrap();
            }
            return sha;
        }
    }
    let mut file = std::fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).unwrap();
    hasher.finalize().into()
}

fn at(path: &Path, offset: u64, len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let f = std::fs::File::open(path).unwrap();
    let mut got = 0;
    while got < len {
        match f.read_at(&mut buf[got..], offset + got as u64).unwrap() {
            0 => break,
            n => got += n,
        }
    }
    buf.truncate(got);
    buf
}

/// The page at `page` as the window shows it now.
fn shown(path: &Path, page: u64) -> Sha {
    sha_of(&at(path, page, PAGE as usize))
}

fn temps(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".sopen-"))
        .collect();
    names.sort();
    names
}

fn none() -> Vec<String> {
    Vec::new()
}

fn patterned(len: u64) -> Vec<u8> {
    (0..len).map(|i| (i * 7 % 251) as u8).collect()
}

fn run(offset: u64, bytes: &[u8]) -> Patch {
    Patch {
        offset,
        bytes: bytes.to_vec(),
    }
}

enum Body {
    Bytes(Vec<u8>),
    /// A file of this size with nothing written (a hole).
    Sparse(u64),
    /// A sparse file of this size with these bytes written into it.
    Seeded(u64, Vec<(u64, Vec<u8>)>),
}

/// One editing session on one remote file: the file, the window's evidence, its journal and its
/// undo file.
struct Rig {
    dir: tempfile::TempDir,
    cache: tempfile::TempDir,
    path: PathBuf,
    target: String,
    runs: Vec<Patch>,
    snap: Snapshot,
    journal: Option<Journal>,
    undo: UndoFile,
}

impl Rig {
    /// The file `body`, opened, with `runs` typed over it: the evidence is each edited page as
    /// shown at that moment, the baseline the file's stat (and whole hash at most 256 MiB).
    fn new(fs: &mut Probe, body: Body, runs: Vec<Patch>) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("model.bin");
        match body {
            Body::Bytes(bytes) => std::fs::write(&path, bytes).unwrap(),
            Body::Sparse(size) => std::fs::File::create(&path).unwrap().set_len(size).unwrap(),
            Body::Seeded(size, seeds) => {
                std::fs::File::create(&path).unwrap().set_len(size).unwrap();
                for (offset, bytes) in seeds {
                    plain_write(&path, offset, &bytes);
                }
            }
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        set_mtime(&path, OLD);
        let target = path.to_str().unwrap().to_string();
        let stat = fs.lstat_opt(&target).unwrap().unwrap();
        let sha256 = (stat.size <= HASH_CHECK_MAX).then(|| file_sha(&path));
        let baseline = Fingerprint {
            size: stat.size,
            mtime: stat.mtime,
            sha256,
        };
        let seen = Seen::pages(pages_of(&runs).into_iter().map(|p| (p, shown(&path, p))));
        let edits: String = runs
            .iter()
            .map(|r| {
                let old = at(&path, r.offset, r.bytes.len());
                format!("0x{:X}: {} was {}\n", r.offset, hex(&r.bytes), hex(&old))
            })
            .collect();
        let snap = Snapshot {
            baseline,
            seen,
            edits,
            pastes: Vec::new(),
        };
        let journal =
            Journal::create(journal::disk(), cache.path(), "box", &target, Kind::Hex).unwrap();
        let undo = UndoFile::new(cache.path(), "box", &target, baseline, SystemTime::now());
        Rig {
            dir,
            cache,
            path,
            target,
            runs,
            snap,
            journal: Some(journal),
            undo,
        }
    }

    fn folder(&self) -> &Path {
        self.path.parent().unwrap()
    }

    fn journal(&self) -> &Journal {
        self.journal.as_ref().unwrap()
    }

    /// The journal's `save` record: its phase by name, or "none".
    fn phase(&self) -> &'static str {
        match self.journal().pending_save().map(|s| &s.phase) {
            None => "none",
            Some(Phase::Naming { .. }) => "naming",
            Some(Phase::Created { .. }) => "created",
            Some(Phase::Staged { .. }) => "staged",
            Some(Phase::Renaming { .. }) => "renaming",
            Some(Phase::Writing { .. }) => "writing",
        }
    }

    /// What a reopen makes of the `save` record, reading the remote fresh: the outcome, what may
    /// be done with the temp, and the row's line.
    fn reconciled(&self, fs: &mut Probe) -> (Outcome, Temp, String) {
        let intent = self.journal().pending_save().expect("a save record");
        let r = reconcile(intent, "box", &mut Fresh::new(fs));
        (r.outcome, r.temp, r.line)
    }

    /// The undo file's bytes written back over the file.
    fn write_undo_back(&self) {
        let text = std::fs::read_to_string(self.undo.path()).unwrap();
        let undo = UndoFile::parse(&text).unwrap();
        for r in &undo.runs {
            plain_write(&self.path, r.offset, &r.bytes);
        }
    }

    /// Bytes on the local disk under the cache: the journal and the undo file.
    fn local_bytes(&self) -> u64 {
        fn walk(dir: &Path) -> u64 {
            std::fs::read_dir(dir)
                .unwrap()
                .map(|e| {
                    let e = e.unwrap();
                    match e.file_type().unwrap().is_dir() {
                        true => walk(&e.path()),
                        false => e.metadata().unwrap().len(),
                    }
                })
                .sum()
        }
        walk(self.cache.path())
    }

    /// The Save landed and nothing is unsaved: the journal goes.
    fn landed(&mut self) {
        let dir = self.journal().dir().to_path_buf();
        let journal = self.journal.take().unwrap();
        assert!(journal.save_landed(None).unwrap().is_none());
        assert!(!dir.exists());
    }
}

#[derive(Default)]
struct Opts<'a> {
    hook: Hook<'a>,
    progress: Option<&'a mut dyn FnMut(Progress)>,
    cancel: Option<&'a AtomicBool>,
    pace: Option<Pace>,
    pass: Option<&'a Baseline>,
    in_place: bool,
}

/// The rule of [`Pace::timed`] at a thousandth of its sizes.
fn small_pace() -> Pace {
    Pace::sized(64 * KIB, 16 * KIB, MIB, 2 * MIB)
}

fn go(fs: &mut Probe, rig: &mut Rig, o: Opts<'_>) -> Result<SaveOutcome> {
    let job = Job {
        host: "box",
        target: &rig.target,
        runs: &rig.runs,
        pass: o.pass,
        rate: Some(3.6e6),
    };
    let never = AtomicBool::new(false);
    let mut quiet = |_: Progress| {};
    let mut clock = stopwatch();
    let progress: &mut dyn FnMut(Progress) = match o.progress {
        Some(p) => p,
        None => &mut quiet,
    };
    let mut ctl = Ctl::new(o.cancel.unwrap_or(&never), progress, &mut clock);
    ctl.pace = o.pace.unwrap_or_else(small_pace);
    // A reborrow: the hook's own lifetime is longer than this call's.
    ctl.hook = match o.hook {
        Some(hook) => Some(hook),
        None => None,
    };
    let journal = rig.journal.as_mut().unwrap();
    match o.in_place {
        true => write_in_place(fs, &job, journal, &rig.snap, &mut rig.undo, &mut ctl),
        false => save(fs, &job, journal, &rig.snap, &mut rig.undo, &mut ctl),
    }
}

fn saved(outcome: Result<SaveOutcome>) -> Saved {
    match outcome {
        Ok(SaveOutcome::Saved(saved)) => saved,
        other => panic!("not saved: {other:?}"),
    }
}

fn refusal(outcome: Result<SaveOutcome>) -> Refusal {
    match outcome {
        Ok(SaveOutcome::Refused { why, notes }) => {
            assert_eq!(notes, none(), "nothing is left behind");
            why
        }
        other => panic!("not refused: {other:?}"),
    }
}

/// A 300 KiB file with two runs in two pages: small enough for the whole-content check.
fn small(fs: &mut Probe) -> (Rig, Vec<u8>) {
    let body = patterned(300 * KIB);
    let runs = vec![run(0x1000 + 5, b"\xAA\xBB\xCC\xDD"), run(0x40000, b"\x01")];
    (Rig::new(fs, Body::Bytes(body.clone()), runs), body)
}

fn patched(mut body: Vec<u8>, runs: &[Patch]) -> Vec<u8> {
    for r in runs {
        body[r.offset as usize..r.end() as usize].copy_from_slice(&r.bytes);
    }
    body
}

fn with_byte(mut body: Vec<u8>, offset: u64, byte: u8) -> Vec<u8> {
    body[offset as usize] = byte;
    body
}

// ---------------------------------------------------------------------------------------------
// X2: the size of the copy requests.

/// 07 §4.2 step 5, at its real sizes, with the durations given (no clock): 64 MiB first, then
/// about 1 s at the last rate within 16 MiB–1 GiB; a flush after 2 GiB or after 10 s of copying,
/// whichever first; a flush's time counted in the rate.
#[test]
fn x2_requests_are_sized_to_a_second_at_the_last_rate() {
    let secs = Duration::from_secs_f64;
    let mut pace = Pace::timed();
    assert_eq!(pace.next(u64::MAX), 64 * MIB, "the first request");
    assert_eq!(pace.next(5 * MIB), 5 * MIB, "never past the end");
    // 64 MiB in 0.5 s is 128 MiB/s: the next request is 128 MiB.
    pace.copied(64 * MIB, secs(0.5));
    assert_eq!(pace.next(u64::MAX), 128 * MIB);
    // X2's laptop: 64 MiB took 18–70 ms. Up to 62.5 ms the second request is the 1 GiB cap; at
    // 70 ms (914 MiB/s) it is 914 MiB.
    for (ms, second) in [(18, 1024 * MIB), (62, 1024 * MIB), (70, 958_698_057)] {
        let mut fast = Pace::timed();
        fast.copied(64 * MIB, Duration::from_millis(ms));
        assert_eq!(fast.next(u64::MAX), second, "{ms} ms");
    }
    // Its worst: a 1 GiB request in 1.73 s cuts the next one to about 592 MiB.
    pace.copied(1024 * MIB, secs(1.73));
    let cut = pace.next(u64::MAX);
    assert!((580 * MIB..600 * MIB).contains(&cut), "{cut}");
    // A slow disk: 2 MiB/s is held at the 16 MiB floor — 8 s a request, far from 30 s.
    pace.copied(cut, secs(cut as f64 / (2 * MIB) as f64));
    assert_eq!(pace.next(u64::MAX), 16 * MIB);
    // A request answered within the clock's grain: as large as the rule allows.
    pace.copied(16 * MIB, Duration::ZERO);
    assert_eq!(pace.next(u64::MAX), 1024 * MIB);

    // The flush: after 2 GiB copied.
    let mut pace = Pace::timed();
    pace.copied(64 * MIB, secs(0.1));
    assert!(!pace.sync_due());
    pace.copied(1024 * MIB, secs(0.5));
    assert!(!pace.sync_due(), "1088 MiB since the last flush");
    pace.copied(1024 * MIB, secs(0.5));
    assert!(pace.sync_due(), "2112 MiB since the last flush");
    // A server answering at memory speed that pays at the flush: 2112 MiB over 1.1 s of
    // requests and a 20 s flush is 100 MiB/s, and that sizes the next request.
    pace.synced(secs(20.0));
    let next = pace.next(u64::MAX);
    assert!((99 * MIB..101 * MIB).contains(&next), "{next}");
    assert!(!pace.sync_due(), "counted from the flush again");
    // …or after 10 s of copying, whichever comes first.
    pace.copied(next, secs(6.0));
    assert!(!pace.sync_due());
    pace.copied(16 * MIB, secs(4.0));
    assert!(pace.sync_due(), "10 s of copying, 116 MiB");

    // A file copy's steps (04 §5.1) never change and are never flushed on the way.
    let mut steps = Pace::steps(8 * MIB);
    for took in [Duration::ZERO, secs(0.001), secs(40.0)] {
        steps.copied(8 * MIB, took);
        assert_eq!(steps.next(u64::MAX), 8 * MIB);
        assert!(!steps.sync_due());
    }
    for _ in 0..400 {
        steps.copied(8 * MIB, secs(1.0));
    }
    assert!(!steps.sync_due(), "3.2 GiB and 400 s later");
}

/// The same rule driving the real loops — `copy-data` on the `sftp-server`, the plain copy on
/// the local disk — with a scripted clock: the request sizes follow the durations the clock
/// gives, and the temp is flushed after 10 s of copying and after the byte bound.
#[test]
fn x2_the_copy_loops_follow_a_scripted_clock() {
    // What each request and each flush takes, in the order they happen.
    const TOOK: [f64; 10] = [1.0, 0.25, 4.0, 5.0, 3.75, 0.0, 0.5, 0.5, 0.0, 0.1];
    let k = |n: u64| n * KIB;
    for (side, mut fs) in sides() {
        let dir = tempfile::tempdir().unwrap();
        let body = patterned(3 * MIB);
        let src = dir.path().join("src");
        let dst = dir.path().join(".src.sopen-test");
        std::fs::write(&src, &body).unwrap();
        let mut from = fs.open(src.to_str().unwrap(), OpenFor::Read).unwrap();
        let mut to = fs.create_temp(dst.to_str().unwrap()).unwrap();
        // The clock is read before and after every request and every flush.
        let (mut reads, mut now) = (0usize, Duration::ZERO);
        let mut clock = move || {
            if reads % 2 == 1 {
                now += Duration::from_secs_f64(TOOK[reads / 2]);
            }
            reads += 1;
            now
        };
        let mut events = Vec::new();
        let mut ctl = CopyCtl {
            pace: small_pace(),
            clock: &mut clock,
            cancel: &|| false,
            progress: &mut |e| events.push(e),
        };
        fs.copy(&mut from, &mut to, 3 * MIB, &mut ctl).unwrap();
        fs.close(from);
        fs.close(to);
        let (mut sizes, mut flushes, mut done) = (Vec::new(), Vec::new(), 0);
        for e in events {
            match e {
                CopyEvent::Copied(d) => {
                    sizes.push(d - done);
                    done = d;
                }
                CopyEvent::Synced(d) => flushes.push(d),
            }
        }
        assert_eq!(
            sizes,
            [
                k(64),   // the first request; 1 s: 64 KiB/s
                k(64),   // 0.25 s: 256 KiB/s
                k(256),  // 4 s: 64 KiB/s
                k(64),   // 5 s: under the floor — and 10.25 s of copying: a flush, 3.75 s
                k(32),   // 448 KiB over 14 s with the flush; answered at once: the cap
                k(1024), // 0.5 s
                k(1024), // 0.5 s — 2080 KiB since the flush: a flush
                k(544),  // the rest
            ],
            "{side}"
        );
        assert_eq!(flushes, [k(448), k(2528)], "{side}");
        assert_eq!(std::fs::read(&dst).unwrap(), body, "{side}");
    }
}

// ---------------------------------------------------------------------------------------------
// E1, E5, E19 (first row): files above 256 MiB, by the server copy.

const BIG: u64 = HASH_CHECK_MAX + MIB;

/// Distinct bytes in the big file apart from the 4 edited: at its start, across the start of the
/// edited page (so that page is not all zeros either), right after it, in the middle, and in the
/// file's last bytes.
fn seeds() -> Vec<(u64, Vec<u8>)> {
    let edited = 200 * MIB;
    [
        0,
        MIB + 17,
        edited - 9,
        edited + PAGE,
        130 * MIB + 5,
        BIG - 4096,
        BIG - 100,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, offset)| {
        let bytes = (0..100u8).map(|b| b.wrapping_mul(7).wrapping_add(i as u8 * 31 + 1));
        (offset, bytes.collect())
    })
    .collect()
}

/// A sparse file one MiB over the content-check limit holding [`seeds`], with 4 bytes typed over
/// one page.
fn big(fs: &mut Probe) -> Rig {
    let offset = 200 * MIB + 0xC35;
    let patch = vec![run(offset, b"\xDE\xAD\xBE\xEF")];
    let rig = Rig::new(fs, Body::Seeded(BIG, seeds()), patch);
    assert_eq!(
        rig.snap.baseline.sha256, None,
        "no whole hash above 256 MiB"
    );
    rig
}

/// The seeded regions are in `path` as they were written.
#[track_caller]
fn seeds_hold(path: &Path, tag: &str) {
    for (offset, bytes) in seeds() {
        assert_eq!(
            at(path, offset, bytes.len()),
            bytes,
            "{tag}: at 0x{offset:x}"
        );
    }
}

/// A file's identity and attributes by its path on this machine: device and inode, size,
/// modification time, mode.
fn ident(path: &Path) -> ((u64, u64), u64, SystemTime, u32) {
    let m = std::fs::symlink_metadata(path).unwrap();
    (
        (m.dev(), m.ino()),
        m.len(),
        m.modified().unwrap(),
        m.mode() & 0o7777,
    )
}

/// E1: a 4-byte patch to a file above 256 MiB moves one page read back and the 4 bytes — the
/// copy is the server's. The undo file written back gives the original SHA-256.
#[test]
fn e1_a_small_patch_to_a_huge_file_moves_one_page_and_the_patch() {
    for (side, mut fs) in sides() {
        let mut rig = big(&mut fs);
        let original = file_sha(&rig.path);
        let offset = rig.runs[0].offset;
        let page = offset / PAGE * PAGE;
        let mut lines = Vec::new();
        let saved = saved(go(
            &mut fs,
            &mut rig,
            Opts {
                progress: Some(&mut |p| lines.push(p.line("box"))),
                pace: Some(Pace::timed()),
                ..Opts::default()
            },
        ));
        assert_eq!(saved.route, Route::ServerCopy, "{side}");
        // Counted where the backend itself reads and writes — on the SFTP session's wire, in
        // the local backend — so a copy that moved the file through this machine would show.
        assert!(fs.boundary().is_some(), "{side}: counted at the backend");
        fs.moved(
            PAGE,
            4,
            &format!("{side}: the page check and the patch, nothing else"),
        );
        if let Some(copied) = fs.copied_locally() {
            assert_eq!(copied, BIG, "{side}: the copy is the backend's own");
        }
        // The copy kept every byte away from the edit.
        seeds_hold(&rig.path, side);
        assert_eq!(at(&rig.path, offset, 4), b"\xDE\xAD\xBE\xEF", "{side}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        let local = rig.local_bytes();
        assert!(local <= 64 * KIB, "{side}: {local} bytes on the local disk");
        assert_eq!(
            saved.checked.as_deref(),
            Some(
                "checked size, modification time and the 4 KiB under your edits, not the whole \
                 content"
            ),
            "{side}"
        );
        assert_eq!(saved.next.sha256, None);
        assert_eq!(saved.seen[&page], shown(&rig.path, page), "{side}");
        let line = saved.line("box", &rig.target);
        assert!(
            line.starts_with("saved box:/") && line.contains(" · backup "),
            "{line}"
        );
        assert!(line.ends_with(".undo.txt"), "{line}");
        let first = &lines[0];
        assert!(
            first.starts_with("saving — copying on box ") && first.ends_with(" MB)"),
            "{first}"
        );
        // The mode and the next baseline are the staged temp's.
        let now = fs.lstat_opt(&rig.target).unwrap().unwrap();
        assert_eq!(now.mode, 0o640, "{side}");
        assert_eq!((saved.next.size, saved.next.mtime), (now.size, now.mtime));
        // The journal goes once nothing is unsaved.
        assert_eq!(rig.phase(), "renaming");
        rig.landed();
        // Writing the undo file back gives the original.
        assert_ne!(file_sha(&rig.path), original);
        rig.write_undo_back();
        assert_eq!(file_sha(&rig.path), original, "{side}");
    }
}

/// E5: same size, mtime put back, the edited bytes changed (above 256 MiB): refused at the page
/// check, the temp removed, the original untouched.
#[test]
fn e5_a_change_under_the_edits_is_refused_at_the_page_check() {
    for (side, mut fs) in sides() {
        let mut rig = big(&mut fs);
        let offset = rig.runs[0].offset;
        // The window showed the page; then another writer changed a byte of it.
        other_writer(&rig.path, offset + 100, b"\x99");
        let before = shown(&rig.path, offset / PAGE * PAGE);
        // The whole file and its identity, taken outside the Save's own traffic.
        let (whole, same) = (file_sha(&rig.path), ident(&rig.path));
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        assert_eq!(file_sha(&rig.path), whole, "{side}: no byte of it changed");
        assert_eq!(ident(&rig.path), same, "{side}: the same file");
        fs.moved(
            PAGE,
            0,
            &format!("{side}: one page read back, nothing written"),
        );
        let Refusal::Page { offset: page, .. } = why else {
            panic!("{side}: {why:?}");
        };
        assert_eq!(page, offset / PAGE * PAGE);
        let text = why.to_string();
        assert!(
            text.contains(": shown ") && text.contains("… now "),
            "{text}"
        );
        assert!(
            fs.changes.contains(&"copy"),
            "{side}: refused after the copy"
        );
        assert!(!fs.changes.contains(&"write"), "{side}: nothing patched");
        assert!(!fs.changes.contains(&"rename"), "{side}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        assert_eq!(shown(&rig.path, page), before, "{side}: untouched");
        assert_eq!(at(&rig.path, offset, 4), [0u8; 4], "{side}");
        assert_eq!(rig.phase(), "none", "{side}: the record went with the temp");
        assert!(rig.undo.is_empty(), "{side}: nothing was replaced");
    }
}

/// E19, first row: a writer changes a region after the copy read it and puts the mtime back
/// (above 256 MiB). The Save lands, and the writer's change is not in the result. This pins the
/// documented loss.
#[test]
fn e19_a_write_after_the_copy_read_it_is_lost_as_documented() {
    for (side, mut fs) in sides() {
        let mut rig = big(&mut fs);
        let (path, offset) = (rig.path.clone(), rig.runs[0].offset);
        let size = rig.snap.baseline.size;
        let whole = Progress::Copying {
            done: size,
            total: size,
        };
        let mut wrote = false;
        let saved = saved(go(
            &mut fs,
            &mut rig,
            Opts {
                progress: Some(&mut |p| {
                    // The copy has read everything: a writer changes a byte far from the edits.
                    if p == whole && !wrote {
                        other_writer(&path, 10 * MIB, b"\x77");
                        wrote = true;
                    }
                }),
                pace: Some(Pace::timed()),
                ..Opts::default()
            },
        ));
        assert!(wrote, "{side}");
        assert_eq!(saved.notes, none(), "{side}: unseen");
        assert_eq!(at(&rig.path, offset, 4), b"\xDE\xAD\xBE\xEF", "{side}");
        assert_eq!(
            at(&rig.path, 10 * MIB, 1),
            [0],
            "{side}: the writer's byte is gone"
        );
    }
}

/// E19, second row: a writer changes the target between the last check (step 9) and the rename.
/// The rename replaces it, unseen — at any size.
#[test]
fn e19_a_write_between_the_last_check_and_the_rename_is_lost_as_documented() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        let path = rig.path.clone();
        let saved = saved(go(
            &mut fs,
            &mut rig,
            Opts {
                hook: Some(&mut |step| {
                    if step == Step::Rename {
                        // A normal write: its modification time is now.
                        plain_write(&path, 9, b"\x77");
                    }
                    At::Go
                }),
                ..Opts::default()
            },
        ));
        assert_eq!(saved.notes, none(), "{side}");
        let want = patched(body, &rig.runs);
        assert_eq!(std::fs::read(&rig.path).unwrap(), want, "{side}");
    }
}

// ---------------------------------------------------------------------------------------------
// E3, E4, E6: what is refused.

/// The server copy of a small file: the whole-content check, the patch, the rename.
#[test]
fn a_small_file_is_saved_by_the_server_copy_with_its_whole_hash() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        let saved = saved(go(&mut fs, &mut rig, Opts::default()));
        let want = patched(body.clone(), &rig.runs);
        assert_eq!(saved.route, Route::ServerCopy, "{side}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), want, "{side}");
        assert_eq!(saved.next.sha256, Some(sha_of(&want)), "{side}");
        assert_eq!(saved.checked, None, "{side}: the whole content was checked");
        fs.moved(
            300 * KIB,
            5,
            &format!("{side}: the temp read back once, and the patch"),
        );
        for (&page, sha) in &saved.seen {
            assert_eq!(*sha, shown(&rig.path, page), "{side}");
        }
        assert_eq!(temps(rig.folder()), none());
        // The undo file: owner-only, the bytes replaced, and it gives the original back.
        let meta = std::fs::metadata(rig.undo.path()).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let text = std::fs::read_to_string(rig.undo.path()).unwrap();
        let source = format!("# source: box:{}\n", rig.target);
        assert!(text.contains(&source), "{text}");
        assert!(text.contains("# as opened: 307200 bytes, modified 2023-11-14T22:13:20Z\n"));
        let old = format!("0x1005: {}\n", hex(&body[0x1005..0x1009]));
        assert!(text.contains(&old), "{text}");
        rig.write_undo_back();
        assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{side}");
    }
}

/// E3: the remote's mtime changed since the open. Refused at the first `lstat`; nothing is
/// created on the remote, and the journal holds no save record.
#[test]
fn e3_a_file_changed_since_open_is_refused_having_created_nothing() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        set_mtime(&rig.path, OLD + 60);
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        let Refusal::Changed { reason, .. } = &why else {
            panic!("{side}: {why:?}");
        };
        assert_eq!(
            reason, "modified 2023-11-14T22:13:20Z → 2023-11-14T22:14:20Z",
            "{side}"
        );
        assert_eq!(
            fs.changes,
            Vec::<&str>::new(),
            "{side}: no request that writes"
        );
        fs.moved(0, 0, side);
        assert_eq!(std::fs::read(&rig.path).unwrap(), body);
        assert_eq!(rig.phase(), "none");
        assert!(rig.journal().dir().exists(), "the journal keeps the edits");
        assert!(!rig.undo.path().exists());

        // Deleted, or no longer a regular file: the same.
        std::fs::remove_file(&rig.path).unwrap();
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        assert!(why.to_string().contains("it was deleted"), "{why}");
        std::os::unix::fs::symlink("/dev/null", &rig.path).unwrap();
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        assert!(
            why.to_string().contains("it is now a symbolic link"),
            "{why}"
        );
        assert_eq!(fs.changes, Vec::<&str>::new(), "{side}");
        assert_eq!(temps(rig.dir.path()), none());
    }
}

/// E4: a writer writes one byte of the source after the third copy request. Refused at the check
/// before the replace; the temp is removed; the target is the file as the writer left it.
#[test]
fn e4_a_writer_during_the_copy_is_refused_before_the_replace() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        let path = rig.path.clone();
        let mut requests = 0;
        let why = refusal(go(
            &mut fs,
            &mut rig,
            Opts {
                pace: Some(Pace::steps(64 * KIB)),
                progress: Some(&mut |p| {
                    if let Progress::Copying { done, .. } = p {
                        requests += 1;
                        if requests == 3 {
                            // In a region the copy has read already; its mtime is now.
                            assert_eq!(done, 192 * KIB);
                            plain_write(&path, 9, b"\x77");
                        }
                    }
                }),
                ..Opts::default()
            },
        ));
        assert_eq!(requests, 5, "{side}: 300 KiB in 64 KiB requests");
        let Refusal::Changed { reason, .. } = &why else {
            panic!("{side}: {why:?}");
        };
        assert!(
            reason.starts_with("modified 2023-11-14T22:13:20Z → "),
            "{reason}"
        );
        assert!(
            fs.changes.contains(&"write"),
            "{side}: refused after the patch"
        );
        assert!(!fs.changes.contains(&"rename"), "{side}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        let theirs = with_byte(body, 9, 0x77);
        assert_eq!(std::fs::read(&rig.path).unwrap(), theirs, "{side}");
        assert_eq!(rig.phase(), "none");
    }
}

/// E6: same size, mtime put back, a byte changed far from the edits (at most 256 MiB): refused by
/// the whole-content check, the original untouched.
#[test]
fn e6_a_change_far_from_the_edits_is_refused_by_the_whole_content_check() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        other_writer(&rig.path, 250 * KIB, b"\x77");
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        assert_eq!(
            why,
            Refusal::Changed {
                now: None,
                reason: "its content changed (same size and modification time)".to_string()
            },
            "{side}"
        );
        assert!(!fs.changes.contains(&"write"), "{side}");
        assert!(!fs.changes.contains(&"rename"), "{side}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        let theirs = with_byte(body, 250 * KIB, 0x77);
        assert_eq!(std::fs::read(&rig.path).unwrap(), theirs, "{side}");
        assert_eq!(rig.phase(), "none");
    }
}

/// E6b: the same on the route without `copy-data`, with the changed block already read by the
/// view before the change: still refused. The streamed rewrite takes no cache — it read the
/// whole target from the remote.
#[test]
fn e6b_the_streamed_rewrite_reads_the_target_fresh() {
    for (side, mut fs) in sides_without_copy() {
        let (mut rig, body) = small(&mut fs);
        // The view's cache holds the block as it was: these bytes are never offered to the Save.
        let cached = at(&rig.path, 248 * KIB, 8 * KIB as usize);
        other_writer(&rig.path, 250 * KIB, b"\x77");
        assert_ne!(at(&rig.path, 248 * KIB, 8 * KIB as usize), cached);
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        assert!(
            matches!(&why, Refusal::Changed { reason, .. } if reason.starts_with("its content")),
            "{side}: {why:?}"
        );
        assert_eq!(
            fs.read,
            300 * KIB,
            "{side}: the whole target, read by the Save"
        );
        assert!(fs.changes.contains(&"rewrite"), "{side}");
        assert!(!fs.changes.contains(&"copy") && !fs.changes.contains(&"rename"));
        assert_eq!(temps(rig.folder()), none(), "{side}");
        let theirs = with_byte(body, 250 * KIB, 0x77);
        assert_eq!(std::fs::read(&rig.path).unwrap(), theirs, "{side}");
    }
}

/// E6c: a change before the baseline pass, to the bytes being edited. The edited page's `seen`
/// does not match the pass's page hash, so the baseline is not accepted and Save is refused —
/// with the page edited before the pass, and edited after it.
#[test]
fn e6c_a_baseline_that_disagrees_with_the_page_shown_is_not_accepted() {
    for (side, mut fs) in sides() {
        for edited_first in [true, false] {
            let (mut rig, body) = small(&mut fs);
            // 1. Page A is shown.
            let page = 0x1000;
            let as_shown = shown(&rig.path, page);
            let far = shown(&rig.path, 0x40000);
            // 3, in the first order: the owner edits it now, and `seen` is the page as shown.
            let seen_before = edited_first.then(|| rig.snap.seen.clone());
            // 2. Another writer changes a byte of it, with size and mtime put back.
            other_writer(&rig.path, page + 200, b"\x77");
            // 4. The baseline pass then reads the changed byte.
            let never = AtomicBool::new(false);
            let base = rig.snap.baseline;
            let mut lines = Vec::new();
            let pass = baseline_pass(&mut fs, &rig.target, &base, &never, &mut |d, t| {
                lines.push(checking_line(d, t))
            })
            .unwrap();
            assert_eq!(
                lines.last().map(String::as_str),
                Some("checking the file for Save — 100 %")
            );
            // 3, in the second order: the page is still on screen as it was shown, and is
            // edited only now.
            let seen =
                seen_before.unwrap_or_else(|| Seen::pages([(page, as_shown), (0x40000, far)]));
            rig.snap.seen = seen.clone();
            assert_eq!(pass.size, 300 * KIB);
            assert_ne!(
                pass.sha256,
                sha_of(&body),
                "the pass hashed the writer's bytes"
            );
            assert_eq!(pass.page(page), Some(short(&shown(&rig.path, page))));
            let Seen::Known(ev) = &seen else {
                unreachable!()
            };
            assert_eq!(pass.agrees(&ev.pages), Err(page), "{side}");
            // The window takes the pass's hash as its baseline — and Save refuses.
            rig.snap.baseline.sha256 = Some(pass.sha256);
            fs.read = 0;
            let why = refusal(go(
                &mut fs,
                &mut rig,
                Opts {
                    pass: Some(&pass),
                    ..Opts::default()
                },
            ));
            assert_eq!(why, Refusal::UnderEdits { offset: page }, "{side}");
            assert_eq!(
                why.to_string(),
                "the file changed under your edits — Cmd/Ctrl+R reloads"
            );
            assert_eq!(fs.changes, Vec::<&str>::new(), "{side}: nothing was sent");
            let theirs = with_byte(body, page + 200, 0x77);
            assert_eq!(std::fs::read(&rig.path).unwrap(), theirs, "{side}");
        }
    }
}

/// The pass itself: the whole SHA-256, a short hash per 4 KiB page (the last one short), fed in
/// any pieces; and it refuses a file that is not the one opened.
#[test]
fn the_baseline_pass_hashes_the_whole_and_every_page() {
    let body = patterned(3 * PAGE + 100);
    let mut pass = BaselinePass::new();
    for piece in body.chunks(1000) {
        pass.update(piece);
    }
    let base = pass.finish();
    assert_eq!((base.size, base.sha256), (body.len() as u64, sha_of(&body)));
    for (i, page) in body.chunks(PAGE as usize).enumerate() {
        assert_eq!(base.page(i as u64 * PAGE), Some(short(&sha_of(page))));
    }
    assert_eq!(base.page(4 * PAGE), None);
    let seen: BTreeMap<u64, Sha> = [(PAGE, sha_of(&body[4096..8192]))].into();
    assert_eq!(base.agrees(&seen), Ok(()));
    let past: BTreeMap<u64, Sha> = [(8 * PAGE, [1; 32])].into();
    assert_eq!(base.agrees(&past), Err(8 * PAGE));
    assert_eq!(BaselinePass::new().finish().sha256, sha_of(b""));

    for (side, mut fs) in sides() {
        let (rig, body) = small(&mut fs);
        let never = AtomicBool::new(false);
        let base = rig.snap.baseline;
        let pass = baseline_pass(&mut fs, &rig.target, &base, &never, &mut |_, _| {}).unwrap();
        assert_eq!(pass.sha256, sha_of(&body), "{side}");
        assert_eq!(fs.read, 300 * KIB, "{side}: read from the remote");
        set_mtime(&rig.path, OLD + 5);
        let e = baseline_pass(&mut fs, &rig.target, &base, &never, &mut |_, _| {}).unwrap_err();
        assert!(
            e.to_string().contains("changed since it was opened"),
            "{e:#}"
        );
        let stop = AtomicBool::new(true);
        set_mtime(&rig.path, OLD);
        let e = baseline_pass(&mut fs, &rig.target, &base, &stop, &mut |_, _| {}).unwrap_err();
        assert!(is_cancelled(&e), "{e:#}");
    }
}

// ---------------------------------------------------------------------------------------------
// E7, E8, E9: the route.

/// E7: `statvfs` reports less than size + 1 GiB. Refused before any request that writes, naming
/// the needed and the free bytes, with Write in place… and Save As… on offer.
#[test]
fn e7_no_room_is_refused_before_anything_is_written() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        let need = 300 * KIB + ROOM_MARGIN;
        fs.free = Some(Some(need - 1));
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        assert_eq!(
            why,
            Refusal::NoRoom {
                dir: rig.folder().to_str().unwrap().to_string(),
                need,
                free: need - 1
            },
            "{side}"
        );
        assert!(why.offers_other_ways());
        let text = why.to_string();
        assert!(text.starts_with("needs 1.1 GB free in /"), "{text}");
        assert!(text.ends_with(", has 1.1 GB"), "{text}");
        assert_eq!(fs.changes, Vec::<&str>::new(), "{side}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), body);
        assert_eq!(rig.phase(), "none");
        // Exactly enough, or a server without `statvfs`: the Save runs.
        fs.free = Some(Some(need));
        assert_eq!(
            route(&mut fs, &rig.target, 300 * KIB, None),
            Ok(Route::ServerCopy)
        );
        fs.free = Some(None);
        saved(go(&mut fs, &mut rig, Opts::default()));
        // The server's own `statvfs` answers for the folder.
        fs.free = None;
        let own = fs.free_space(rig.folder().to_str().unwrap());
        assert!(own.is_some(), "{side}: no statvfs");
    }
    // The spec's words.
    let no_room = Refusal::NoRoom {
        dir: "/data".to_string(),
        need: 24_000_000_000,
        free: 3_100_000_000,
    };
    assert_eq!(
        no_room.to_string(),
        "needs 24.0 GB free in /data, has 3.1 GB"
    );
}

/// E8: no `copy-data` and a 300 MiB file. Save is refused before any transfer, with the cost
/// stated. Write in place — the dialog's button — then puts the patch in the remote file, and
/// the undo file exists.
#[test]
fn e8_without_copy_data_a_big_file_is_refused_and_can_be_written_in_place() {
    for (side, mut fs) in sides_without_copy() {
        let runs = vec![run(200 * MIB + 7, b"\x11\x22"), run(299 * MIB, b"\x33")];
        let mut rig = Rig::new(&mut fs, Body::Sparse(300 * MIB), runs);
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
        assert_eq!(
            why.to_string(),
            "rewriting 314.6 MB through this machine moves 629.1 MB (about 3 min at the current \
             3.6 MB/s); this server has no copy-data (OpenSSH before 9.0)",
            "{side}"
        );
        assert!(why.offers_other_ways());
        fs.moved(0, 0, &format!("{side}: before any transfer"));
        assert_eq!(fs.changes, Vec::<&str>::new(), "{side}");
        assert_eq!(rig.phase(), "none");
        assert!(!rig.undo.path().exists());

        let question = in_place_question("box", &rig.target, 3, rig.undo.path());
        assert!(
            question.starts_with("Write the 3 changed bytes into box:/"),
            "{question}"
        );
        assert!(question.contains("This is not atomic."), "{question}");
        let saved = saved(go(
            &mut fs,
            &mut rig,
            Opts {
                in_place: true,
                ..Opts::default()
            },
        ));
        assert_eq!(saved.route, Route::InPlace, "{side}");
        assert_eq!(at(&rig.path, 200 * MIB + 7, 2), b"\x11\x22", "{side}");
        assert_eq!(at(&rig.path, 299 * MIB, 1), b"\x33", "{side}");
        fs.moved(2 * PAGE, 3, &format!("{side}: two pages, three bytes"));
        assert_eq!(
            fs.changes,
            ["write", "write", "fsync"],
            "{side}: no temp, no rename"
        );
        let undo = UndoFile::parse(&std::fs::read_to_string(rig.undo.path()).unwrap()).unwrap();
        assert_eq!(
            undo.runs,
            [run(200 * MIB + 7, &[0, 0]), run(299 * MIB, &[0])]
        );
        assert_eq!(undo.source, format!("box:{}", rig.target));
        assert_eq!(undo.size, 300 * MIB);
        // The next baseline is the target's new stat.
        let now = fs.lstat_opt(&rig.target).unwrap().unwrap();
        assert_eq!((saved.next.size, saved.next.mtime), (now.size, now.mtime));
        assert_ne!(saved.next.mtime, rig.snap.baseline.mtime, "{side}");
        assert_eq!(rig.phase(), "writing");
        rig.landed();
    }
    // The spec's words, for its 24 GB file.
    let cost = Refusal::NoCopyData {
        size: 24_000_000_000,
        rate: Some(3.6e6),
    };
    assert_eq!(
        cost.to_string(),
        "rewriting 24.0 GB through this machine moves 48.0 GB (about 3 h 40 min at the current \
         3.6 MB/s); this server has no copy-data (OpenSSH before 9.0)"
    );
}

/// E9: no `copy-data` and a 1 MiB file: the streamed rewrite. The content checks are E1's, and
/// the upload is the file's size.
#[test]
fn e9_without_copy_data_a_small_file_is_rewritten_through_this_machine() {
    for (side, mut fs) in sides_without_copy() {
        let body = patterned(MIB);
        let runs = vec![run(0xFFE, b"\x01\x02\x03\x04"), run(MIB - 1, b"\xFF")];
        let mut rig = Rig::new(&mut fs, Body::Bytes(body.clone()), runs);
        let mut lines = Vec::new();
        let saved = saved(go(
            &mut fs,
            &mut rig,
            Opts {
                progress: Some(&mut |p| lines.push(p.line("box"))),
                ..Opts::default()
            },
        ));
        let want = patched(body.clone(), &rig.runs);
        assert_eq!(saved.route, Route::StreamedRewrite, "{side}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), want, "{side}");
        assert_eq!(saved.next.sha256, Some(sha_of(&want)), "{side}");
        fs.moved(MIB, MIB, &format!("{side}: one pass, the file's size up"));
        assert!(
            !fs.changes.contains(&"copy") && !fs.changes.contains(&"write"),
            "{side}"
        );
        // A run across a page boundary: both pages are evidence, before and after.
        let pages: Vec<u64> = saved.seen.keys().copied().collect();
        assert_eq!(pages, [0, PAGE, MIB - PAGE]);
        for (&page, sha) in &saved.seen {
            assert_eq!(*sha, shown(&rig.path, page), "{side}");
        }
        assert_eq!(temps(rig.folder()), none(), "{side}");
        assert_eq!(fs.lstat_opt(&rig.target).unwrap().unwrap().mode, 0o640);
        assert!(lines[0].starts_with("saving — rewriting box's file through this machine "));
        rig.write_undo_back();
        assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{side}");
    }
}

/// The `save` record as it is on the disk, read back: `None` when there is none.
fn persisted(rig: &Rig) -> Option<journal::SaveIntent> {
    let file = rig.journal().dir().join("save");
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("{}: {e}", file.display()),
    };
    Some(journal::parse_save(&bytes).unwrap())
}

/// A record's phase by name, and the temp it names.
fn phase_of(intent: &journal::SaveIntent) -> (&'static str, Option<&str>) {
    match &intent.phase {
        Phase::Naming { temp } => ("naming", Some(temp)),
        Phase::Created { temp, .. } => ("created", Some(temp)),
        Phase::Staged { temp, .. } => ("staged", Some(temp)),
        Phase::Renaming { temp, .. } => ("renaming", Some(temp)),
        Phase::Writing { .. } => ("writing", None),
    }
}

/// The streamed rewrite with every step stopped and every step's reply lost. For each, three
/// things are held to what 07 says, each on its own and before anything is reconciled: the
/// target (its bytes and that it is the same file), the temp (whether one is left, and that the
/// record names it), and the `save` record as it is on the disk. Then, apart, what a reopen
/// makes of it.
#[test]
fn a_streamed_rewrite_stopped_or_unanswered_at_any_step() {
    const REWRITE: [Step; 17] = [
        Step::CheckTarget,
        Step::Room,
        Step::OpenSource,
        Step::StatSource,
        Step::Create,
        Step::StatCreated,
        Step::SetOwner,
        Step::SetMode,
        Step::StatReady,
        Step::Rewrite,
        Step::Undo,
        Step::FullMode,
        Step::Fsync,
        Step::StatTemp,
        Step::Recheck,
        Step::Rename,
        Step::StatAfter,
    ];
    let number = |step: Step| REWRITE.iter().position(|s| *s == step).unwrap();
    for (side, mut fs) in sides_without_copy() {
        let (mut rig, _) = small(&mut fs);
        let mut seen = Vec::new();
        saved(hooked(&mut fs, &mut rig, &mut |step| {
            seen.push(step);
            At::Go
        }));
        assert_eq!(seen, REWRITE, "{side}");

        for at in [At::Stop, At::LoseReply] {
            for step in REWRITE {
                let (mut rig, body) = small(&mut fs);
                let same = ident(&rig.path);
                let after = patched(body.clone(), &rig.runs);
                let got = hooked(&mut fs, &mut rig, &mut |s| match s == step {
                    true => at,
                    false => At::Go,
                });
                let tag = format!("{side}, {at:?} at {step:?}");
                let n = number(step);

                // What 07 says of this step: is the Save reported saved, is the target
                // replaced, is a temp left, and what the record says.
                let (ok, replaced, temp_left, phase) = match (at, step) {
                    (At::Stop, _) => {
                        let phase = if n < number(Step::Create) {
                            "none"
                        } else if n <= number(Step::StatCreated) {
                            "naming"
                        } else if n <= number(Step::Recheck) {
                            "created"
                        } else {
                            "renaming"
                        };
                        let left = n > number(Step::Create) && step != Step::StatAfter;
                        (false, step == Step::StatAfter, left, phase)
                    }
                    // Nothing of their own is sent (`statvfs` is advisory; the undo file is
                    // local), or the rename was already answered: saved.
                    (_, Step::Room | Step::Undo | Step::StatAfter) => {
                        (true, true, false, "renaming")
                    }
                    // It landed, unanswered.
                    (_, Step::Rename) => (false, true, false, "renaming"),
                    // Made by the server, never known to be this Save's.
                    (_, Step::Create) => (false, false, true, "naming"),
                    // Every other lost reply: the Save's own temp removed, the record ended.
                    _ => (false, false, false, "none"),
                };

                // 1. What the Save said.
                match (&got, at) {
                    (Ok(SaveOutcome::Saved(saved)), _) => {
                        assert!(ok, "{tag}: {saved:?}");
                        let unchecked = step == Step::StatAfter;
                        assert_eq!(saved.notes.len(), unchecked as usize, "{tag}: {saved:?}");
                    }
                    (Err(e), At::Stop) => {
                        assert!(!ok && is_stopped(e), "{tag}: {e:#}");
                    }
                    (Err(e), _) => {
                        let unknown = matches!(step, Step::Create | Step::Rename);
                        assert!(!ok, "{tag}: {e:#}");
                        assert_eq!(is_outcome_unknown(e), unknown, "{tag}: {e:#}");
                    }
                    (other, _) => panic!("{tag}: {other:?}"),
                }

                // 2. The target: its bytes, and whether it is still the same file.
                let now = std::fs::read(&rig.path).unwrap();
                let want = if replaced { &after } else { &body };
                assert!(now == *want, "{tag}: the target's bytes");
                assert_eq!(ident(&rig.path) == same, !replaced, "{tag}: the same file");
                let mode = std::fs::metadata(&rig.path).unwrap().mode() & 0o7777;
                assert_eq!(mode, 0o640, "{tag}");

                // 3. The temp: whether one is left, and as far as it got.
                let left = temps(rig.folder());
                assert_eq!(left.len(), temp_left as usize, "{tag}: {left:?}");
                if let Some(name) = left.first() {
                    let meta = std::fs::metadata(rig.folder().join(name)).unwrap();
                    let private = n <= number(Step::SetMode);
                    let mode = if private { 0o600 } else { 0o640 };
                    assert_eq!(meta.mode() & 0o7777, mode, "{tag}");
                    let filled = n > number(Step::Rewrite);
                    let size = if filled { body.len() as u64 } else { 0 };
                    assert_eq!(meta.len(), size, "{tag}");
                }

                // 4. The record, as it is on the disk — and as the journal holds it.
                let record = persisted(&rig);
                assert_eq!(record.as_ref(), rig.journal().pending_save(), "{tag}");
                assert_eq!(
                    record.as_ref().map_or("none", |r| phase_of(r).0),
                    phase,
                    "{tag}"
                );
                if let Some(record) = &record {
                    assert_eq!(record.route, Route::StreamedRewrite, "{tag}");
                    assert_eq!(record.kind, Kind::Hex, "{tag}");
                    assert_eq!(record.target, rig.target, "{tag}");
                    assert_eq!(record.baseline, rig.snap.baseline, "{tag}");
                    assert_eq!(record.seen, rig.snap.seen, "{tag}");
                    // It names the temp that is there; with none left, a name nothing has.
                    let named = Path::new(phase_of(record).1.unwrap());
                    assert_eq!(named.parent(), Some(rig.folder()), "{tag}");
                    match left.first() {
                        Some(name) => assert_eq!(named, rig.folder().join(name), "{tag}"),
                        None => assert!(!named.exists(), "{tag}"),
                    }
                    let Seen::Known(pre) = &rig.snap.seen else {
                        unreachable!()
                    };
                    if let Phase::Renaming { staged, post, .. } = &record.phase {
                        assert_eq!(staged.size, body.len() as u64, "{tag}");
                        assert_eq!(post.next_baseline.sha256, Some(sha_of(&after)), "{tag}");
                        let pages: Vec<_> = post.check.pages.keys().collect();
                        assert_eq!(pages, pre.pages.keys().collect::<Vec<_>>(), "{tag}");
                    }
                }

                // The undo file: only once the pass has read and checked everything.
                let passed = match at {
                    At::Stop => n > number(Step::Undo),
                    _ => ok || n >= number(Step::Undo),
                };
                assert_eq!(rig.undo.path().exists(), passed, "{tag}");

                // 5. Apart from all that: what a reopen makes of the record.
                if record.is_none() || ok {
                    continue;
                }
                let (outcome, temp, _) = rig.reconciled(&mut fs);
                let landed = match replaced {
                    true => Outcome::Landed,
                    false => Outcome::NotLanded,
                };
                assert_eq!(outcome, landed, "{tag}");
                let verdict = match temp {
                    Temp::Gone(_) => "gone",
                    Temp::Remove(_) => "remove",
                    Temp::Keep { .. } => "keep",
                    Temp::None => "none",
                };
                let want = match (at, step) {
                    (At::Stop, Step::Create | Step::StatAfter) => "gone",
                    // Exactly as recorded: private as created, ready to be filled, or staged.
                    (At::Stop, Step::SetOwner | Step::SetMode | Step::Rewrite | Step::Rename) => {
                        "remove"
                    }
                    // The create's reply or the temp's new mode not yet in the record, or the
                    // temp filled since: nothing proves whose it is.
                    (At::Stop, _) => "keep",
                    (_, Step::Create) => "keep",
                    _ => "gone",
                };
                assert_eq!(verdict, want, "{tag}");
            }
        }
    }
}

/// Without `posix-rename@openssh.com` Save is off, as today, before anything is sent.
#[test]
fn a_server_that_cannot_replace_is_refused() {
    let mut fs = Probe::local();
    fs.no_replace = true;
    let (mut rig, _) = small(&mut fs);
    let why = refusal(go(&mut fs, &mut rig, Opts::default()));
    assert_eq!(why, Refusal::CannotReplace);
    assert!(!why.offers_other_ways());
    assert_eq!(fs.changes, Vec::<&str>::new());
}

// ---------------------------------------------------------------------------------------------
// Cancel, and every step stopped or its reply lost.

/// Cancel between copy requests: the temp is removed and the target is untouched.
#[test]
fn cancel_between_copy_requests_removes_the_temp() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        let cancel = AtomicBool::new(false);
        let mut requests = 0;
        let e = go(
            &mut fs,
            &mut rig,
            Opts {
                pace: Some(Pace::steps(64 * KIB)),
                cancel: Some(&cancel),
                progress: Some(&mut |p| {
                    if matches!(p, Progress::Copying { .. }) {
                        requests += 1;
                        cancel.store(requests == 2, Ordering::Relaxed);
                    }
                }),
                ..Opts::default()
            },
        )
        .unwrap_err();
        assert!(is_cancelled(&e), "{side}: {e:#}");
        assert_eq!(
            requests, 2,
            "{side}: the request sent finished; no other was sent"
        );
        assert_eq!(temps(rig.folder()), none(), "{side}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{side}");
        assert!(!fs.changes.contains(&"write") && !fs.changes.contains(&"rename"));
        assert_eq!(rig.phase(), "none", "{side}");
        assert!(rig.undo.is_empty());
    }
}

/// The steps of a server-copy Save of [`small`] in 128 KiB requests, in order.
const STEPS: [Step; 22] = [
    Step::CheckTarget,
    Step::Room,
    Step::OpenSource,
    Step::StatSource,
    Step::Create,
    Step::StatCreated,
    Step::SetOwner,
    Step::SetMode,
    Step::StatReady,
    Step::Copy(0),
    Step::Copy(1),
    Step::Copy(2),
    Step::ReadBack,
    Step::Undo,
    Step::Patch(0),
    Step::Patch(1),
    Step::FullMode,
    Step::Fsync,
    Step::StatTemp,
    Step::Recheck,
    Step::Rename,
    Step::StatAfter,
];

fn step_number(step: Step) -> usize {
    STEPS.iter().position(|s| *s == step).unwrap()
}

fn hooked(fs: &mut Probe, rig: &mut Rig, hook: &mut dyn FnMut(Step) -> At) -> Result<SaveOutcome> {
    go(
        fs,
        rig,
        Opts {
            hook: Some(hook),
            pace: Some(Pace::steps(128 * KIB)),
            ..Opts::default()
        },
    )
}

/// Every step of a Save has a hook, fired in §4.2's order right before the step's request.
#[test]
fn every_step_is_hooked_in_order() {
    for (side, mut fs) in sides() {
        let (mut rig, _) = small(&mut fs);
        let mut seen = Vec::new();
        saved(hooked(&mut fs, &mut rig, &mut |step| {
            seen.push(step);
            At::Go
        }));
        assert_eq!(seen, STEPS, "{side}");
        assert_eq!(
            fs.changes,
            [
                "create", "fsetstat", "fsetstat", "copy", "write", "write", "fsetstat", "fsync",
                "rename"
            ],
            "{side}"
        );
    }
}

/// A Save stopped before each step, as a kill would stop it: the target is untouched until the
/// rename has been sent, nothing is cleaned up, and the journal's record lets a reopen tell
/// what happened and whether the temp may be removed (§3.1).
#[test]
fn a_save_stopped_at_any_step_leaves_the_target_whole_and_the_record_true() {
    for (side, mut fs) in sides() {
        for stop in STEPS {
            let (mut rig, body) = small(&mut fs);
            let same = ident(&rig.path);
            let e = hooked(&mut fs, &mut rig, &mut |step| match step == stop {
                true => At::Stop,
                false => At::Go,
            })
            .unwrap_err();
            let tag = format!("{side}, stopped before {stop:?}");
            assert!(is_stopped(&e), "{tag}: {e:#}");
            let landed = stop == Step::StatAfter;
            // The same file, not only the same bytes — until the rename has been sent.
            assert_eq!(ident(&rig.path) == same, !landed, "{tag}");
            let want = match landed {
                true => patched(body, &rig.runs),
                false => body,
            };
            assert_eq!(std::fs::read(&rig.path).unwrap(), want, "{tag}");
            let n = step_number(stop);
            let phase = if n < step_number(Step::Create) {
                "none"
            } else if n <= step_number(Step::StatCreated) {
                "naming"
            } else if n <= step_number(Step::Recheck) {
                "created"
            } else {
                "renaming"
            };
            assert_eq!(rig.phase(), phase, "{tag}");
            // The undo file is durable before the first byte of the temp is patched, and not
            // written before the content check passed.
            let undone = n > step_number(Step::Undo);
            assert_eq!(rig.undo.path().exists(), undone, "{tag}");
            if undone {
                let text = std::fs::read_to_string(rig.undo.path()).unwrap();
                let undo = UndoFile::parse(&text).unwrap();
                let old = [
                    run(0x1005, &at(&rig.path, 0x1005, 4)),
                    run(0x40000, &at(&rig.path, 0x40000, 1)),
                ];
                if !landed {
                    assert_eq!(undo.runs, old, "{tag}");
                }
            }
            let left = temps(rig.folder());
            if phase == "none" {
                assert_eq!(left, none(), "{tag}");
                continue;
            }
            let (outcome, temp, _) = rig.reconciled(&mut fs);
            match stop {
                // Named before its create: nothing is there.
                Step::Create => {
                    assert_eq!(left, none(), "{tag}");
                    assert!(matches!(temp, Temp::Gone(_)), "{tag}: {temp:?}");
                }
                // Created, its reply not yet recorded: never assumed to be sopen's.
                Step::StatCreated => {
                    assert_eq!(left.len(), 1, "{tag}");
                    assert!(matches!(temp, Temp::Keep { .. }), "{tag}: {temp:?}");
                }
                // Private as created, or its owner and mode set and the record brought up to
                // them: the recorded stat still fits exactly, so it is this Save's.
                Step::SetOwner | Step::SetMode | Step::Copy(0) => {
                    assert!(matches!(temp, Temp::Remove(_)), "{tag}: {temp:?}");
                }
                // Staged and checked, the rename not sent: removed on reopen.
                Step::Rename => assert!(matches!(temp, Temp::Remove(_)), "{tag}: {temp:?}"),
                Step::StatAfter => assert!(matches!(temp, Temp::Gone(_)), "{tag}: {temp:?}"),
                // Everywhere else the temp is no longer exactly as recorded — its mode just
                // set, or filled in part or whole: nothing proves it is this Save's, so it is
                // kept and named.
                _ => assert!(matches!(temp, Temp::Keep { .. }), "{tag}: {temp:?}"),
            }
            let want = match landed {
                true => Outcome::Landed,
                false => Outcome::NotLanded,
            };
            assert_eq!(outcome, want, "{tag}");
        }
    }
}

/// A Save killed and reopened, with `stop` the step it was killed before.
fn killed(fs: &mut Probe, stop: Step) -> (Rig, Vec<u8>, PathBuf) {
    let (mut rig, body) = small(fs);
    let e = hooked(fs, &mut rig, &mut |step| match step == stop {
        true => At::Stop,
        false => At::Go,
    })
    .unwrap_err();
    assert!(is_stopped(&e), "killed before {stop:?}: {e:#}");
    let left = temps(rig.folder());
    assert_eq!(left.len(), 1, "killed before {stop:?}");
    assert_eq!(rig.phase(), "created", "killed before {stop:?}");
    let temp = rig.folder().join(&left[0]);
    (rig, body, temp)
}

/// E18, a Save killed mid-copy. Its temp is no longer the file the record describes — it grew
/// — and its name, owner, mode and size prove nothing once the process is gone: it is kept and
/// named, without a claim that it is sopen's, for the user to remove. Killed right after the record was brought
/// up to the temp's owner and mode, before the first copy request, the recorded stat fits
/// exactly and the temp is removed.
#[test]
fn a_temp_left_by_a_kill_is_removed_only_on_an_exact_match() {
    for (side, mut fs) in sides() {
        // Right after `StatReady`, before the first copy request: exactly as recorded.
        let (rig, body, temp_path) = killed(&mut fs, Step::Copy(0));
        let (outcome, temp, _) = rig.reconciled(&mut fs);
        assert_eq!(outcome, Outcome::NotLanded, "{side}");
        assert_eq!(
            temp,
            Temp::Remove(temp_path.to_str().unwrap().into()),
            "{side}"
        );
        fs.remove(temp_path.to_str().unwrap()).unwrap();
        assert_eq!(temps(rig.folder()), none(), "{side}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{side}");

        // Mid-copy, after one request and after the last: kept, named, and said to be a partial
        // copy that could not be proved sopen's.
        for (stop, copied) in [(Step::Copy(1), 128 * KIB), (Step::Copy(2), 256 * KIB)] {
            let (rig, body, temp_path) = killed(&mut fs, stop);
            let tag = format!("{side}, killed before {stop:?}");
            assert_eq!(
                std::fs::metadata(&temp_path).unwrap().len(),
                copied,
                "{tag}"
            );
            let (outcome, temp, line) = rig.reconciled(&mut fs);
            assert_eq!(outcome, Outcome::NotLanded, "{tag}");
            let Temp::Keep { path, why } = temp else {
                panic!("{tag}: {temp:?}");
            };
            assert_eq!(Path::new(&path), temp_path, "{tag}");
            let want = format!(
                "a file at the interrupted save's temporary path {path} ({copied} bytes); sopen \
                 could not verify it is its own partial copy, so it was not removed"
            );
            assert_eq!(why, want, "{tag}");
            assert!(line.contains(&path) && line.contains(&why), "{line}");
            assert!(temp_path.exists(), "{tag}");
            assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{tag}");
        }
    }
}

/// The adversarial reopen: after the kill, another file takes the temp's name — a regular file
/// of the same owner and mode, smaller than the target, with other bytes and another
/// modification time. Nothing but an exact match of the recorded stat removes a temp, so it is
/// kept, and its bytes are as its owner wrote them.
#[test]
fn a_file_put_under_the_temps_name_after_a_kill_is_never_removed() {
    for (side, mut fs) in sides() {
        for stop in [Step::Copy(0), Step::Copy(1), Step::Copy(2), Step::Patch(0)] {
            let (rig, body, temp_path) = killed(&mut fs, stop);
            let tag = format!("{side}, killed before {stop:?}");
            let was = std::fs::metadata(&temp_path).unwrap();
            // Someone else's file, under that name: same owner, same mode, smaller than the
            // target, other bytes, another time.
            std::fs::remove_file(&temp_path).unwrap();
            let theirs = b"someone else's file, not a byte of the target".repeat(100);
            assert!((theirs.len() as u64) < rig.snap.baseline.size);
            std::fs::write(&temp_path, &theirs).unwrap();
            let mode = std::fs::Permissions::from_mode(was.mode() & 0o7777);
            std::fs::set_permissions(&temp_path, mode).unwrap();
            set_mtime(&temp_path, OLD + 777);
            let now = std::fs::metadata(&temp_path).unwrap();
            assert_eq!((now.uid(), now.gid()), (was.uid(), was.gid()), "{tag}");
            assert_eq!(now.mode(), was.mode(), "{tag}");

            let (outcome, temp, line) = rig.reconciled(&mut fs);
            assert_eq!(outcome, Outcome::NotLanded, "{tag}");
            let Temp::Keep { path, .. } = temp else {
                panic!("{tag}: someone else's file would be removed: {temp:?}");
            };
            assert_eq!(Path::new(&path), temp_path, "{tag}");
            assert!(line.contains(&path), "{line}");
            assert_eq!(
                std::fs::read(&temp_path).unwrap(),
                theirs,
                "{tag}: untouched"
            );
            assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{tag}");
        }
    }
}

/// The reply of each step lost: before the rename the target is untouched and the Save's own
/// temp is removed (a lost create's is named and kept, never removed); a lost rename reply is an
/// unknown outcome naming both paths, never "nothing was written".
#[test]
fn a_lost_reply_at_any_step_is_what_07_says() {
    for (side, mut fs) in sides() {
        for lost in STEPS {
            let (mut rig, body) = small(&mut fs);
            let same = ident(&rig.path);
            let got = hooked(&mut fs, &mut rig, &mut |step| match step == lost {
                true => At::LoseReply,
                false => At::Go,
            });
            let tag = format!("{side}, the reply of {lost:?} lost");
            let after = patched(body.clone(), &rig.runs);
            let now = std::fs::read(&rig.path).unwrap();
            let left = temps(rig.folder());
            let replaced = matches!(
                lost,
                Step::Room | Step::Undo | Step::Rename | Step::StatAfter
            );
            assert_eq!(ident(&rig.path) == same, !replaced, "{tag}");
            match lost {
                // Steps that send nothing of their own (`statvfs` is advisory; the undo file is
                // local): the Save goes on.
                Step::Room | Step::Undo => {
                    saved(got);
                    assert_eq!(now, after, "{tag}");
                }
                // The rename's reply was in: saved, and the missing check is said.
                Step::StatAfter => {
                    let saved = saved(got);
                    assert_eq!(now, after, "{tag}");
                    assert!(saved.notes[0].contains("could not be checked"), "{tag}");
                }
                Step::Rename => {
                    let e = got.unwrap_err();
                    assert!(is_outcome_unknown(&e), "{tag}: {e:#}");
                    let text = format!("{e:#}");
                    assert!(
                        text.contains("got no reply: outcome unknown — check /"),
                        "{text}"
                    );
                    assert!(text.contains(".model.bin.sopen-"), "{text}");
                    assert!(!text.contains("not replaced"), "{text}");
                    assert_eq!(now, after, "{tag}: it landed");
                    assert_eq!(rig.phase(), "renaming", "{tag}");
                    let (outcome, temp, line) = rig.reconciled(&mut fs);
                    assert_eq!(outcome, Outcome::Landed, "{tag}");
                    assert!(matches!(temp, Temp::Gone(_)), "{tag}");
                    assert!(line.contains("landed (found on reopen)"), "{line}");
                }
                Step::Create => {
                    let e = got.unwrap_err();
                    assert!(is_outcome_unknown(&e), "{tag}: {e:#}");
                    assert!(format!("{e:#}").contains("was not replaced, but creating"));
                    assert_eq!(now, body, "{tag}");
                    assert_eq!(left.len(), 1, "{tag}: named, never removed");
                    assert_eq!(rig.phase(), "naming", "{tag}");
                    let (outcome, temp, _) = rig.reconciled(&mut fs);
                    assert_eq!(outcome, Outcome::NotLanded, "{tag}");
                    assert!(matches!(temp, Temp::Keep { .. }), "{tag}: {temp:?}");
                }
                _ => {
                    let e = got.unwrap_err();
                    assert_eq!(now, body, "{tag}");
                    assert_eq!(left, none(), "{tag}: {e:#}");
                    assert_eq!(rig.phase(), "none", "{tag}");
                    // The temp is gone and the target was never written: nothing is unknown.
                    assert!(!is_outcome_unknown(&e), "{tag}: {e:#}");
                    let created = step_number(lost) > step_number(Step::Create);
                    assert_eq!(
                        format!("{e:#}").contains("was not replaced"),
                        created,
                        "{tag}: {e:#}"
                    );
                }
            }
        }
    }
}

/// A temp this Save can no longer prove its own is kept and named: its removal's reply lost, or
/// another file put under its name before the removal.
#[test]
fn a_temp_that_is_not_provably_ours_is_kept_and_named() {
    for (side, mut fs) in sides() {
        // The removal's reply is lost.
        let (mut rig, body) = small(&mut fs);
        let e = hooked(&mut fs, &mut rig, &mut |step| match step {
            Step::Fsync | Step::RemoveTemp => At::LoseReply,
            _ => At::Go,
        })
        .unwrap_err();
        let text = format!("{e:#}");
        assert!(
            text.contains("is left behind (its removal got no reply)"),
            "{text}"
        );
        assert_eq!(rig.phase(), "created", "{side}: kept for the reopen");
        assert_eq!(std::fs::read(&rig.path).unwrap(), body);

        // Another file has the temp's name when the refused Save comes to remove it.
        let (mut rig, _) = small(&mut fs);
        other_writer(&rig.path, 250 * KIB, b"\x77");
        let folder = rig.folder().to_path_buf();
        let got = hooked(&mut fs, &mut rig, &mut |step| {
            if step == Step::RemoveTemp {
                let temp = folder.join(&temps(&folder)[0]);
                std::fs::remove_file(&temp).unwrap();
                std::fs::write(&temp, "someone else's file").unwrap();
            }
            At::Go
        });
        let Ok(SaveOutcome::Refused { notes, .. }) = got else {
            panic!("{side}: {got:?}");
        };
        assert!(
            notes[0].contains("is left behind (another file has its name now"),
            "{notes:?}"
        );
        let left = temps(rig.folder());
        assert_eq!(left.len(), 1, "{side}");
        assert_eq!(
            std::fs::read(rig.folder().join(&left[0])).unwrap(),
            b"someone else's file",
            "{side}: never removed, never changed"
        );
        assert_eq!(rig.phase(), "created", "{side}");
    }
}

/// An exclusive-create collision: a file already has the temp's name. The create fails, the
/// Save fails, and that file is never removed or changed.
#[test]
fn a_taken_temp_name_fails_the_save_and_is_left_alone() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        fs.squat = true;
        let e = go(&mut fs, &mut rig, Opts::default()).unwrap_err();
        assert!(!is_outcome_unknown(&e), "{side}: {e:#}");
        assert!(format!("{e:#}").contains("was not replaced"), "{e:#}");
        let left = temps(rig.folder());
        assert_eq!(left.len(), 1, "{side}");
        let theirs = std::fs::read(rig.folder().join(&left[0])).unwrap();
        assert_eq!(theirs, b"someone else's", "{side}");
        assert_eq!(fs.changes, ["create"], "{side}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), body);
        assert_eq!(rig.phase(), "none", "{side}");
    }
}

// ---------------------------------------------------------------------------------------------
// Save copy.

fn go_copy(fs: &mut Probe, rig: &Rig, hook: Hook<'_>) -> Result<CopyOutcome> {
    let job = Job {
        host: "box",
        target: &rig.target,
        runs: &rig.runs,
        pass: None,
        rate: None,
    };
    let never = AtomicBool::new(false);
    let mut quiet = |_: Progress| {};
    let mut clock = stopwatch();
    let mut ctl = Ctl::new(&never, &mut quiet, &mut clock);
    ctl.pace = small_pace();
    ctl.hook = match hook {
        Some(hook) => Some(hook),
        None => None,
    };
    save_copy(fs, &job, &rig.snap, &mut ctl)
}

/// Save copy (§6): the edited file appears as `<stem>.edited<ext>`, complete, by a hard link from
/// a temp — never over anything; the original is not written. With and without `copy-data`.
#[test]
fn save_copy_publishes_a_new_sibling_and_never_replaces() {
    assert_eq!(edited_name("model.bin"), "model.edited.bin");
    assert_eq!(edited_name("Makefile"), "Makefile.edited");
    assert_eq!(edited_name(".profile"), ".profile.edited");
    let all = sides().into_iter().chain(sides_without_copy());
    for (side, mut fs) in all {
        let (rig, body) = small(&mut fs);
        let want = patched(body.clone(), &rig.runs);
        let copy = rig.folder().join("model.edited.bin");
        let got = go_copy(&mut fs, &rig, None).unwrap();
        let path = copy.to_str().unwrap().to_string();
        let saved = CopyOutcome::Saved {
            path,
            checked: None,
            notes: Vec::new(),
        };
        assert_eq!(got, saved, "{side}");
        assert_eq!(std::fs::read(&copy).unwrap(), want, "{side}");
        assert_eq!(
            std::fs::read(&rig.path).unwrap(),
            body,
            "{side}: not written"
        );
        let original = fs.lstat_opt(&rig.target).unwrap().unwrap();
        assert_eq!(original.mtime, rig.snap.baseline.mtime, "{side}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        let mode = std::fs::metadata(&copy).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o640, "{side}");
        assert!(!fs.changes.contains(&"rename"), "{side}: never a rename");
        assert_eq!(rig.phase(), "none", "{side}: no save record");
        assert!(!rig.undo.path().exists(), "{side}: nothing was replaced");

        // The name is taken now: the next copy is `-1`, and the first is untouched.
        std::fs::write(&copy, "theirs").unwrap();
        let got = go_copy(&mut fs, &rig, None).unwrap();
        let CopyOutcome::Saved { path, .. } = got else {
            panic!("{side}: {got:?}");
        };
        assert!(path.ends_with("/model.edited-1.bin"), "{path}");
        assert_eq!(std::fs::read(&path).unwrap(), want, "{side}");
        assert_eq!(std::fs::read(&copy).unwrap(), b"theirs", "{side}");

        // A file that changed is not copied as if it were the one edited.
        other_writer(&rig.path, 250 * KIB, b"\x77");
        let got = go_copy(&mut fs, &rig, None).unwrap();
        assert!(
            matches!(&got, CopyOutcome::Refused { notes, .. } if notes.is_empty()),
            "{side}: {got:?}"
        );
        assert_eq!(temps(rig.folder()), none(), "{side}");
        assert!(!rig.folder().join("model.edited-2.bin").exists(), "{side}");
    }
}

/// A Save copy whose link got no reply may be in place: it says so, naming the path, and the
/// temp goes either way. Stopped before the link, nothing has the new name.
#[test]
fn save_copy_with_a_lost_link_reply_names_the_copy() {
    for (side, mut fs) in sides() {
        let (rig, body) = small(&mut fs);
        let e = go_copy(
            &mut fs,
            &rig,
            Some(&mut |step| match step {
                Step::Link => At::LoseReply,
                _ => At::Go,
            }),
        )
        .unwrap_err();
        assert!(is_outcome_unknown(&e), "{side}: {e:#}");
        let text = format!("{e:#}");
        assert!(
            text.contains("the copy may be in place — check /"),
            "{text}"
        );
        assert!(text.contains("/model.edited.bin"), "{text}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        let copy = rig.folder().join("model.edited.bin");
        assert_eq!(std::fs::read(&copy).unwrap(), patched(body, &rig.runs));

        // A reply lost before the link was sent: no copy, and nothing is unknown.
        let (rig, _) = small(&mut fs);
        let e = go_copy(
            &mut fs,
            &rig,
            Some(&mut |step| match step {
                Step::Fsync => At::LoseReply,
                _ => At::Go,
            }),
        )
        .unwrap_err();
        assert!(!is_outcome_unknown(&e), "{side}: {e:#}");
        let text = format!("{e:#}");
        assert!(text.starts_with("no copy saved"), "{text}");
        assert!(!text.contains("may be in place"), "{text}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        assert!(!rig.folder().join("model.edited.bin").exists(), "{side}");

        let (rig, _) = small(&mut fs);
        let e = go_copy(
            &mut fs,
            &rig,
            Some(&mut |step| match step {
                Step::Link => At::Stop,
                _ => At::Go,
            }),
        )
        .unwrap_err();
        assert!(is_stopped(&e), "{side}: {e:#}");
        assert!(!rig.folder().join("model.edited.bin").exists(), "{side}");
    }
}

// ---------------------------------------------------------------------------------------------
// Write in place: every step.

const IN_PLACE: [Step; 10] = [
    Step::CheckTarget,
    Step::OpenSource,
    Step::StatSource,
    Step::ReadPages,
    Step::Undo,
    Step::Recheck,
    Step::Write(0),
    Step::Write(1),
    Step::Fsync,
    Step::StatAfter,
];

/// Two runs in one 4 KiB page of a small file (so the whole content is checked too).
fn two_runs_in_a_page(fs: &mut Probe) -> (Rig, Vec<u8>) {
    let body = patterned(300 * KIB);
    let runs = vec![run(0x2010, b"\xAA\xBB"), run(0x2800, b"\xCC")];
    (Rig::new(fs, Body::Bytes(body.clone()), runs), body)
}

fn in_place_hooked(
    fs: &mut Probe,
    rig: &mut Rig,
    hook: &mut dyn FnMut(Step) -> At,
) -> Result<SaveOutcome> {
    go(
        fs,
        rig,
        Opts {
            hook: Some(hook),
            in_place: true,
            ..Opts::default()
        },
    )
}

/// Write in place, stopped before each step: nothing is written before the undo file is durable
/// and the runs are recorded; stopped after the first of two runs in one page, the reopen says
/// "in part", judged run by run.
#[test]
fn a_write_in_place_stopped_at_any_step_is_judged_run_by_run() {
    for (side, mut fs) in sides() {
        let (mut rig, _) = two_runs_in_a_page(&mut fs);
        let mut seen = Vec::new();
        let saved = saved(in_place_hooked(&mut fs, &mut rig, &mut |step| {
            seen.push(step);
            At::Go
        }));
        assert_eq!(seen, IN_PLACE, "{side}");
        assert_eq!(saved.checked, None, "{side}: the whole content was checked");
        assert_eq!(saved.next.sha256, Some(file_sha(&rig.path)), "{side}");

        for stop in IN_PLACE {
            let (mut rig, body) = two_runs_in_a_page(&mut fs);
            let same = ident(&rig.path);
            let e = in_place_hooked(&mut fs, &mut rig, &mut |step| match step == stop {
                true => At::Stop,
                false => At::Go,
            })
            .unwrap_err();
            let tag = format!("{side}, stopped before {stop:?}");
            assert!(is_stopped(&e), "{tag}: {e:#}");
            let now = std::fs::read(&rig.path).unwrap();
            let n = IN_PLACE.iter().position(|s| *s == stop).unwrap();
            // The whole file at every stop: unchanged before the first write, only the first
            // run before the second, both after that — and always the same file.
            let whole = match stop {
                Step::Write(1) => patched(body.clone(), &rig.runs[..1]),
                Step::Fsync | Step::StatAfter => patched(body.clone(), &rig.runs),
                _ => body.clone(),
            };
            assert!(now == whole, "{tag}: the whole file");
            assert_eq!(ident(&rig.path).0, same.0, "{tag}: the same file");
            // The undo file is durable before the runs are recorded, and both before a write.
            let recorded = n >= 5;
            assert_eq!(rig.undo.path().exists(), recorded, "{tag}");
            assert_eq!(
                rig.phase(),
                if recorded { "writing" } else { "none" },
                "{tag}"
            );
            if !recorded {
                assert_eq!(now, body, "{tag}");
                continue;
            }
            let (outcome, _, line) = rig.reconciled(&mut fs);
            let want = match stop {
                Step::Recheck | Step::Write(0) => Outcome::NotLanded,
                Step::Write(1) => Outcome::InPart,
                _ => Outcome::Landed,
            };
            assert_eq!(outcome, want, "{tag}");
            if stop == Step::Write(1) {
                assert_eq!(now, patched(body, &rig.runs[..1]), "{tag}");
                assert!(line.contains("holds some of your changes"), "{line}");
                assert!(line.contains(".undo.txt"), "{line}");
            }
        }
    }
}

/// Write in place with a reply lost: once a write may have happened, the outcome is unknown on
/// the target itself, and the error says where the original bytes are.
#[test]
fn a_write_in_place_with_a_lost_reply_says_the_file_may_hold_some_changes() {
    for (side, mut fs) in sides() {
        for lost in [Step::Write(0), Step::Write(1), Step::Fsync] {
            let (mut rig, _) = two_runs_in_a_page(&mut fs);
            let e = in_place_hooked(&mut fs, &mut rig, &mut |step| match step == lost {
                true => At::LoseReply,
                false => At::Go,
            })
            .unwrap_err();
            let tag = format!("{side}, the reply of {lost:?} lost");
            assert!(is_outcome_unknown(&e), "{tag}: {e:#}");
            let want = format!(
                "box:{} may hold some of your changes — check it; the original bytes are at {}",
                rig.target,
                rig.undo.path().display()
            );
            assert_eq!(e.to_string(), want, "{tag}");
            assert_eq!(rig.phase(), "writing", "{tag}: kept for the reopen");
        }
        // A check before the first write fails: nothing was written, and the record goes.
        let (mut rig, body) = two_runs_in_a_page(&mut fs);
        let e = in_place_hooked(&mut fs, &mut rig, &mut |step| match step {
            Step::Recheck => At::LoseReply,
            _ => At::Go,
        })
        .unwrap_err();
        assert!(!is_outcome_unknown(&e), "{side}: {e:#}");
        assert!(format!("{e:#}").contains("was not touched"), "{e:#}");
        assert_eq!(std::fs::read(&rig.path).unwrap(), body);
        assert_eq!(rig.phase(), "none", "{side}");
        // The file changed under the edits: refused by the page check, nothing written.
        let (mut rig, _) = two_runs_in_a_page(&mut fs);
        other_writer(&rig.path, 0x2020, b"\x77");
        fs.changes.clear();
        let why = refusal(in_place_hooked(&mut fs, &mut rig, &mut |_| At::Go));
        assert!(
            matches!(why, Refusal::Page { offset: 0x2000, .. }),
            "{side}: {why:?}"
        );
        assert_eq!(fs.changes, Vec::<&str>::new(), "{side}");
        assert!(!rig.undo.path().exists(), "{side}");
    }
}

// ---------------------------------------------------------------------------------------------
// Round 1: flushes, fresh reads, the copy's end, the link's name, cancels.

/// A server without `fsync@openssh.com`: the wrapper hiding it, and the real server told to
/// refuse it (`-P fsync`), which then does not advertise it.
fn sides_without_fsync() -> Vec<(&'static str, Probe)> {
    let mut local = Probe::local();
    local.no_fsync = true;
    let mut out = vec![("LocalFs, fsync hidden", local)];
    if let Some(fs) = SftpRemote::local_server_args(&["-P", "fsync"], None) {
        assert!(!PatchFs::can_fsync(&fs), "-P fsync hides the extension");
        assert!(PatchFs::can_copy(&fs));
        out.push(("local_server() -P fsync", Probe::new(fs)));
    }
    out
}

/// A Save never reports a flush it did not make: on a server that cannot flush, `fsync` is an
/// error of its own and sends nothing; the Save goes ahead and says so, before and after.
#[test]
fn a_server_that_cannot_flush_is_said_never_passed_over() {
    const NOTE: &str = "this server cannot flush to disk; the save is not guaranteed to survive \
                        a power cut on box";
    for (side, mut fs) in sides_without_fsync() {
        // The primitive: an error of its own, never Ok.
        let (mut rig, body) = small(&mut fs);
        let mut file = fs.open(&rig.target, OpenFor::Read).unwrap();
        let e = fs.fsync(&mut file).unwrap_err();
        assert!(is_no_fsync(&e), "{side}: {e:#}");
        fs.close(file);
        // Up front, with the route.
        assert_eq!(flush_warning(&fs, "box").as_deref(), Some(NOTE), "{side}");
        assert_eq!(
            route(&mut fs, &rig.target, 300 * KIB, None),
            Ok(Route::ServerCopy)
        );
        // The Save lands, and its result says it.
        let saved = saved(go(&mut fs, &mut rig, Opts::default()));
        assert_eq!(saved.notes, [NOTE], "{side}");
        assert!(!fs.changes.contains(&"fsync"), "{side}: none was sent");
        let want = patched(body, &rig.runs);
        assert_eq!(std::fs::read(&rig.path).unwrap(), want, "{side}");

        // Write in place, and Save copy: the same note.
        let (mut rig, _) = two_runs_in_a_page(&mut fs);
        let in_place = Opts {
            in_place: true,
            ..Opts::default()
        };
        assert_eq!(
            saved_notes(go(&mut fs, &mut rig, in_place)),
            [NOTE],
            "{side}"
        );
        let (rig, _) = small(&mut fs);
        let CopyOutcome::Saved { notes, .. } = go_copy(&mut fs, &rig, None).unwrap() else {
            panic!("{side}: no copy");
        };
        assert_eq!(notes, [NOTE], "{side}");
    }
    // A server that can: no warning, no note, and the flush is sent.
    for (side, mut fs) in sides() {
        assert_eq!(flush_warning(&fs, "box"), None, "{side}");
        let (mut rig, _) = small(&mut fs);
        assert_eq!(saved_notes(go(&mut fs, &mut rig, Opts::default())), none());
        assert!(fs.changes.contains(&"fsync"), "{side}");
    }
}

fn saved_notes(outcome: Result<SaveOutcome>) -> Vec<String> {
    saved(outcome).notes
}

/// `Fresh` never mixes two files: with the path replaced between two reads, the attributes and
/// the bytes it gives afterwards are both the new file's.
#[test]
fn fresh_reads_follow_the_path_not_an_old_handle() {
    use crate::journal::Inspect;
    for (side, mut fs) in sides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let name = path.to_str().unwrap();
        std::fs::write(&path, b"AAAAAAAA the first file").unwrap();
        let mut fresh = Fresh::new(&mut fs);
        assert_eq!(fresh.read_at(name, 0, 8).unwrap(), b"AAAAAAAA", "{side}");
        // Another file takes the name (a rename over it, as a Save does).
        let other = dir.path().join("other");
        std::fs::write(&other, b"BBBBBBBB the second, longer file").unwrap();
        std::fs::rename(&other, &path).unwrap();
        let now = fresh.lstat(name).unwrap().unwrap();
        assert_eq!(now.size, 32, "{side}: the new file's attributes");
        assert_eq!(fresh.read_at(name, 0, 8).unwrap(), b"BBBBBBBB", "{side}");
        let sha = fresh.whole_sha256(name, now.size).unwrap();
        assert_eq!(
            sha,
            Some(sha_of(b"BBBBBBBB the second, longer file")),
            "{side}"
        );
        // And again without an `lstat` between.
        std::fs::write(&other, b"CCCCCCCC").unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert_eq!(fresh.read_at(name, 0, 8).unwrap(), b"CCCCCCCC", "{side}");
        assert_eq!(
            fresh.page_sha256(name, 0).unwrap(),
            Some(sha_of(b"CCCCCCCC"))
        );
    }
}

/// The local copy counts what it copied: a source that ends before the size asked is an error,
/// never a request reported whole.
#[test]
fn a_local_copy_of_a_source_that_ends_early_fails() {
    let mut fs = LocalFs::new();
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("src"), dir.path().join(".src.sopen-test"));
    std::fs::write(&src, patterned(100 * KIB)).unwrap();
    let mut from = fs.open(src.to_str().unwrap(), OpenFor::Read).unwrap();
    let mut to = fs.create_temp(dst.to_str().unwrap()).unwrap();
    let mut events = Vec::new();
    let mut clock = stopwatch();
    let mut ctl = CopyCtl {
        pace: Pace::steps(64 * KIB),
        clock: &mut clock,
        cancel: &|| false,
        progress: &mut |e| events.push(e),
    };
    let e = fs
        .copy(&mut from, &mut to, 200 * KIB, &mut ctl)
        .unwrap_err();
    let text = format!("{e:#}");
    assert!(
        text.contains("ended after 102400 of 204800 bytes"),
        "{text}"
    );
    let eof = e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::UnexpectedEof)
    });
    assert!(eof, "{text}");
    assert_eq!(
        events,
        [CopyEvent::Copied(64 * KIB)],
        "only what was copied"
    );
}

/// A Save copy whose link got no reply names the name the link was sent for: with
/// `model.edited.bin` taken, that is `model.edited-1.bin`.
#[test]
fn save_copy_with_a_lost_link_reply_names_the_name_it_tried() {
    for (side, mut fs) in sides() {
        let (rig, body) = small(&mut fs);
        let taken = rig.folder().join("model.edited.bin");
        std::fs::write(&taken, "theirs").unwrap();
        let e = go_copy(
            &mut fs,
            &rig,
            Some(&mut |step| match step {
                Step::Link => At::LoseReply,
                _ => At::Go,
            }),
        )
        .unwrap_err();
        assert!(is_outcome_unknown(&e), "{side}: {e:#}");
        let tried = rig.folder().join("model.edited-1.bin");
        let want = format!("the copy may be in place — check {}", tried.display());
        assert_eq!(e.to_string(), want, "{side}: {e:#}");
        assert_eq!(std::fs::read(&tried).unwrap(), patched(body, &rig.runs));
        assert_eq!(std::fs::read(&taken).unwrap(), b"theirs", "{side}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
    }
}

/// A cancel is a cancel wherever it comes: during the pass's last progress report, and of an
/// empty file, which has no chunk to stop at.
#[test]
fn a_cancelled_baseline_pass_never_succeeds() {
    for (side, mut fs) in sides() {
        let (rig, _) = small(&mut fs);
        let cancel = AtomicBool::new(false);
        let base = rig.snap.baseline;
        let got = baseline_pass(&mut fs, &rig.target, &base, &cancel, &mut |done, size| {
            cancel.store(done == size, Ordering::Relaxed)
        });
        let e = got.expect_err("cancelled during the last report");
        assert!(is_cancelled(&e), "{side}: {e:#}");

        let empty = rig.folder().join("empty");
        std::fs::write(&empty, b"").unwrap();
        let name = empty.to_str().unwrap();
        let stat = fs.lstat_opt(name).unwrap().unwrap();
        let cancel = AtomicBool::new(true);
        let before = fs.changes.len();
        let got = baseline_pass(&mut fs, name, &stat.fingerprint(), &cancel, &mut |_, _| {});
        let e = got.expect_err("a cancelled empty file");
        assert!(is_cancelled(&e), "{side}: {e:#}");
        assert_eq!(fs.changes.len(), before);
        // Not cancelled, an empty file passes.
        let never = AtomicBool::new(false);
        let pass = baseline_pass(&mut fs, name, &stat.fingerprint(), &never, &mut |_, _| {});
        assert_eq!(pass.unwrap().sha256, sha_of(b""), "{side}");
    }
}

// ---------------------------------------------------------------------------------------------
// The undo file's publication, and its source.

const STAGES: [UndoStage; 5] = [
    UndoStage::Reserve,
    UndoStage::Write,
    UndoStage::Sync,
    UndoStage::Rename,
    UndoStage::SyncDir,
];

/// The undo file's injected failures end with the test, pass or fail.
struct NoUndoFault;

impl Drop for NoUndoFault {
    fn drop(&mut self) {
        undo_fails_at(None);
    }
}

fn undo_runs(path: &Path) -> Vec<Patch> {
    UndoFile::parse(&std::fs::read_to_string(path).unwrap())
        .unwrap()
        .runs
}

/// A publication that failed is never taken for done: at every stage, of a first publication and
/// of an update to a file that already holds runs, the same call again writes the file — and
/// until it does, the undo file holds what it held.
#[test]
fn a_failed_undo_publication_is_retried_not_accepted() {
    let _guard = NoUndoFault;
    let opened = Fingerprint {
        size: 1 << 20,
        mtime: UNIX_EPOCH + Duration::from_secs(OLD),
        sha256: None,
    };
    let first = [run(0x10, b"\x01\x02")];
    let second = [run(0x11, b"\xEE\x03"), run(0x900, b"\x09")];
    let both = [run(0x10, b"\x01\x02\x03"), run(0x900, b"\x09")];
    for stage in STAGES {
        // The first publication fails.
        let cache = tempfile::tempdir().unwrap();
        let at = UNIX_EPOCH + Duration::from_secs(OLD);
        let mut undo = UndoFile::new(cache.path(), "box", "/d/f", opened, at);
        undo_fails_at(Some(stage));
        let e = undo.add(&first).unwrap_err();
        assert!(format!("{e:#}").contains("injected"), "{stage:?}: {e:#}");
        assert_eq!(undo.len(), 0, "{stage:?}: nothing is held");
        let listed = UndoFile::list(cache.path(), "box", "/d/f");
        if stage != UndoStage::SyncDir {
            assert_eq!(listed, Vec::<PathBuf>::new(), "{stage:?}: nothing to load");
        }
        // Still failing: still an error, never Ok.
        assert!(undo.add(&first).is_err(), "{stage:?}: the retry fails too");
        undo_fails_at(None);
        undo.add(&first).unwrap();
        assert_eq!(undo_runs(undo.path()), first, "{stage:?}");
        assert_eq!(undo.len(), 2, "{stage:?}");

        // An update to a file that already holds runs fails.
        undo_fails_at(Some(stage));
        if stage == UndoStage::Reserve {
            // The name is this session's already: nothing is reserved again.
            undo.add(&second).unwrap();
            assert_eq!(undo_runs(undo.path()), both, "{stage:?}");
            continue;
        }
        let e = undo.add(&second).unwrap_err();
        assert!(format!("{e:#}").contains("injected"), "{stage:?}: {e:#}");
        assert_eq!(undo.len(), 2, "{stage:?}: it holds what was published");
        let held = UndoFile::parse(&undo.text()).unwrap().runs;
        assert_eq!(held, first, "{stage:?}");
        let on_disk = undo_runs(undo.path());
        match stage {
            // Renamed into place, its folder not yet flushed: either may be there.
            UndoStage::SyncDir => assert!(on_disk == first || on_disk == both, "{on_disk:?}"),
            _ => assert_eq!(on_disk, first, "{stage:?}: the file is as it was"),
        }
        assert!(undo.add(&second).is_err(), "{stage:?}: the retry fails too");
        undo_fails_at(None);
        undo.add(&second).unwrap();
        assert_eq!(undo_runs(undo.path()), both, "{stage:?}");
        assert_eq!(undo.len(), 4, "{stage:?}");
        // Nothing new: nothing is written, whatever would fail.
        undo_fails_at(Some(UndoStage::Write));
        undo.add(&first).unwrap();
        undo_fails_at(None);
    }
}

/// A Save whose undo file cannot be published patches nothing, and the next Save does not take
/// the undo file for written.
#[test]
fn a_save_whose_undo_file_fails_patches_nothing_and_the_next_writes_it() {
    let _guard = NoUndoFault;
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        undo_fails_at(Some(UndoStage::Sync));
        let e = go(&mut fs, &mut rig, Opts::default()).unwrap_err();
        undo_fails_at(None);
        assert!(
            format!("{e:#}").contains("cannot write the undo file"),
            "{e:#}"
        );
        assert!(!fs.changes.contains(&"write"), "{side}: nothing patched");
        assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{side}");
        assert_eq!(temps(rig.folder()), none(), "{side}");
        assert_eq!(rig.phase(), "none", "{side}");
        // The same Save again: the undo file is written before the patch, and gives it back.
        let saved = saved(go(&mut fs, &mut rig, Opts::default()));
        assert_eq!(undo_runs(&saved.undo).len(), 2, "{side}");
        rig.write_undo_back();
        assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{side}");
    }
}

/// A file name is any bytes: one holding a newline, a carriage return, a backslash, or text that
/// reads as a header or as an undo line comes back exactly, and adds no byte to restore.
#[test]
fn the_undo_files_source_cannot_add_or_change_a_record() {
    let opened = Fingerprint {
        size: 4096,
        mtime: UNIX_EPOCH + Duration::from_secs(OLD),
        sha256: None,
    };
    let names = [
        "/d/plain.bin",
        "/d/new\nline",
        "/d/evil\n0x0: FF",
        "/d/evil\n0x0: FF\n# as opened: 1 bytes, modified never\n# source: other:/x",
        "/d/cr\rreturn\r\n0x1: AA",
        "/d/back\\slash\\n not a newline",
        "/d/trailing\\",
        "/d/# source: other:/etc/passwd",
        "\n",
    ];
    for (i, name) in names.into_iter().enumerate() {
        let cache = tempfile::tempdir().unwrap();
        let at = UNIX_EPOCH + Duration::from_secs(OLD + i as u64);
        let mut undo = UndoFile::new(cache.path(), "box", name, opened, at);
        let runs = [run(0x20, b"\x01\x02"), run(0x800, b"\x7F")];
        undo.add(&runs).unwrap();
        let text = std::fs::read_to_string(undo.path()).unwrap();
        let read = UndoFile::parse(&text).unwrap();
        assert_eq!(read.source, format!("box:{name}"), "{name:?}");
        assert_eq!(read.runs, runs, "{name:?}: exactly the bytes replaced");
        assert_eq!(read.size, 4096, "{name:?}");
        // Four header lines and one line per run, whatever the name holds.
        assert_eq!(text.lines().count(), 6, "{name:?}: {text}");
        assert!(!text.contains('\r'), "{name:?}");
    }
    assert!(UndoFile::parse("# source: a\\x\n# as opened: 1 bytes\n").is_err());
}

// ---------------------------------------------------------------------------------------------
// The undo file, and words.

/// One undo file per session, holding the file as opened: a later Save adds the offsets it
/// changes for the first time, and an offset already there keeps the byte recorded first.
#[test]
fn the_undo_file_keeps_the_first_byte_of_every_offset() {
    let cache = tempfile::tempdir().unwrap();
    let opened = Fingerprint {
        size: 1 << 20,
        mtime: UNIX_EPOCH + Duration::from_secs(OLD),
        sha256: None,
    };
    let at = UNIX_EPOCH + Duration::from_secs(OLD + 99);
    let mut undo = UndoFile::new(cache.path(), "box", "/data/model.bin", opened, at);
    let path = cache
        .path()
        .join(".backup/box/data/model.bin.20231114T221459Z.undo.txt");
    assert_eq!(undo.path(), path);
    assert!(!path.exists(), "nothing until the first Save");
    undo.add(&[run(0x10, b"\x01\x02"), run(0xFFFFF, b"\x09")])
        .unwrap();
    // The second Save changes 0x11 again (now holding the first Save's byte) and 0x12 anew.
    undo.add(&[run(0x11, b"\xEE\x03")]).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        "# sopen undo file: the bytes its saves replaced in box:/data/model.bin\n\
         # source: box:/data/model.bin\n\
         # as opened: 1048576 bytes, modified 2023-11-14T22:13:20Z\n\
         # one line per run of replaced bytes — offset: the bytes before the save (hex)\n\
         0x10: 010203\n\
         0xFFFFF: 09\n"
    );
    let read = UndoFile::parse(&text).unwrap();
    assert_eq!(read.source, "box:/data/model.bin");
    assert_eq!(read.size, 1 << 20);
    assert_eq!(
        read.runs,
        [run(0x10, b"\x01\x02\x03"), run(0xFFFFF, b"\x09")]
    );
    assert_eq!(undo.len(), 4);
    // Rewritten through a temp, which does not stay; owner-only.
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["model.bin.20231114T221459Z.undo.txt"]);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);

    // Another session at the same second never writes over it: it takes the next name.
    let mut second = UndoFile::new(cache.path(), "box", "/data/model.bin", opened, at);
    second.add(&[run(0, b"\x55")]).unwrap();
    assert_ne!(second.path(), path);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    let later = UNIX_EPOCH + Duration::from_secs(OLD + 3600);
    let mut third = UndoFile::new(cache.path(), "box", "/data/model.bin", opened, later);
    third.add(&[run(0, b"\x66")]).unwrap();
    // Newest first, for the recovery rows; another file's are not listed.
    let mut other = UndoFile::new(cache.path(), "box", "/data/model.bin.old", opened, later);
    other.add(&[run(0, b"\x66")]).unwrap();
    let listed = UndoFile::list(cache.path(), "box", "/data/model.bin");
    assert_eq!(listed, [third.path(), second.path(), path.as_path()]);
    assert!(UndoFile::parse("0x10: 0").is_err());
    assert!(
        UndoFile::parse("# source: a\n0x10: 01\n").is_err(),
        "no size"
    );
}

/// Across two Saves of one session on a real file: the undo file still gives the file as opened.
#[test]
fn two_saves_in_a_session_share_one_undo_file() {
    for (side, mut fs) in sides() {
        let (mut rig, body) = small(&mut fs);
        let first = saved(go(&mut fs, &mut rig, Opts::default()));
        rig.landed();
        // The window's next edit, against the next baseline and evidence.
        rig.runs = vec![run(0x1006, b"\x00\x00\x00\x00\x00")];
        let old = at(&rig.path, 0x1006, 5);
        rig.snap = Snapshot {
            baseline: first.next,
            seen: Seen::pages([(0x1000, first.seen[&0x1000])]),
            edits: format!("0x1006: 0000000000 was {}\n", hex(&old)),
            pastes: Vec::new(),
        };
        let journal = Journal::create(
            journal::disk(),
            rig.cache.path(),
            "box",
            &rig.target,
            Kind::Hex,
        );
        rig.journal = Some(journal.unwrap());
        let second = saved(go(&mut fs, &mut rig, Opts::default()));
        assert_eq!(second.undo, first.undo, "{side}");
        assert_eq!(
            rig.undo.len(),
            4 + 1 + 2,
            "{side}: 0x1005–0x100A and 0x40000"
        );
        rig.write_undo_back();
        assert_eq!(std::fs::read(&rig.path).unwrap(), body, "{side}");
    }
}

/// What a Save is told must hold before anything is sent.
#[test]
fn a_save_without_its_evidence_sends_nothing() {
    let mut fs = Probe::local();
    let check = |fs: &mut Probe, change: &dyn Fn(&mut Rig), want: &str| {
        let (mut rig, _) = small(fs);
        change(&mut rig);
        let e = go(fs, &mut rig, Opts::default()).unwrap_err();
        assert!(format!("{e:#}").contains(want), "{e:#}");
        assert_eq!(fs.changes, Vec::<&str>::new());
    };
    check(
        &mut fs,
        &|r| r.snap.seen = Seen::Pending,
        "evidence is known",
    );
    check(
        &mut fs,
        &|r| r.snap.baseline.sha256 = None,
        "not been checked for Save yet",
    );
    check(
        &mut fs,
        &|r| r.runs.push(run(0x48000, b"\x01")),
        "has no evidence",
    );
    check(&mut fs, &|r| r.runs.swap(0, 1), "out of order");
    check(&mut fs, &|r| r.runs.clear(), "nothing to save");
    check(
        &mut fs,
        &|r| r.runs[1] = run(300 * KIB - 1, b"\x01\x02"),
        "past the file's",
    );
}

#[test]
fn the_words() {
    let copying = Progress::Copying {
        done: 8_900_000_000,
        total: 24_000_000_000,
    };
    assert_eq!(
        copying.line("host"),
        "saving — copying on host 37 % (8.9 of 24.0 GB)"
    );
    assert_eq!(checking_line(37, 100), "checking the file for Save — 37 %");
    assert_eq!(about(13_333.0), "about 3 h 40 min");
    assert_eq!(about(7_200.0), "about 2 h");
    assert_eq!(about(700.0), "about 12 min");
    assert_eq!(about(3.2), "about 3 s");
    let runs = runs_of(&[(5, 1), (6, 2), (9, 3), (4096, 4)].into());
    assert_eq!(runs, [run(5, &[1, 2]), run(9, &[3]), run(4096, &[4])]);
    assert_eq!(
        pages_of(&[run(4094, &[0; 4]), run(3 * PAGE, &[0])]),
        [0, PAGE, 3 * PAGE].into()
    );
    // The numbers 07 and the journal fix.
    assert_eq!(HASH_CHECK_MAX, 256 * MIB);
    assert_eq!(ROOM_MARGIN, 1024 * MIB);
    assert_eq!(PAGE, 4096);
    assert_eq!(
        (Pace::FIRST, Pace::MIN, Pace::MAX, Pace::SYNC_BYTES),
        (64 * MIB, 16 * MIB, 1024 * MIB, 2048 * MIB)
    );
    assert_eq!(Pace::REQUEST, Duration::from_secs(1));
    assert_eq!(Pace::SYNC_AFTER, Duration::from_secs(10));
}
