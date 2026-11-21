//! 07 §12's scenarios for the save layer (E1, E3–E9, E6b, E6c, E19), each against the real
//! `sftp-server` ([`SftpRemote::local_server`]) and against [`LocalFs`]; every step stopped and
//! every reply lost; and X2's pacing rule on a scripted clock.

use super::*;
use crate::journal::{reconcile, Kind, Outcome, Phase, Temp};
use crate::remote::{is_cancelled, is_outcome_unknown, Entry, Like, LocalFs, SftpRemote, Uploaded};
use std::os::unix::fs::{FileExt, PermissionsExt};
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
    fn fsync(&mut self, f: &mut Handle) -> Result<()> {
        self.changes.push("fsync");
        self.inner.fsync(f)
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
    let mut out = vec![("LocalFs", Probe::new(LocalFs::new()))];
    if let Some(fs) = SftpRemote::local_server() {
        assert!(
            PatchFs::can_copy(&fs),
            "this sftp-server must offer copy-data"
        );
        out.push(("local_server()", Probe::new(fs)));
    }
    out
}

/// The same two without `copy-data`: a wrapper reporting it absent, and the real server told to
/// refuse it (`-P copy-data`), which then does not advertise it.
fn sides_without_copy() -> Vec<(&'static str, Probe)> {
    let mut local = Probe::new(LocalFs::new());
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

/// A sparse file one MiB over the content-check limit, with 4 bytes typed over one page.
fn big(fs: &mut Probe) -> Rig {
    let size = HASH_CHECK_MAX + MIB;
    let offset = 200 * MIB + 0xC35;
    let patch = vec![run(offset, b"\xDE\xAD\xBE\xEF")];
    let rig = Rig::new(fs, Body::Sparse(size), patch);
    assert_eq!(
        rig.snap.baseline.sha256, None,
        "no whole hash above 256 MiB"
    );
    rig
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
        assert_eq!(
            (fs.read, fs.written),
            (PAGE, 4),
            "{side}: the page check and the patch, nothing else"
        );
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
        let why = refusal(go(&mut fs, &mut rig, Opts::default()));
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
        assert_eq!(
            (fs.read, fs.written),
            (300 * KIB, 5),
            "{side}: the temp read back once, and the patch"
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
        assert_eq!((fs.read, fs.written), (0, 0), "{side}");
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
        assert_eq!((fs.read, fs.written), (0, 0), "{side}: before any transfer");
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
        assert_eq!(
            (fs.read, fs.written),
            (2 * PAGE, 3),
            "{side}: two pages, three bytes"
        );
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
        assert_eq!(
            (fs.read, fs.written),
            (MIB, MIB),
            "{side}: one pass, the file's size up"
        );
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

/// The streamed rewrite stopped, or its reply lost: the target is untouched either way; a lost
/// reply removes the Save's own temp, a stop leaves it for the reopen, kept and named.
#[test]
fn a_streamed_rewrite_stopped_or_unanswered_leaves_the_target_whole() {
    const REWRITE: [Step; 15] = [
        Step::CheckTarget,
        Step::Room,
        Step::OpenSource,
        Step::StatSource,
        Step::Create,
        Step::StatCreated,
        Step::SetOwner,
        Step::SetMode,
        Step::Rewrite,
        Step::Undo,
        Step::FullMode,
        Step::Fsync,
        Step::StatTemp,
        Step::Recheck,
        Step::Rename,
    ];
    for (side, mut fs) in sides_without_copy() {
        let (mut rig, _) = small(&mut fs);
        let mut seen = Vec::new();
        saved(hooked(&mut fs, &mut rig, &mut |step| {
            seen.push(step);
            At::Go
        }));
        assert_eq!(seen[..15], REWRITE, "{side}");
        assert_eq!(seen[15..], [Step::StatAfter], "{side}");

        for at in [At::Stop, At::LoseReply] {
            for step in REWRITE {
                let (mut rig, body) = small(&mut fs);
                let got = hooked(&mut fs, &mut rig, &mut |s| match s == step {
                    true => at,
                    false => At::Go,
                });
                let tag = format!("{side}, {at:?} at {step:?}");
                let advisory = matches!(step, Step::Room | Step::Undo);
                let lost_rename = at == At::LoseReply && step == Step::Rename;
                if at == At::LoseReply && advisory {
                    saved(got);
                    continue;
                }
                let e = got.unwrap_err();
                let now = std::fs::read(&rig.path).unwrap();
                if lost_rename {
                    assert!(is_outcome_unknown(&e), "{tag}: {e:#}");
                    assert_eq!(now, patched(body, &rig.runs), "{tag}");
                    continue;
                }
                assert_eq!(now, body, "{tag}: {e:#}");
                assert!(
                    !is_outcome_unknown(&e) || step == Step::Create,
                    "{tag}: {e:#}"
                );
                match at {
                    At::Stop => assert!(is_stopped(&e), "{tag}: {e:#}"),
                    _ if step == Step::Create => {}
                    _ => {
                        assert_eq!(temps(rig.folder()), none(), "{tag}");
                        assert_eq!(rig.phase(), "none", "{tag}");
                    }
                }
                // Before the pass has read and checked everything, no undo file is written.
                let after_pass = REWRITE.iter().position(|s| *s == step).unwrap() > 9;
                assert_eq!(rig.undo.path().exists(), after_pass, "{tag}");
            }
        }
    }
}

/// Without `posix-rename@openssh.com` Save is off, as today, before anything is sent.
#[test]
fn a_server_that_cannot_replace_is_refused() {
    let mut fs = Probe::new(LocalFs::new());
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
const STEPS: [Step; 21] = [
    Step::CheckTarget,
    Step::Room,
    Step::OpenSource,
    Step::StatSource,
    Step::Create,
    Step::StatCreated,
    Step::SetOwner,
    Step::SetMode,
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
            let e = hooked(&mut fs, &mut rig, &mut |step| match step == stop {
                true => At::Stop,
                false => At::Go,
            })
            .unwrap_err();
            let tag = format!("{side}, stopped before {stop:?}");
            assert!(is_stopped(&e), "{tag}: {e:#}");
            let landed = stop == Step::StatAfter;
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
                // As created: provably this Save's, removed on reopen.
                Step::SetOwner | Step::SetMode => {
                    assert!(matches!(temp, Temp::Remove(_)), "{tag}: {temp:?}");
                }
                // Staged and checked, the rename not sent: removed on reopen.
                Step::Rename => assert!(matches!(temp, Temp::Remove(_)), "{tag}: {temp:?}"),
                Step::StatAfter => assert!(matches!(temp, Temp::Gone(_)), "{tag}: {temp:?}"),
                // Between `created` and `staged` the temp no longer has the stat recorded at
                // its creation: it is kept and named, never removed.
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

/// The reply of each step lost: before the rename the target is untouched and the Save's own
/// temp is removed (a lost create's is named and kept, never removed); a lost rename reply is an
/// unknown outcome naming both paths, never "nothing was written".
#[test]
fn a_lost_reply_at_any_step_is_what_07_says() {
    for (side, mut fs) in sides() {
        for lost in STEPS {
            let (mut rig, body) = small(&mut fs);
            let got = hooked(&mut fs, &mut rig, &mut |step| match step == lost {
                true => At::LoseReply,
                false => At::Go,
            });
            let tag = format!("{side}, the reply of {lost:?} lost");
            let after = patched(body.clone(), &rig.runs);
            let now = std::fs::read(&rig.path).unwrap();
            let left = temps(rig.folder());
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
            let e = in_place_hooked(&mut fs, &mut rig, &mut |step| match step == stop {
                true => At::Stop,
                false => At::Go,
            })
            .unwrap_err();
            let tag = format!("{side}, stopped before {stop:?}");
            assert!(is_stopped(&e), "{tag}: {e:#}");
            let now = std::fs::read(&rig.path).unwrap();
            let n = IN_PLACE.iter().position(|s| *s == stop).unwrap();
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
    let mut fs = Probe::new(LocalFs::new());
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
